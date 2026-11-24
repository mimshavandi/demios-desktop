//! MCP client over stdio and Streamable HTTP (T-CON-01), built on `rmcp`.
//!
//! One [`McpClient`] per configured server. It connects lazily, reconnects when the server
//! process or session has gone away, and applies connect/request timeouts. It never retries a
//! *call* on its own: whether a call may be re-sent is the executor's decision
//! ([`dn_core::capability::CapabilitySpec::retry_safe`]).

use crate::config::{McpServerConfig, McpTransport, resolve_values};
use crate::error::{ToolError, ToolErrorKind};
use crate::secrets::SecretStore;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService, ServiceError, ServiceExt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::AsyncBufReadExt;
use tokio::sync::Mutex;

/// A tool as advertised by a server, decoupled from `rmcp`'s (non-exhaustive) model types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub hints: ToolHints,
}

/// MCP tool annotations. These are **hints from the server**, untrusted by definition; they
/// only seed the risk tier, and the user can override (see [`crate::capability`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolHints {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

/// Result of `tools/call`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpCallOutput {
    /// MCP content blocks as JSON (text, image, resource…).
    pub content: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    /// The tool itself reported failure (`isError: true`).
    #[serde(default)]
    pub is_error: bool,
}

impl McpCallOutput {
    /// Concatenated text blocks; handy for errors and for models that only take text.
    pub fn text(&self) -> String {
        self.content
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionStatus {
    Disconnected,
    Connected,
    /// Remote server wants OAuth (or the stored token was rejected).
    NeedsAuth,
    Failed {
        developer_message: String,
    },
}

/// Supplies an OAuth-authorised HTTP client for remote servers (implemented in [`crate::oauth`]).
pub trait HttpAuthProvider: Send + Sync {
    /// Returns a ready `AuthorizationManager`, or `AuthRequired` when the user must sign in.
    fn authorization_manager(
        &self,
        config: &McpServerConfig,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<rmcp::transport::auth::AuthorizationManager, ToolError>,
                > + Send
                + '_,
        >,
    >;
}

type Service = RunningService<RoleClient, ()>;

const STDERR_TAIL_LINES: usize = 40;
/// Lines / characters of stderr included in developer messages.
const STDERR_MESSAGE_LINES: usize = 10;
const STDERR_LINE_CHARS: usize = 300;

/// Environment variables a stdio child inherits from us; everything else is cleared so provider
/// keys and other app secrets in our environment never reach servers.
const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "TMPDIR",
    // Windows: needed by most runtimes (node/npx, python) to start at all.
    "SystemRoot",
    "windir",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "ComSpec",
    "PATHEXT",
    "TEMP",
    "TMP",
];

/// The inherited part of a stdio child's environment (see [`INHERITED_ENV`], plus `LC_*`).
pub fn inherited_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(k, _)| {
            k.to_str().is_some_and(|k| {
                INHERITED_ENV.iter().any(|a| a.eq_ignore_ascii_case(k)) || k.starts_with("LC_")
            })
        })
        .collect()
}

pub struct McpClient {
    config: McpServerConfig,
    secrets: Arc<dyn SecretStore>,
    auth: Option<Arc<dyn HttpAuthProvider>>,
    service: Mutex<Option<Service>>,
    status: StdMutex<ConnectionStatus>,
    stderr_tail: Arc<StdMutex<VecDeque<String>>>,
    /// Secret values resolved for this server, scrubbed from diagnostics.
    secret_values: StdMutex<Vec<String>>,
    connects: std::sync::atomic::AtomicU32,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server_id", &self.config.id)
            .field("status", &self.status())
            .finish()
    }
}

impl McpClient {
    pub fn new(config: McpServerConfig, secrets: Arc<dyn SecretStore>) -> Result<Self, ToolError> {
        config.validate()?;
        Ok(Self {
            config,
            secrets,
            auth: None,
            service: Mutex::new(None),
            status: StdMutex::new(ConnectionStatus::Disconnected),
            stderr_tail: Arc::new(StdMutex::new(VecDeque::new())),
            secret_values: StdMutex::new(Vec::new()),
            connects: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// Use OAuth for a Streamable HTTP server that has `oauth` settings.
    pub fn with_auth(mut self, auth: Arc<dyn HttpAuthProvider>) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn server_id(&self) -> &str {
        &self.config.id
    }

    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }

    pub fn status(&self) -> ConnectionStatus {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or(ConnectionStatus::Disconnected)
    }

    /// How many times a connection has been (re)established. Useful for diagnostics/tests.
    pub fn connect_count(&self) -> u32 {
        self.connects.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Last stderr lines of a stdio server (for developer messages; may contain paths).
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Short, secret-scrubbed stderr excerpt for developer messages.
    fn diagnostic_tail(&self) -> String {
        let secrets = self
            .secret_values
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        let tail = self.stderr_tail();
        let start = tail.len().saturating_sub(STDERR_MESSAGE_LINES);
        tail[start..]
            .iter()
            .map(|l| {
                let mut l = l.clone();
                for s in secrets.iter().filter(|s| !s.is_empty()) {
                    l = l.replace(s.as_str(), "[redacted]");
                }
                if l.chars().count() > STDERR_LINE_CHARS {
                    l = l.chars().take(STDERR_LINE_CHARS).collect::<String>() + "…";
                }
                l
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn remember_secrets(
        &self,
        refs: &std::collections::BTreeMap<String, crate::config::ValueRef>,
        resolved: &[(String, String)],
    ) {
        if let Ok(mut v) = self.secret_values.lock() {
            for (name, value) in resolved {
                if matches!(refs.get(name), Some(crate::config::ValueRef::Secret { .. })) {
                    v.push(value.clone());
                }
            }
        }
    }

    fn set_status(&self, s: ConnectionStatus) {
        if let Ok(mut g) = self.status.lock() {
            *g = s;
        }
    }

    fn err(&self, kind: ToolErrorKind, msg: impl Into<String>) -> ToolError {
        ToolError::new(format!("mcp.{}", self.config.id), kind, msg)
    }

    /// Connect if not connected (or if the previous connection died).
    pub async fn connect(&self) -> Result<(), ToolError> {
        let mut guard = self.service.lock().await;
        if let Some(s) = guard.as_ref()
            && !s.is_closed()
        {
            return Ok(());
        }
        if let Some(mut old) = guard.take() {
            let _ = old.close().await;
        }
        match tokio::time::timeout(self.config.connect_timeout(), self.open()).await {
            Ok(Ok(service)) => {
                *guard = Some(service);
                self.connects
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.set_status(ConnectionStatus::Connected);
                Ok(())
            }
            Ok(Err(e)) => {
                self.set_status(if e.kind == ToolErrorKind::AuthRequired {
                    ConnectionStatus::NeedsAuth
                } else {
                    ConnectionStatus::Failed {
                        developer_message: e.developer_message.clone(),
                    }
                });
                Err(e)
            }
            Err(_) => {
                let e = self.err(
                    ToolErrorKind::Timeout,
                    format!(
                        "connect/initialize did not finish within {:?}",
                        self.config.connect_timeout()
                    ),
                );
                self.set_status(ConnectionStatus::Failed {
                    developer_message: e.developer_message.clone(),
                });
                Err(e)
            }
        }
    }

    /// Drop the current connection (kills a stdio child) and connect again.
    pub async fn restart(&self) -> Result<(), ToolError> {
        self.shutdown().await;
        self.connect().await
    }

    pub async fn shutdown(&self) {
        let mut guard = self.service.lock().await;
        if let Some(mut s) = guard.take() {
            let _ = s.close().await;
        }
        self.set_status(ConnectionStatus::Disconnected);
    }

    async fn open(&self) -> Result<Service, ToolError> {
        match &self.config.transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let prefix = format!("mcp.{}", self.config.id);
                let resolved = resolve_values(&prefix, env, self.secrets.as_ref())?;
                self.remember_secrets(env, &resolved);
                let mut cmd = tokio::process::Command::new(command);
                cmd.env_clear()
                    .envs(inherited_env())
                    .args(args)
                    .envs(resolved);
                if let Some(cwd) = cwd {
                    cmd.current_dir(cwd);
                }
                let (transport, stderr) = rmcp::transport::TokioChildProcess::builder(cmd)
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| {
                        self.err(
                            ToolErrorKind::Connection,
                            format!("failed to spawn `{command}`: {e}"),
                        )
                    })?;
                if let Some(stderr) = stderr {
                    let tail = self.stderr_tail.clone();
                    tokio::spawn(async move {
                        let mut lines = tokio::io::BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            if let Ok(mut t) = tail.lock() {
                                if t.len() == STDERR_TAIL_LINES {
                                    t.pop_front();
                                }
                                t.push_back(line.chars().take(4 * STDERR_LINE_CHARS).collect());
                            }
                        }
                    });
                }
                ().serve(transport).await.map_err(|e| {
                    let tail = self.diagnostic_tail();
                    self.err(
                        ToolErrorKind::Connection,
                        format!("initialize failed: {e}; stderr: {tail}"),
                    )
                })
            }
            McpTransport::StreamableHttp {
                url,
                headers,
                oauth,
            } => {
                use rmcp::transport::StreamableHttpClientTransport;
                use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
                let prefix = format!("mcp.{}", self.config.id);
                let mut custom = HashMap::new();
                let resolved = resolve_values(&prefix, headers, self.secrets.as_ref())?;
                self.remember_secrets(headers, &resolved);
                for (name, value) in resolved {
                    let n = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                        .map_err(|e| self.err(ToolErrorKind::Config, format!("header: {e}")))?;
                    let mut v = reqwest::header::HeaderValue::from_str(&value)
                        .map_err(|e| self.err(ToolErrorKind::Config, format!("header: {e}")))?;
                    v.set_sensitive(true);
                    custom.insert(n, v);
                }
                let cfg = StreamableHttpClientTransportConfig::with_uri(url.as_str())
                    .custom_headers(custom);
                let http = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| self.err(ToolErrorKind::Config, e.to_string()))?;
                let result = if oauth.is_some() {
                    let auth = self.auth.as_ref().ok_or_else(|| {
                        self.err(
                            ToolErrorKind::Config,
                            "server has oauth settings but no auth provider was given",
                        )
                    })?;
                    let manager = auth.authorization_manager(&self.config).await?;
                    let client = rmcp::transport::auth::AuthClient::new(http, manager);
                    ().serve(StreamableHttpClientTransport::with_client(client, cfg))
                        .await
                } else {
                    ().serve(StreamableHttpClientTransport::with_client(http, cfg))
                        .await
                };
                result.map_err(|e| {
                    if e.is_authorization_required() {
                        self.err(
                            ToolErrorKind::AuthRequired,
                            format!("server requires authorization: {e}"),
                        )
                    } else {
                        self.err(ToolErrorKind::Connection, format!("initialize failed: {e}"))
                    }
                })
            }
        }
    }

    async fn peer(&self) -> Result<rmcp::Peer<RoleClient>, ToolError> {
        self.connect().await?;
        let guard = self.service.lock().await;
        guard
            .as_ref()
            .map(|s| s.peer().clone())
            .ok_or_else(|| self.err(ToolErrorKind::Connection, "not connected"))
    }

    fn map_service_error(&self, e: ServiceError) -> ToolError {
        if e.auth_challenge().is_some() {
            self.set_status(ConnectionStatus::NeedsAuth);
            return self.err(ToolErrorKind::AuthRequired, e.to_string());
        }
        match e {
            ServiceError::McpError(data) => self.err(
                ToolErrorKind::ToolFailed,
                format!("server error {}: {}", data.code.0, data.message),
            ),
            ServiceError::Timeout { .. } => self.err(ToolErrorKind::Timeout, e.to_string()),
            ServiceError::TransportClosed | ServiceError::TransportSend(_) => {
                self.set_status(ConnectionStatus::Disconnected);
                let tail = self.diagnostic_tail();
                self.err(
                    ToolErrorKind::Connection,
                    format!("{e}; stderr tail: {tail}"),
                )
            }
            other => self.err(ToolErrorKind::Connection, other.to_string()),
        }
    }

    async fn with_timeout<T>(
        &self,
        what: &str,
        fut: impl std::future::Future<Output = Result<T, ServiceError>>,
    ) -> Result<T, ToolError> {
        match tokio::time::timeout(self.config.request_timeout(), fut).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                let dead = matches!(
                    e,
                    ServiceError::TransportClosed | ServiceError::TransportSend(_)
                );
                let err = self.map_service_error(e);
                if dead {
                    // The process/session is gone even if rmcp hasn't noticed yet: drop it so
                    // the next request reconnects (restart handling).
                    if let Some(mut s) = self.service.lock().await.take() {
                        let _ = s.close().await;
                    }
                }
                Err(err)
            }
            Err(_) => Err(self.err(
                ToolErrorKind::Timeout,
                format!(
                    "{what} did not answer within {:?}",
                    self.config.request_timeout()
                ),
            )),
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, ToolError> {
        let peer = self.peer().await?;
        let tools = self
            .with_timeout("tools/list", peer.list_all_tools())
            .await?;
        tools
            .into_iter()
            .map(|t| {
                let v = serde_json::to_value(&t)
                    .map_err(|e| self.err(ToolErrorKind::ToolFailed, e.to_string()))?;
                Ok(tool_from_json(&v))
            })
            .collect()
    }

    /// Call a tool once. Does not retry.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpCallOutput, ToolError> {
        let peer = self.peer().await?;
        let args = match arguments {
            serde_json::Value::Object(m) => Some(m),
            serde_json::Value::Null => None,
            other => {
                return Err(self.err(
                    ToolErrorKind::InvalidInput,
                    format!("tool arguments must be a JSON object, got {other}"),
                ));
            }
        };
        let mut params = CallToolRequestParams::new(name.to_owned());
        params.arguments = args;
        let result = self
            .with_timeout("tools/call", peer.call_tool(params))
            .await
            .map_err(|e| {
                // The request was handed to the transport: a timeout or a dropped connection
                // doesn't tell us whether the server acted on it.
                if matches!(e.kind, ToolErrorKind::Timeout | ToolErrorKind::Connection) {
                    e.maybe_executed()
                } else {
                    e
                }
            })?;
        let v = serde_json::to_value(&result)
            .map_err(|e| self.err(ToolErrorKind::ToolFailed, e.to_string()))?;
        Ok(McpCallOutput {
            content: v
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Array(vec![])),
            structured_content: v.get("structuredContent").cloned(),
            is_error: v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false),
        })
    }
}

fn tool_from_json(v: &serde_json::Value) -> McpTool {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned);
    let ann = v.get("annotations");
    let hint = |k: &str| ann.and_then(|a| a.get(k)).and_then(|b| b.as_bool());
    McpTool {
        name: s("name").unwrap_or_default(),
        title: s("title").or_else(|| {
            ann.and_then(|a| a.get("title"))
                .and_then(|t| t.as_str())
                .map(str::to_owned)
        }),
        description: s("description").unwrap_or_default(),
        input_schema: v
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"})),
        output_schema: v.get("outputSchema").cloned(),
        hints: ToolHints {
            read_only: hint("readOnlyHint"),
            destructive: hint("destructiveHint"),
            idempotent: hint("idempotentHint"),
            open_world: hint("openWorldHint"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_annotations() {
        let t = tool_from_json(&serde_json::json!({
            "name": "read_file",
            "description": "Read",
            "inputSchema": {"type": "object", "required": ["path"]},
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }));
        assert_eq!(t.name, "read_file");
        assert_eq!(t.hints.read_only, Some(true));
        assert_eq!(t.hints.open_world, Some(false));
        assert_eq!(t.hints.destructive, None);
    }
}

//! MCP server configuration (T-CON-01). Configs are safe to persist and sync: they carry
//! **secret ids**, never secret values. Values are resolved through [`SecretStore`] at connect time.

use crate::error::{ToolError, ToolErrorKind};
use crate::secrets::SecretStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Stable id, `[a-z0-9_-]+`; becomes the middle segment of `mcp.<server_id>.<tool>`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub transport: McpTransport,
    /// Per-request timeout (list/call). Default 60 s.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    /// Connect + initialize timeout. Default 30 s.
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
}

fn default_request_timeout_ms() -> u64 {
    60_000
}
fn default_connect_timeout_ms() -> u64 {
    30_000
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// Spawn a local process and speak JSON-RPC over stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// Extra environment. The child does **not** inherit secrets from ours beyond the
        /// normal process environment; put credentials here as `secret` refs.
        #[serde(default)]
        env: BTreeMap<String, ValueRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// Streamable HTTP (MCP 2025-03-26+).
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, ValueRef>,
        /// When set, the server is authorised with OAuth (T-CON-04) and the access token is
        /// kept under `oauth.credential_secret_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth: Option<OAuthSettings>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OAuthSettings {
    /// Secret id under which the token set (JSON) is stored, e.g. `mcp.<server_id>.oauth`.
    pub credential_secret_id: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Pre-registered client id; when absent dynamic client registration is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// A config value: either plain (non-secret, e.g. `LOG_LEVEL=debug`) or a secret reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueRef {
    Plain { value: String },
    Secret { secret_id: String },
}

impl ValueRef {
    pub fn plain(v: impl Into<String>) -> Self {
        Self::Plain { value: v.into() }
    }
    pub fn secret(id: impl Into<String>) -> Self {
        Self::Secret {
            secret_id: id.into(),
        }
    }
}

/// Names that must never hold a plain value.
fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTHORIZATION",
        "COOKIE",
    ]
    .iter()
    .any(|s| n.contains(s))
}

impl McpServerConfig {
    pub fn stdio(id: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            transport: McpTransport::Stdio {
                command: command.into(),
                args,
                env: BTreeMap::new(),
                cwd: None,
            },
            request_timeout_ms: default_request_timeout_ms(),
            connect_timeout_ms: default_connect_timeout_ms(),
        }
    }

    pub fn http(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            transport: McpTransport::StreamableHttp {
                url: url.into(),
                headers: BTreeMap::new(),
                oauth: None,
            },
            request_timeout_ms: default_request_timeout_ms(),
            connect_timeout_ms: default_connect_timeout_ms(),
        }
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms)
    }

    /// Reject configs that would leak or mis-route: bad ids, inline secrets, non-HTTP(S) URLs,
    /// plain-HTTP to non-loopback hosts.
    pub fn validate(&self) -> Result<(), ToolError> {
        let err = |m: String| ToolError::new(format!("mcp.{}", self.id), ToolErrorKind::Config, m);
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(err(format!(
                "server id `{}` must match [a-z0-9_-]+",
                self.id
            )));
        }
        let check_values = |values: &BTreeMap<String, ValueRef>| -> Result<(), ToolError> {
            for (name, v) in values {
                if matches!(v, ValueRef::Plain { .. }) && looks_secret(name) {
                    return Err(err(format!(
                        "`{name}` looks like a credential; store it as a secret ref, not inline"
                    )));
                }
            }
            Ok(())
        };
        match &self.transport {
            McpTransport::Stdio { command, env, .. } => {
                if command.trim().is_empty() {
                    return Err(err("stdio command is empty".into()));
                }
                check_values(env)?;
            }
            McpTransport::StreamableHttp { url, headers, .. } => {
                check_values(headers)?;
                let parsed = reqwest::Url::parse(url).map_err(|e| err(format!("bad url: {e}")))?;
                match parsed.scheme() {
                    "https" => {}
                    "http" if is_loopback(&parsed) => {}
                    "http" => {
                        return Err(err(
                            "plain http is only allowed for loopback servers; use https".into(),
                        ));
                    }
                    s => return Err(err(format!("unsupported url scheme `{s}`"))),
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some(h) => {
            let h = h.trim_start_matches('[').trim_end_matches(']');
            h == "localhost"
                || h.ends_with(".localhost")
                || h.parse::<std::net::IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
        }
        None => false,
    }
}

/// Resolve a map of [`ValueRef`]s to concrete strings. Missing secrets are a config error.
pub(crate) fn resolve_values(
    capability_prefix: &str,
    values: &BTreeMap<String, ValueRef>,
    secrets: &dyn SecretStore,
) -> Result<Vec<(String, String)>, ToolError> {
    values
        .iter()
        .map(|(name, v)| {
            let value = match v {
                ValueRef::Plain { value } => value.clone(),
                ValueRef::Secret { secret_id } => secrets
                    .get(secret_id)
                    .map_err(|e| {
                        ToolError::new(capability_prefix, ToolErrorKind::Config, e.to_string())
                    })?
                    .ok_or_else(|| {
                        ToolError::new(
                            capability_prefix,
                            ToolErrorKind::Config,
                            format!("secret `{secret_id}` (for `{name}`) is not set"),
                        )
                    })?,
            };
            Ok((name.clone(), value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_inline_secret_env() {
        let mut c = McpServerConfig::stdio("gh", "gh-mcp", vec![]);
        if let McpTransport::Stdio { env, .. } = &mut c.transport {
            env.insert("GITHUB_TOKEN".into(), ValueRef::plain("ghp_x"));
        }
        let e = c.validate().unwrap_err();
        assert_eq!(e.kind, ToolErrorKind::Config);
        if let McpTransport::Stdio { env, .. } = &mut c.transport {
            env.insert("GITHUB_TOKEN".into(), ValueRef::secret("mcp.gh.token"));
        }
        c.validate().unwrap();
    }

    #[test]
    fn http_rules() {
        McpServerConfig::http("a", "https://example.com/mcp")
            .validate()
            .unwrap();
        McpServerConfig::http("a", "http://127.0.0.1:9/mcp")
            .validate()
            .unwrap();
        assert!(
            McpServerConfig::http("a", "http://example.com/mcp")
                .validate()
                .is_err()
        );
        assert!(
            McpServerConfig::http("Bad.Id", "https://x")
                .validate()
                .is_err()
        );
    }

    #[test]
    fn config_round_trips_without_values() {
        let mut c = McpServerConfig::http("api", "https://example.com/mcp");
        if let McpTransport::StreamableHttp { headers, .. } = &mut c.transport {
            headers.insert("Authorization".into(), ValueRef::secret("mcp.api.auth"));
        }
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("mcp.api.auth"));
        let back: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}

//! Tool executor: registry of MCP servers + capabilities, preflight, approval, ledger, retry.
//!
//! This is dn-tools' own API. When `dn-agent` publishes its `ToolExecutor` trait, the adapter is
//! a thin shim over [`Executor::tool_specs`] and [`Executor::execute`].

use crate::approval::{ApprovalGate, ApprovalPrompt, GateOutcome};
use crate::capability::{CapabilityOverride, McpCapability, preflight};
use crate::client::{McpCallOutput, McpClient};
use crate::error::{ToolError, ToolErrorKind};
use crate::ledger::{self, Claim};
use dn_core::CoreError;
use dn_core::Id;
use dn_core::capability::{Preflight, RiskTier};
use dn_core::chat::{ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// Per-call context from the agent loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub conversation_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub capability_id: String,
    pub output: McpCallOutput,
    /// True when the output came from the ledger (a succeeded run with the same key).
    pub replayed: bool,
    pub attempts: u32,
}

/// Retry policy for retry-safe capabilities (reads and idempotent writes).
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: std::time::Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff: std::time::Duration::from_millis(200),
        }
    }
}

pub struct Executor {
    clients: RwLock<HashMap<String, Arc<McpClient>>>,
    capabilities: RwLock<HashMap<String, McpCapability>>,
    /// LLM-safe name → capability id.
    llm_names: RwLock<HashMap<String, String>>,
    overrides: RwLock<HashMap<String, CapabilityOverride>>,
    gate: Arc<ApprovalGate>,
    ledger: Arc<Mutex<rusqlite::Connection>>,
    retry: RetryPolicy,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor").finish_non_exhaustive()
    }
}

impl Executor {
    /// `ledger` is a connection to the app database with [`ledger::migrations`] applied.
    pub fn new(gate: Arc<ApprovalGate>, ledger: Arc<Mutex<rusqlite::Connection>>) -> Self {
        Self {
            clients: RwLock::default(),
            capabilities: RwLock::default(),
            llm_names: RwLock::default(),
            overrides: RwLock::default(),
            gate,
            ledger,
            retry: RetryPolicy::default(),
        }
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn gate(&self) -> &ApprovalGate {
        &self.gate
    }

    pub fn client(&self, server_id: &str) -> Option<Arc<McpClient>> {
        self.clients.read().ok()?.get(server_id).cloned()
    }

    /// Set user overrides (risk, approval, idempotency field, disabled), keyed by capability id.
    /// Applied on the next [`Self::refresh`]; also re-applied to known capabilities now.
    pub fn set_overrides(&self, overrides: HashMap<String, CapabilityOverride>) {
        if let Ok(mut o) = self.overrides.write() {
            *o = overrides;
        }
        let servers: Vec<String> = self
            .clients
            .read()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default();
        let ov = self.overrides.read().map(|o| o.clone()).unwrap_or_default();
        if let Ok(mut caps) = self.capabilities.write() {
            for cap in caps.values_mut() {
                if servers.contains(&cap.server_id) {
                    let tool = crate::client::McpTool {
                        name: cap.tool_name.clone(),
                        title: cap.title.clone(),
                        description: cap.spec.description.clone(),
                        input_schema: cap.spec.input_schema.clone(),
                        output_schema: cap.spec.output_schema.clone(),
                        hints: cap.hints,
                    };
                    *cap = McpCapability::from_tool(&cap.server_id, &tool, ov.get(&cap.spec.id));
                }
            }
        }
    }

    /// Register a server and load its tools.
    pub async fn add_server(&self, client: McpClient) -> Result<Vec<McpCapability>, CoreError> {
        let id = client.server_id().to_owned();
        if let Ok(mut c) = self.clients.write() {
            c.insert(id.clone(), Arc::new(client));
        }
        self.refresh(&id).await
    }

    pub async fn remove_server(&self, server_id: &str) {
        let client = self
            .clients
            .write()
            .ok()
            .and_then(|mut c| c.remove(server_id));
        if let Some(c) = client {
            c.shutdown().await;
        }
        if let Ok(mut caps) = self.capabilities.write() {
            caps.retain(|_, c| c.server_id != server_id);
        }
        if let Ok(mut names) = self.llm_names.write() {
            let prefix = format!("mcp.{server_id}.");
            names.retain(|_, id| !id.starts_with(&prefix));
        }
    }

    /// Re-list a server's tools (after connect, restart or `tools/list_changed`).
    pub async fn refresh(&self, server_id: &str) -> Result<Vec<McpCapability>, CoreError> {
        let client = self.client(server_id).ok_or_else(|| {
            CoreError::from(ToolError::new(
                format!("mcp.{server_id}"),
                ToolErrorKind::NotFound,
                "unknown server",
            ))
        })?;
        let tools = client.list_tools().await?;
        let ov = self.overrides.read().map(|o| o.clone()).unwrap_or_default();
        let caps: Vec<McpCapability> = tools
            .iter()
            .map(|t| {
                let id = crate::capability::capability_id(server_id, &t.name);
                McpCapability::from_tool(server_id, t, ov.get(&id))
            })
            .collect();
        if let (Ok(mut all), Ok(mut names)) = (self.capabilities.write(), self.llm_names.write()) {
            all.retain(|_, c| c.server_id != server_id);
            let prefix = format!("mcp.{server_id}.");
            names.retain(|_, id| !id.starts_with(&prefix));
            for c in &caps {
                names.insert(c.llm_name(), c.spec.id.clone());
                all.insert(c.spec.id.clone(), c.clone());
            }
        }
        Ok(caps)
    }

    pub fn capabilities(&self) -> Vec<McpCapability> {
        let mut v: Vec<_> = self
            .capabilities
            .read()
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default();
        v.sort_by(|a, b| a.spec.id.cmp(&b.spec.id));
        v
    }

    /// Tool specs for the model, using LLM-safe names (`mcp__<server>__<tool>`).
    /// Disabled capabilities are left out.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.capabilities()
            .into_iter()
            .filter(|c| !c.disabled)
            .map(|c| ToolSpec {
                name: c.llm_name(),
                description: c.spec.description.clone(),
                input_schema: c.spec.input_schema.clone(),
            })
            .collect()
    }

    /// Find a capability by id (`mcp.fs.read_file`) or LLM name (`mcp__fs__read_file`).
    pub fn resolve(&self, name: &str) -> Option<McpCapability> {
        let caps = self.capabilities.read().ok()?;
        if let Some(c) = caps.get(name) {
            return Some(c.clone());
        }
        let id = self.llm_names.read().ok()?.get(name).cloned()?;
        caps.get(&id).cloned()
    }

    fn not_found(name: &str) -> ToolError {
        ToolError::new(
            name,
            ToolErrorKind::NotFound,
            format!("no capability `{name}`"),
        )
    }

    /// Preflight a call without running it.
    pub fn preflight(&self, ctx: &CallContext, call: &ToolCall) -> Result<Preflight, CoreError> {
        let cap = self
            .resolve(&call.name)
            .ok_or_else(|| Self::not_found(&call.name))?;
        let status = self
            .client(&cap.server_id)
            .map(|c| c.status())
            .unwrap_or(crate::client::ConnectionStatus::Disconnected);
        let ok = self
            .gate
            .is_satisfied(&ctx.conversation_id, &cap.spec.id, cap.approval);
        Ok(preflight(&cap, &call.arguments, &status, ok))
    }

    /// Idempotency key for a write: the capability's key field when set and present, otherwise
    /// the conversation + tool-call id (so re-executing the *same* model call replays).
    pub fn idempotency_key(cap: &McpCapability, ctx: &CallContext, call: &ToolCall) -> String {
        if let Some(field) = &cap.spec.idempotency_key_field
            && let Some(v) = call.arguments.get(field)
        {
            let v = v
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string());
            return format!("{}|field:{field}={v}", cap.spec.id);
        }
        format!("{}|call:{}:{}", cap.spec.id, ctx.conversation_id, call.id)
    }

    fn with_ledger<T>(
        &self,
        cap_id: &str,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, ledger::LedgerError>,
    ) -> Result<T, ToolError> {
        let conn = self
            .ledger
            .lock()
            .map_err(|_| ToolError::new(cap_id, ToolErrorKind::Ledger, "ledger lock poisoned"))?;
        f(&conn).map_err(|e| {
            let kind = if matches!(e, ledger::LedgerError::ArgumentsMismatch(_)) {
                ToolErrorKind::InvalidInput
            } else {
                ToolErrorKind::Ledger
            };
            ToolError::new(cap_id, kind, e.to_string())
        })
    }

    /// Run a tool call end to end: preflight → (ledger replay) → approval → claim → call
    /// (+ retry when retry-safe) → record.
    pub async fn execute(
        &self,
        ctx: &CallContext,
        call: &ToolCall,
    ) -> Result<ToolOutcome, CoreError> {
        Ok(self.execute_inner(ctx, call).await?)
    }

    async fn execute_inner(
        &self,
        ctx: &CallContext,
        call: &ToolCall,
    ) -> Result<ToolOutcome, ToolError> {
        let cap = self
            .resolve(&call.name)
            .ok_or_else(|| Self::not_found(&call.name))?;
        let id = cap.spec.id.clone();
        let client = self
            .client(&cap.server_id)
            .ok_or_else(|| ToolError::new(&id, ToolErrorKind::NotFound, "server removed"))?;

        let approved_already = self
            .gate
            .is_satisfied(&ctx.conversation_id, &id, cap.approval);
        match preflight(&cap, &call.arguments, &client.status(), approved_already) {
            Preflight::Ready | Preflight::ApprovalRequired => {}
            Preflight::Blocked { reason } => {
                return Err(ToolError::new(&id, ToolErrorKind::Blocked, reason));
            }
            Preflight::NeedsInput { missing_fields } => {
                return Err(ToolError::new(
                    &id,
                    ToolErrorKind::InvalidInput,
                    format!("missing required fields: {missing_fields:?}"),
                ));
            }
            Preflight::NeedsConnection { .. } => {
                // A failed server may have come back; try once before giving up.
                if let Err(e) = client.connect().await {
                    return Err(ToolError::new(&id, e.kind, e.developer_message));
                }
            }
            Preflight::ScopeMissing { scopes } => {
                return Err(ToolError::new(
                    &id,
                    ToolErrorKind::AuthRequired,
                    format!("missing scopes {scopes:?}"),
                ));
            }
        }

        let is_write = cap.spec.risk != RiskTier::Read;
        let key = is_write.then(|| Self::idempotency_key(&cap, ctx, call));

        // A succeeded write replays without asking again: no new side effect happens.
        if let Some(key) = &key {
            match self.with_ledger(&id, |c| ledger::get(c, key))? {
                Some(e) if e.status == ledger::RunStatus::Succeeded => {
                    if e.arguments != call.arguments {
                        return Err(ToolError::new(
                            &id,
                            ToolErrorKind::InvalidInput,
                            "idempotency key reused with different arguments",
                        ));
                    }
                    return Ok(ToolOutcome {
                        capability_id: id,
                        output: decode_output(&cap, e.output)?,
                        replayed: true,
                        attempts: e.attempts,
                    });
                }
                Some(e) if e.status == ledger::RunStatus::Running => {
                    return Err(ToolError::new(
                        &id,
                        ToolErrorKind::InFlight,
                        format!("key `{key}` is already running"),
                    ));
                }
                _ => {}
            }
        }

        let outcome = self
            .gate
            .check(ApprovalPrompt {
                request_id: Id::new(),
                conversation_id: ctx.conversation_id.clone(),
                capability_id: id.clone(),
                title: cap.title.clone().unwrap_or_else(|| cap.tool_name.clone()),
                description: cap.spec.description.clone(),
                risk: cap.spec.risk,
                policy: cap.approval,
                arguments: call.arguments.clone(),
            })
            .await;
        match outcome {
            GateOutcome::AutoAllowed | GateOutcome::Approved => {}
            GateOutcome::Denied => {
                return Err(ToolError::new(&id, ToolErrorKind::Denied, "user denied"));
            }
            GateOutcome::Unanswered => {
                return Err(ToolError::new(
                    &id,
                    ToolErrorKind::Denied,
                    "approval not answered",
                ));
            }
        }

        let attempt_base = match &key {
            Some(key) => {
                match self.with_ledger(&id, |c| {
                    ledger::claim(c, key, &id, &ctx.conversation_id, &call.arguments)
                })? {
                    Claim::Acquired { attempt } => attempt,
                    Claim::Replay { output } => {
                        return Ok(ToolOutcome {
                            capability_id: id,
                            output: decode_output(&cap, Some(output))?,
                            replayed: true,
                            attempts: 1,
                        });
                    }
                    Claim::InFlight => {
                        return Err(ToolError::new(
                            &id,
                            ToolErrorKind::InFlight,
                            format!("key `{key}` is already running"),
                        ));
                    }
                }
            }
            None => 1,
        };

        let max = if cap.spec.retry_safe() {
            self.retry.max_attempts.max(1)
        } else {
            1
        };
        let mut tries = 0;
        let result = loop {
            tries += 1;
            match client
                .call_tool(&cap.tool_name, call.arguments.clone())
                .await
            {
                Ok(out) => break Ok(out),
                Err(e) if e.retryable() && tries < max => {
                    tokio::time::sleep(self.retry.backoff * tries).await;
                    continue;
                }
                Err(e) => break Err(ToolError::new(&id, e.kind, e.developer_message)),
            }
        };

        let result = result.and_then(|out| {
            if out.is_error {
                Err(ToolError::new(
                    &id,
                    ToolErrorKind::ToolFailed,
                    format!("tool reported error: {}", out.text()),
                ))
            } else {
                Ok(out)
            }
        });

        if let Some(key) = &key {
            match &result {
                Ok(out) => {
                    let v = serde_json::to_value(out)
                        .map_err(|e| ToolError::new(&id, ToolErrorKind::Ledger, e.to_string()))?;
                    self.with_ledger(&id, |c| ledger::succeed(c, key, &v))?;
                }
                Err(e) => {
                    self.with_ledger(&id, |c| ledger::fail(c, key, &e.developer_message))?;
                }
            }
        }

        result.map(|output| ToolOutcome {
            capability_id: id,
            output,
            replayed: false,
            attempts: attempt_base + tries - 1,
        })
    }
}

fn decode_output(
    cap: &McpCapability,
    v: Option<serde_json::Value>,
) -> Result<McpCallOutput, ToolError> {
    serde_json::from_value(v.unwrap_or(serde_json::Value::Null)).map_err(|e| {
        ToolError::new(
            &cap.spec.id,
            ToolErrorKind::Ledger,
            format!("cached output unreadable: {e}"),
        )
    })
}

//! Tool executor: registry of MCP servers + capabilities, preflight, approval, ledger, retry.
//!
//! This is dn-tools' own API. The `dn_agent::ToolExecutor` adapter is a thin shim over
//! [`Executor::tool_specs`] and [`Executor::execute_with_cancel`].

use crate::approval::{ApprovalGate, ApprovalPrompt, GateOutcome};
use crate::capability::{CapabilityOverride, McpCapability, preflight, with_hash_suffix};
use crate::client::{McpCallOutput, McpClient};
use crate::error::{ToolError, ToolErrorKind};
use crate::ledger::{self, Claim};
use dn_core::CoreError;
use dn_core::Id;
use dn_core::capability::{Preflight, RiskTier};
use dn_core::chat::{ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// Per-call context from the agent loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    /// The chat this run belongs to. `None` = a one-off run with no persistent chat.
    pub conversation_id: Option<String>,
    /// Fresh id per agent run (UUIDv7). Model tool-call ids repeat across runs (`call_0`, ""),
    /// so ledger keys use `(run_id, call.id)`.
    pub run_id: Id,
}

impl CallContext {
    pub fn new(conversation_id: Option<String>, run_id: Id) -> Self {
        Self {
            conversation_id,
            run_id,
        }
    }

    /// Scope for "allow for this chat" grants and field-based idempotency keys: the
    /// conversation, or the run when there is none (so grants never leak across chats).
    pub fn scope(&self) -> String {
        match &self.conversation_id {
            Some(c) => format!("conv:{c}"),
            None => format!("run:{}", self.run_id),
        }
    }
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

/// Marks a claimed ledger row `unknown` if the execute future is dropped (or panics) between
/// claim and record, so the key doesn't stay `running` (→ in flight) until restart.
struct ClaimGuard {
    ledger: Arc<Mutex<rusqlite::Connection>>,
    key: String,
    armed: bool,
}

impl ClaimGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let conn = match self.ledger.lock() {
            Ok(c) => c,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = ledger::mark_unknown(&conn, &self.key, "dropped before the result was recorded");
    }
}

impl Executor {
    /// `ledger` is a dedicated connection to the app database (WAL mode) with
    /// [`ledger::migrations`] applied. Don't share the chat connection.
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

    fn trusts(&self, server_id: &str) -> bool {
        self.client(server_id)
            .is_some_and(|c| c.config().trusts_annotations())
    }

    /// Set user overrides (risk, approval, idempotency field, disabled), keyed by capability id.
    /// Re-applied to known capabilities now and on every [`Self::refresh`].
    pub fn set_overrides(&self, overrides: HashMap<String, CapabilityOverride>) {
        if let Ok(mut o) = self.overrides.write() {
            *o = overrides;
        }
        let ov = self.overrides.read().map(|o| o.clone()).unwrap_or_default();
        let trust: HashMap<String, bool> = self
            .clients
            .read()
            .map(|c| {
                c.iter()
                    .map(|(id, c)| (id.clone(), c.config().trusts_annotations()))
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(mut caps) = self.capabilities.write() {
            for cap in caps.values_mut() {
                if let Some(trusted) = trust.get(&cap.server_id) {
                    let tool = crate::client::McpTool {
                        name: cap.tool_name.clone(),
                        title: cap.title.clone(),
                        description: cap.spec.description.clone(),
                        input_schema: cap.spec.input_schema.clone(),
                        output_schema: cap.spec.output_schema.clone(),
                        hints: cap.hints,
                    };
                    *cap = McpCapability::from_tool(
                        &cap.server_id,
                        &tool,
                        ov.get(&cap.spec.id),
                        *trusted,
                    );
                }
            }
        }
        self.reindex_names();
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
        self.reindex_names();
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
        let trusted = self.trusts(server_id);
        let ov = self.overrides.read().map(|o| o.clone()).unwrap_or_default();
        if let Ok(mut all) = self.capabilities.write() {
            all.retain(|_, c| c.server_id != server_id);
            for t in &tools {
                let id = crate::capability::capability_id(server_id, &t.name);
                let cap = McpCapability::from_tool(server_id, t, ov.get(&id), trusted);
                all.insert(id, cap);
            }
        }
        self.reindex_names();
        Ok(self
            .capabilities()
            .into_iter()
            .filter(|c| c.server_id == server_id)
            .collect())
    }

    /// Assign unique LLM names across all servers. Capabilities are visited in id order, so the
    /// result doesn't depend on registration order; a name already taken gets a hash suffix of
    /// the capability id.
    fn reindex_names(&self) {
        let (Ok(mut all), Ok(mut names)) = (self.capabilities.write(), self.llm_names.write())
        else {
            return;
        };
        let mut ids: Vec<String> = all.keys().cloned().collect();
        ids.sort();
        let mut taken: HashSet<String> = HashSet::new();
        names.clear();
        for id in ids {
            let Some(cap) = all.get_mut(&id) else {
                continue;
            };
            let mut name = crate::capability::llm_tool_name(&cap.server_id, &cap.tool_name);
            let mut n = 0u32;
            while taken.contains(&name) {
                n += 1;
                name = with_hash_suffix(&name, &format!("{id}#{n}"));
            }
            taken.insert(name.clone());
            names.insert(name.clone(), id.clone());
            cap.llm_name = name;
        }
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

    /// Tool specs for the model, using unique LLM-safe names (`mcp__<server>__<tool>`).
    /// Disabled capabilities are left out.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.capabilities()
            .into_iter()
            .filter(|c| !c.disabled)
            .map(|c| ToolSpec {
                name: c.llm_name.clone(),
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
            .is_satisfied(&ctx.scope(), &cap.spec.id, cap.approval);
        Ok(preflight(&cap, &call.arguments, &status, ok))
    }

    /// Ledger key for a write.
    ///
    /// - With [`CapabilitySpec::idempotency_key_field`](dn_core::capability::CapabilitySpec)
    ///   set and present: `(capability, scope, field value)`. The field must hold a
    ///   **per-request unique token** (e.g. a client request id), not a resource id such as a
    ///   path: the same token replays the first result, and reusing it with different
    ///   arguments is rejected. Auto-retry of such writes relies on the *server* deduplicating
    ///   by that token.
    /// - Otherwise `(capability, run_id, tool-call id)`: re-executing the same model call in
    ///   the same run replays; a new run never does.
    pub fn idempotency_key(cap: &McpCapability, ctx: &CallContext, call: &ToolCall) -> String {
        if let Some(field) = &cap.spec.idempotency_key_field
            && let Some(v) = call.arguments.get(field)
        {
            let v = v
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string());
            return format!("{}|{}|field:{field}={v}", cap.spec.id, ctx.scope());
        }
        format!("{}|run:{}|call:{}", cap.spec.id, ctx.run_id, call.id)
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
        self.execute_with_cancel(ctx, call, std::future::pending())
            .await
    }

    /// Like [`Self::execute`], but stops when `cancel` resolves: the approval wait and the
    /// tool call are abandoned and the result is [`CoreError::Cancelled`]. A write cancelled
    /// mid-call is recorded as `unknown` in the ledger (its side effect may have happened).
    pub async fn execute_with_cancel(
        &self,
        ctx: &CallContext,
        call: &ToolCall,
        cancel: impl std::future::Future<Output = ()> + Send,
    ) -> Result<ToolOutcome, CoreError> {
        let cancel = std::pin::pin!(cancel);
        Ok(self.execute_inner(ctx, call, cancel).await?)
    }

    async fn execute_inner(
        &self,
        ctx: &CallContext,
        call: &ToolCall,
        mut cancel: std::pin::Pin<&mut (impl std::future::Future<Output = ()> + Send)>,
    ) -> Result<ToolOutcome, ToolError> {
        let cap = self
            .resolve(&call.name)
            .ok_or_else(|| Self::not_found(&call.name))?;
        let id = cap.spec.id.clone();
        let scope = ctx.scope();
        let client = self
            .client(&cap.server_id)
            .ok_or_else(|| ToolError::new(&id, ToolErrorKind::NotFound, "server removed"))?;

        let approved_already = self.gate.is_satisfied(&scope, &id, cap.approval);
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
                client.connect().await.map_err(|e| e.for_capability(&id))?;
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
        if let Some(key) = &key
            && let Some(e) = self.with_ledger(&id, |c| ledger::get(c, key))?
        {
            if e.arguments != call.arguments
                && matches!(
                    e.status,
                    ledger::RunStatus::Succeeded | ledger::RunStatus::Unknown
                )
            {
                return Err(ToolError::new(
                    &id,
                    ToolErrorKind::InvalidInput,
                    "idempotency key reused with different arguments",
                ));
            }
            match e.status {
                ledger::RunStatus::Succeeded => {
                    return Ok(ToolOutcome {
                        capability_id: id,
                        output: decode_output(&cap, e.output)?,
                        replayed: true,
                        attempts: e.attempts,
                    });
                }
                ledger::RunStatus::Running => {
                    return Err(ToolError::new(
                        &id,
                        ToolErrorKind::InFlight,
                        format!("key `{key}` is already running"),
                    ));
                }
                ledger::RunStatus::Unknown => {
                    return Err(ToolError::new(
                        &id,
                        ToolErrorKind::OutcomeUnknown,
                        format!(
                            "key `{key}` has an unknown outcome ({}); resolve it first",
                            e.error.unwrap_or_default()
                        ),
                    ));
                }
                ledger::RunStatus::Failed => {}
            }
        }

        let approval = self.gate.check(ApprovalPrompt {
            request_id: Id::new(),
            conversation_id: scope.clone(),
            capability_id: id.clone(),
            title: cap.title.clone().unwrap_or_else(|| cap.tool_name.clone()),
            description: cap.spec.description.clone(),
            risk: cap.spec.risk,
            policy: cap.approval,
            arguments: call.arguments.clone(),
        });
        let outcome = tokio::select! {
            o = approval => o,
            () = cancel.as_mut() => {
                return Err(ToolError::new(
                    &id,
                    ToolErrorKind::Cancelled,
                    "cancelled while waiting for approval",
                ));
            }
        };
        match outcome {
            GateOutcome::AutoAllowed | GateOutcome::Approved => {}
            GateOutcome::Denied => {
                return Err(ToolError::new(&id, ToolErrorKind::Denied, "user denied"));
            }
            GateOutcome::Unanswered => {
                return Err(ToolError::new(
                    &id,
                    ToolErrorKind::Denied,
                    "approval not answered (timed out or UI gone)",
                ));
            }
        }

        let mut guard = None;
        let attempt_base = match &key {
            Some(key) => {
                match self
                    .with_ledger(&id, |c| ledger::claim(c, key, &id, &scope, &call.arguments))?
                {
                    Claim::Acquired { attempt } => {
                        guard = Some(ClaimGuard {
                            ledger: self.ledger.clone(),
                            key: key.clone(),
                            armed: true,
                        });
                        attempt
                    }
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
                    Claim::OutcomeUnknown { error } => {
                        return Err(ToolError::new(
                            &id,
                            ToolErrorKind::OutcomeUnknown,
                            format!(
                                "key `{key}` has an unknown outcome ({}); resolve it first",
                                error.unwrap_or_default()
                            ),
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
        let attempts = async {
            loop {
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
                    Err(e) => break Err(e.for_capability(&id)),
                }
            }
        };
        let result = tokio::select! {
            r = attempts => r,
            () = cancel.as_mut() => Err(ToolError::new(
                &id,
                ToolErrorKind::Cancelled,
                "cancelled during the tool call; outcome unknown",
            ).maybe_executed()),
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
                Err(e) if e.maybe_executed => {
                    self.with_ledger(&id, |c| ledger::mark_unknown(c, key, &e.developer_message))?;
                }
                Err(e) => {
                    self.with_ledger(&id, |c| ledger::fail(c, key, &e.developer_message))?;
                }
            }
            if let Some(g) = guard.as_mut() {
                g.disarm();
            }
        }

        result.map(|output| ToolOutcome {
            capability_id: id,
            output,
            replayed: false,
            attempts: attempt_base + tries - 1,
        })
    }

    /// Resolve a write whose outcome is unknown (UI: "did this go through?").
    pub fn resolve_unknown(
        &self,
        key: &str,
        resolution: &ledger::Resolution,
    ) -> Result<(), CoreError> {
        Ok(self.with_ledger(key, |c| ledger::resolve(c, key, resolution))?)
    }

    /// Writes waiting for the user to resolve.
    pub fn unknown_outcomes(&self) -> Result<Vec<ledger::LedgerEntry>, CoreError> {
        Ok(self.with_ledger("ledger", ledger::list_unknown)?)
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

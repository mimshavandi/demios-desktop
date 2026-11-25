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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

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
    diagnostics: Mutex<std::collections::VecDeque<ToolDiagnostic>>,
    /// Set for good when a lock is found poisoned (#145): the maps behind it, and with them
    /// the user's per-tool choices, can't be trusted, so every tool is refused until
    /// restart. Never read as "no overrides" or "no tools" and carry on.
    unavailable: AtomicBool,
}

/// Developer detail of a failed call, kept locally (not sent to the model).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDiagnostic {
    pub capability_id: String,
    pub kind: String,
    /// Capped at [`DIAGNOSTIC_CHARS`]; stdio stderr is already scrubbed of resolved secrets.
    pub developer_message: String,
}

const DIAGNOSTICS_KEPT: usize = 100;
pub const DIAGNOSTIC_CHARS: usize = 2000;

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
            diagnostics: Mutex::default(),
            unavailable: AtomicBool::new(false),
        }
    }

    /// Whether tools are refused after an internal error (a poisoned lock).
    pub fn is_unavailable(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }

    fn unavailable_error(&self, id: &str) -> ToolError {
        ToolError::new(id, ToolErrorKind::Unavailable, "executor lock poisoned")
    }

    /// Latches [`Self::is_unavailable`] and logs once.
    fn mark_unavailable(&self) {
        if !self.unavailable.swap(true, Ordering::SeqCst) {
            eprintln!("dn-tools: a lock was poisoned; every tool is refused until restart");
        }
    }

    /// Fails closed once unavailable.
    fn check_available(&self, id: &str) -> Result<(), ToolError> {
        if self.is_unavailable() {
            Err(self.unavailable_error(id))
        } else {
            Ok(())
        }
    }

    fn rd<'a, T>(&self, lock: &'a RwLock<T>) -> Option<RwLockReadGuard<'a, T>> {
        match lock.read() {
            Ok(g) => Some(g),
            Err(_) => {
                self.mark_unavailable();
                None
            }
        }
    }

    fn wr<'a, T>(&self, lock: &'a RwLock<T>) -> Option<RwLockWriteGuard<'a, T>> {
        match lock.write() {
            Ok(g) => Some(g),
            Err(_) => {
                self.mark_unavailable();
                None
            }
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
        self.rd(&self.clients)?.get(server_id).cloned()
    }

    fn trusts(&self, server_id: &str) -> bool {
        self.client(server_id)
            .is_some_and(|c| c.config().trusts_annotations())
    }

    /// Set user overrides (risk, approval, idempotency field, disabled), keyed by capability id.
    /// Re-applied to known capabilities now and on every [`Self::refresh`].
    pub fn set_overrides(&self, overrides: HashMap<String, CapabilityOverride>) {
        // On a poisoned lock nothing is rebuilt: capabilities are never re-derived without
        // the user's overrides. The executor refuses every tool instead.
        let Some(mut o) = self.wr(&self.overrides) else {
            return;
        };
        *o = overrides;
        let ov = o.clone();
        drop(o);
        let Some(trust) = self.rd(&self.clients).map(|c| {
            c.iter()
                .map(|(id, c)| (id.clone(), c.config().trusts_annotations()))
                .collect::<HashMap<String, bool>>()
        }) else {
            return;
        };
        if let Some(mut caps) = self.wr(&self.capabilities) {
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
        self.check_available(&format!("mcp.{id}"))?;
        match self.wr(&self.clients) {
            Some(mut c) => {
                c.insert(id.clone(), Arc::new(client));
            }
            None => return Err(self.unavailable_error(&format!("mcp.{id}")).into()),
        }
        self.refresh(&id).await
    }

    pub async fn remove_server(&self, server_id: &str) {
        // Removing and stopping must work even after a poisoned lock (the user is turning a
        // server off), so these recover the data; the executor stays unavailable.
        let client = self.recovering_write(&self.clients).remove(server_id);
        if let Some(c) = client {
            c.shutdown().await;
        }
        self.recovering_write(&self.capabilities)
            .retain(|_, c| c.server_id != server_id);
        self.reindex_names();
    }

    /// Removes every server and stops it ("Delete all local data"): no tool can run, connect
    /// or refresh a token afterwards. Concurrent, each bounded by `per_server`; returns how
    /// many did not stop in time (they are removed either way).
    pub async fn remove_all_servers(&self, per_server: std::time::Duration) -> usize {
        let clients: Vec<Arc<McpClient>> = self
            .recovering_write(&self.clients)
            .drain()
            .map(|(_, v)| v)
            .collect();
        self.recovering_write(&self.capabilities).clear();
        self.reindex_names();
        shutdown_concurrently(
            clients
                .into_iter()
                .map(|c| async move { c.shutdown().await })
                .collect(),
            per_server,
        )
        .await
    }

    /// Stops every server (app exit) concurrently, each bounded by `per_server`, so one
    /// stuck server can't keep the others (and their process trees) alive. Returns how
    /// many servers did not stop in time.
    pub async fn shutdown_all(&self, per_server: std::time::Duration) -> usize {
        let clients: Vec<Arc<McpClient>> = match self.clients.read() {
            Ok(c) => c.values().cloned().collect(),
            Err(p) => {
                self.mark_unavailable();
                p.into_inner().values().cloned().collect()
            }
        };
        shutdown_concurrently(
            clients
                .into_iter()
                .map(|c| async move { c.shutdown().await })
                .collect(),
            per_server,
        )
        .await
    }

    /// Re-list a server's tools (after connect, restart or `tools/list_changed`).
    pub async fn refresh(&self, server_id: &str) -> Result<Vec<McpCapability>, CoreError> {
        self.check_available(&format!("mcp.{server_id}"))?;
        let client = self.client(server_id).ok_or_else(|| {
            CoreError::from(ToolError::new(
                format!("mcp.{server_id}"),
                ToolErrorKind::NotFound,
                "unknown server",
            ))
        })?;
        let tools = client.list_tools().await?;
        let trusted = self.trusts(server_id);
        // Never rebuild a server's tools without the user's overrides.
        let unavailable = || CoreError::from(self.unavailable_error(&format!("mcp.{server_id}")));
        let ov = self.rd(&self.overrides).ok_or_else(unavailable)?.clone();
        {
            let mut all = self.wr(&self.capabilities).ok_or_else(unavailable)?;
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

    /// Assign unique LLM names across all servers. Capabilities that already had a name keep it
    /// (a tool is never renamed mid-conversation); newcomers are visited in id order and get a
    /// hash suffix of their capability id when their name is taken.
    fn reindex_names(&self) {
        let (Some(mut all), Some(mut names)) =
            (self.wr(&self.capabilities), self.wr(&self.llm_names))
        else {
            return;
        };
        let previous: HashMap<String, String> =
            names.drain().map(|(name, id)| (id, name)).collect();
        let mut ids: Vec<String> = all.keys().cloned().collect();
        ids.sort();
        let mut taken: HashSet<String> = HashSet::new();
        let mut newcomers = Vec::new();
        for id in ids {
            match previous.get(&id) {
                Some(name) if all.contains_key(&id) => {
                    taken.insert(name.clone());
                    names.insert(name.clone(), id.clone());
                    if let Some(cap) = all.get_mut(&id) {
                        cap.llm_name = name.clone();
                    }
                }
                _ => newcomers.push(id),
            }
        }
        for id in newcomers {
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

    /// Every capability, sorted by id. Empty once [`Self::is_unavailable`]: no tool is
    /// offered, and every call is refused with a clear error.
    pub fn capabilities(&self) -> Vec<McpCapability> {
        if self.is_unavailable() {
            return vec![];
        }
        let Some(caps) = self.rd(&self.capabilities) else {
            return vec![];
        };
        let mut v: Vec<_> = caps.values().cloned().collect();
        drop(caps);
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
        if self.is_unavailable() {
            return None;
        }
        let caps = self.rd(&self.capabilities)?;
        if let Some(c) = caps.get(name) {
            return Some(c.clone());
        }
        let id = self.rd(&self.llm_names)?.get(name).cloned()?;
        caps.get(&id).cloned()
    }

    /// A write guard even on a poisoned lock (removing or stopping servers must always
    /// work). Marks the executor unavailable.
    fn recovering_write<'a, T>(&self, lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
        lock.write().unwrap_or_else(|p| {
            self.mark_unavailable();
            p.into_inner()
        })
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
        self.check_available(&call.name)?;
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
        Ok(self.execute_tool_error(ctx, call, cancel).await?)
    }

    /// [`Self::execute_with_cancel`] keeping the internal error kind (for the agent adapter).
    pub(crate) async fn execute_tool_error(
        &self,
        ctx: &CallContext,
        call: &ToolCall,
        cancel: impl std::future::Future<Output = ()> + Send,
    ) -> Result<ToolOutcome, ToolError> {
        let cancel = std::pin::pin!(cancel);
        let result = self.execute_inner(ctx, call, cancel).await;
        if let Err(e) = &result {
            self.record_diagnostic(e);
        }
        result
    }

    fn record_diagnostic(&self, e: &ToolError) {
        if let Ok(mut d) = self.diagnostics.lock() {
            if d.len() == DIAGNOSTICS_KEPT {
                d.pop_front();
            }
            d.push_back(ToolDiagnostic {
                capability_id: e.capability_id.clone(),
                kind: format!("{:?}", e.kind),
                developer_message: e.developer_message.chars().take(DIAGNOSTIC_CHARS).collect(),
            });
        }
    }

    /// Developer detail of recent failures, newest last (for a local diagnostics view/log).
    pub fn recent_diagnostics(&self) -> Vec<ToolDiagnostic> {
        self.diagnostics
            .lock()
            .map(|d| d.iter().cloned().collect())
            .unwrap_or_default()
    }

    async fn execute_inner(
        &self,
        ctx: &CallContext,
        call: &ToolCall,
        mut cancel: std::pin::Pin<&mut (impl std::future::Future<Output = ()> + Send)>,
    ) -> Result<ToolOutcome, ToolError> {
        self.check_available(&call.name)?;
        let cap = self
            .resolve(&call.name)
            .ok_or_else(|| Self::not_found(&call.name))?;
        // Resolved, then the lock poisoned before use: still refuse.
        self.check_available(&call.name)?;
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
                let msg = format!("missing required fields: {}", missing_fields.join(", "));
                return Err(
                    ToolError::new(&id, ToolErrorKind::InvalidInput, &msg).with_model_message(msg)
                );
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

        // An earlier call to this write in this scope has an unknown outcome: refuse new calls
        // (they would get a fresh key and could repeat the side effect, even under an
        // "allow for this chat" grant) until the user confirms what happened.
        if is_write
            && let Some(pending) =
                self.with_ledger(&id, |c| ledger::unknown_in_scope(c, &id, &scope))?
        {
            return Err(ToolError::new(
                &id,
                ToolErrorKind::OutcomeUnknown,
                format!("capability has an unresolved unknown outcome (key `{pending}`)"),
            )
            .with_model_message(
                "An earlier call of this tool may or may not have taken effect. Don't retry; \
                 ask the user to check and confirm what happened first.",
            ));
        }

        let approval = self.gate.check(ApprovalPrompt {
            request_id: Id::new(),
            scope: scope.clone(),
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
                )
                .with_model_message(out.text()))
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

    /// Drops the finished ledger rows of a purged conversation; unknown ones stay until
    /// resolved (T-STORE-11).
    pub fn forget_conversation(&self, conversation_id: &str) -> Result<usize, CoreError> {
        let scope = format!("conv:{conversation_id}");
        Ok(self.with_ledger("ledger", |c| ledger::delete_scope(c, &scope))?)
    }

    /// The ledger row for `key`, if any.
    pub fn ledger_entry(&self, key: &str) -> Result<Option<ledger::LedgerEntry>, CoreError> {
        Ok(self.with_ledger(key, |c| ledger::get(c, key))?)
    }

    /// Deletes one finished ledger row (after resolving a call whose conversation is gone).
    pub fn forget_call(&self, key: &str) -> Result<bool, CoreError> {
        Ok(self.with_ledger(key, |c| ledger::delete_finished(c, key))?)
    }

    /// Deletes the whole ledger ("Delete all local data").
    pub fn forget_all(&self) -> Result<usize, CoreError> {
        Ok(self.with_ledger("ledger", ledger::delete_all)?)
    }

    /// Writes waiting for the user to resolve.
    pub fn unknown_outcomes(&self) -> Result<Vec<ledger::LedgerEntry>, CoreError> {
        Ok(self.with_ledger("ledger", ledger::list_unknown)?)
    }
}

/// Runs every shutdown at once, each under its own timeout; returns how many timed out.
pub(crate) async fn shutdown_concurrently<F>(
    shutdowns: Vec<F>,
    per_item: std::time::Duration,
) -> usize
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let mut set = tokio::task::JoinSet::new();
    for f in shutdowns {
        set.spawn(async move { tokio::time::timeout(per_item, f).await.is_err() });
    }
    let mut timed_out = 0;
    while let Some(r) = set.join_next().await {
        if r.unwrap_or(true) {
            timed_out += 1;
        }
    }
    timed_out
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

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn one_hung_server_does_not_keep_the_others_running() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let quick = |n: Arc<AtomicUsize>| async move {
            n.fetch_add(1, Ordering::SeqCst);
        };
        let shutdowns: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> = vec![
            Box::pin(quick(Arc::clone(&stopped))),
            // A server whose shutdown never finishes.
            Box::pin(std::future::pending::<()>()),
            Box::pin(quick(Arc::clone(&stopped))),
        ];
        let started = std::time::Instant::now();
        let timed_out =
            shutdown_concurrently(shutdowns, std::time::Duration::from_millis(100)).await;
        assert_eq!(timed_out, 1);
        assert_eq!(
            stopped.load(Ordering::SeqCst),
            2,
            "the others still stopped"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "concurrent, not sequential"
        );
    }
}

/// #145: a poisoned lock fails closed. The user's per-tool choices are never dropped by
/// reading the lock as empty; every tool is refused with a clear error instead.
#[cfg(test)]
mod poison_tests {
    use super::*;

    fn executor() -> Executor {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for m in ledger::migrations() {
            conn.execute_batch(m).unwrap();
        }
        let (gate, _rx) = ApprovalGate::new(4);
        Executor::new(Arc::new(gate), Arc::new(Mutex::new(conn)))
    }

    fn tool(name: &str) -> crate::client::McpTool {
        crate::client::McpTool {
            name: name.into(),
            title: None,
            description: format!("{name} tool"),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            hints: Default::default(),
        }
    }

    /// `fs.delete_all` turned off by the user, `fs.read` left on.
    fn with_tools(ex: &Executor) {
        let off = CapabilityOverride {
            disabled: true,
            ..Default::default()
        };
        ex.set_overrides(HashMap::from([(
            "mcp.fs.delete_all".to_string(),
            off.clone(),
        )]));
        let mut caps = ex.capabilities.write().unwrap();
        caps.insert(
            "mcp.fs.delete_all".into(),
            McpCapability::from_tool("fs", &tool("delete_all"), Some(&off), true),
        );
        caps.insert(
            "mcp.fs.read".into(),
            McpCapability::from_tool("fs", &tool("read"), None, true),
        );
        drop(caps);
        ex.reindex_names();
    }

    fn poison<T: Send + Sync>(lock: &RwLock<T>) {
        std::thread::scope(|s| {
            let _ = s
                .spawn(|| {
                    let _g = lock.write().unwrap();
                    panic!("poison the lock (test)");
                })
                .join();
        });
        assert!(lock.is_poisoned());
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: serde_json::json!({}),
        }
    }

    fn ctx() -> CallContext {
        CallContext::new(Some("conv".into()), Id::new())
    }

    #[tokio::test]
    async fn a_disabled_tool_stays_refused_after_a_poisoned_overrides_lock() {
        let ex = executor();
        with_tools(&ex);
        // Before: the disabled tool is hidden and blocked; the other is offered.
        let names: Vec<String> = ex.tool_specs().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["mcp__fs__read"]);
        assert!(matches!(
            ex.preflight(&ctx(), &call("mcp__fs__delete_all")).unwrap(),
            Preflight::Blocked { .. }
        ));

        poison(&ex.overrides);
        // Applying overrides again (as Settings or a refresh does) must not rebuild the
        // capabilities without them.
        ex.set_overrides(HashMap::new());
        assert!(ex.is_unavailable());
        assert!(
            ex.capabilities.read().unwrap()["mcp.fs.delete_all"].disabled,
            "the user's choice was not dropped"
        );

        // Fail closed: nothing offered, every call refused with the clear error.
        assert!(ex.tool_specs().is_empty());
        assert!(ex.capabilities().is_empty());
        for name in ["mcp__fs__delete_all", "mcp__fs__read", "mcp.fs.delete_all"] {
            let e = ex
                .execute_tool_error(&ctx(), &call(name), std::future::pending())
                .await
                .unwrap_err();
            assert_eq!(e.kind, ToolErrorKind::Unavailable, "{name}");
            assert!(
                e.user_message().contains("Restart Demios"),
                "{}",
                e.user_message()
            );
            match ex.preflight(&ctx(), &call(name)).unwrap_err() {
                CoreError::Capability { user_message, .. } => {
                    assert!(user_message.contains("turned off after an internal error"));
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn any_poisoned_executor_lock_turns_every_tool_off() {
        for which in ["capabilities", "llm_names", "clients"] {
            let ex = executor();
            with_tools(&ex);
            match which {
                "capabilities" => poison(&ex.capabilities),
                "llm_names" => poison(&ex.llm_names),
                _ => poison(&ex.clients),
            }
            // The first access that meets the poisoned lock latches "unavailable".
            let _ = ex.resolve("mcp__fs__read");
            let _ = ex.client("fs");
            let _ = ex.tool_specs();
            assert!(ex.is_unavailable(), "{which}");
            let e = ex
                .execute_tool_error(&ctx(), &call("mcp__fs__read"), std::future::pending())
                .await
                .unwrap_err();
            assert_eq!(e.kind, ToolErrorKind::Unavailable, "{which}");
            assert!(ex.refresh("fs").await.is_err(), "{which}");
        }
    }

    #[tokio::test]
    async fn removing_servers_still_works_after_a_poisoned_lock() {
        let ex = executor();
        with_tools(&ex);
        poison(&ex.capabilities);
        assert_eq!(
            ex.remove_all_servers(std::time::Duration::from_millis(50))
                .await,
            0
        );
        assert!(ex.is_unavailable());
        assert!(
            ex.capabilities
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty()
        );
    }
}

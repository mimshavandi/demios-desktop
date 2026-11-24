//! Process-wide state shared by IPC commands.

use crate::agent::Agent;
use crate::error::{IpcError, IpcResult};
use crate::models::ModelRegistry;
use crate::secrets::{Keys, Secret};
use crate::settings::{self, Settings};
use dn_agent::{CancelToken, NoTools, ToolExecutor};
use dn_core::Id;
use dn_store::{Connection, MigrationSet, chat as store};
use dn_tools::{ApprovalDecision, ApprovalPrompt, ApprovalRequest};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Key-store id of the Hugging Face token (`dn_models::hub::hf_token_secret()`).
pub const HF_TOKEN_PROVIDER_ID: &str = "huggingface";

/// Keychain ids (dn-tools secret ids) an MCP server config refers to: secret env vars and
/// headers, and the OAuth token set.
pub fn mcp_secret_ids(servers: &[dn_tools::McpServerConfig]) -> Vec<String> {
    use dn_tools::{McpTransport, ValueRef};
    let mut out = Vec::new();
    for s in servers {
        let refs: Vec<&ValueRef> = match &s.transport {
            McpTransport::Stdio { env, .. } => env.values().collect(),
            McpTransport::StreamableHttp { headers, oauth, .. } => {
                if let Some(o) = oauth {
                    out.push(o.credential_secret_id.clone());
                }
                headers.values().collect()
            }
        };
        for r in refs {
            if let ValueRef::Secret { secret_id } = r {
                out.push(secret_id.clone());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every migration set the app composes into its database. Add a crate's
/// `migrations()` here when the app starts using that crate's tables.
pub fn migration_sets() -> Vec<MigrationSet> {
    vec![
        MigrationSet::new("app", settings::migrations()),
        MigrationSet::new("tool_ledger", dn_tools::migrations()),
    ]
}

/// Tool-runtime traffic to the UI: approval cards (design-system §4) and MCP server status.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolEvent {
    Requested {
        prompt: ApprovalPrompt,
    },
    /// Answered (`expired: false`), or timed out / abandoned because its run ended or was
    /// stopped (`expired: true`): remove the card.
    Resolved {
        request_id: Id,
        expired: bool,
    },
    McpServer {
        status: McpServerStatus,
    },
}

/// Connection state of one configured MCP server, for toasts and the Models page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct McpServerStatus {
    pub server_id: String,
    pub name: String,
    /// `connecting`, `connected` or `failed`.
    pub state: &'static str,
    /// User-facing reason when `failed`.
    pub message: Option<String>,
    pub tools: usize,
}

/// What the UI gets when it subscribes.
#[derive(Clone, Debug, Serialize)]
pub struct ToolSnapshot {
    pub approvals: Vec<ApprovalPrompt>,
    pub servers: Vec<McpServerStatus>,
}

type ToolSink = Arc<dyn Fn(ToolEvent) + Send + Sync>;

/// How long a removed provider key can be restored with Undo (it is gone from the key
/// store immediately and held only in memory meanwhile).
pub const KEY_UNDO_WINDOW: Duration = Duration::from_secs(15);

/// A running answer in one conversation. `token` identifies this run, so a finished
/// run never releases the slot of a newer one.
struct Running {
    token: u64,
    cancel: CancelToken,
}

/// Text streamed so far for an assistant message, so reopening a chat mid-stream shows it.
#[derive(Default)]
struct Live {
    text: String,
    utf16_len: u32,
}

struct RemovedKey {
    secret: Secret,
    slots: Vec<(String, String)>,
    at: Instant,
}

pub struct AppState {
    db: Mutex<Connection>,
    pub models: ModelRegistry,
    pub keys: Keys,
    pub agent: Agent,
    /// What the agent loop may call (the manifest-filtered dn-tools executor in the app).
    pub tools: Arc<dyn ToolExecutor>,
    /// The dn-tools executor itself, for outcome-unknown resolution. `None` in tests.
    pub executor: Option<Arc<dn_tools::Executor>>,
    approvals: Mutex<HashMap<Id, ApprovalRequest>>,
    approval_sink: Mutex<Option<ToolSink>>,
    mcp_status: Mutex<std::collections::BTreeMap<String, McpServerStatus>>,
    running: Mutex<HashMap<Id, Running>>,
    live: Mutex<HashMap<Id, Live>>,
    removed_keys: Mutex<HashMap<String, RemovedKey>>,
    next_token: AtomicU64,
}

/// A reserved generation slot. Dropping it without [`Reservation::into_parts`] releases
/// the slot, so an early return (failed DB write) never leaves a chat stuck "busy".
pub struct Reservation {
    state: Arc<AppState>,
    pub conversation_id: Id,
    pub token: u64,
    cancel: CancelToken,
    armed: bool,
}

impl Reservation {
    /// Hands the cancel token to the running task; the task must call
    /// [`AppState::release`] with `token` when it ends.
    pub fn into_parts(mut self) -> (u64, CancelToken) {
        self.armed = false;
        (self.token, self.cancel.clone())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            self.state.release(self.conversation_id, self.token);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // These maps only hold handles and buffers; recover rather than fail chats.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl AppState {
    pub fn new(conn: Connection, models: ModelRegistry, keys: Keys) -> Self {
        Self {
            db: Mutex::new(conn),
            models,
            keys,
            agent: Agent::load(None),
            tools: Arc::new(NoTools),
            executor: None,
            approvals: Mutex::new(HashMap::new()),
            approval_sink: Mutex::new(None),
            mcp_status: Mutex::new(Default::default()),
            running: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            removed_keys: Mutex::new(HashMap::new()),
            next_token: AtomicU64::new(1),
        }
    }

    /// Opens the database at `path` (creating it with every migration set), marks
    /// interrupted streams cancelled, purges chats deleted in an earlier session, gives
    /// dn-tools its own ledger connection (interrupted writes become `unknown` before any
    /// run), loads the agent and starts the approval bridge.
    pub fn open(
        path: &std::path::Path,
        skills: Option<&std::path::Path>,
    ) -> dn_store::Result<Arc<Self>> {
        let conn = dn_store::open(path, &migration_sets())?;
        let n = store::recover_interrupted(&conn)?;
        if n > 0 {
            eprintln!("marked {n} interrupted answer(s) as cancelled");
        }
        let purged = store::purge_deleted(&conn, dn_store::now_ms())?;

        let ledger = dn_store::open_connection(path)?;
        match dn_tools::ledger::mark_interrupted(&ledger) {
            Ok(0) => {}
            Ok(n) => eprintln!("{n} tool call(s) were interrupted; their outcome is unknown"),
            Err(e) => eprintln!("could not check the tool ledger: {e}"),
        }
        // A purged chat keeps no tool history either (unknown writes stay until resolved).
        for id in &purged {
            if let Err(e) = dn_tools::ledger::delete_scope(&ledger, &format!("conv:{id}")) {
                eprintln!("could not clear the tool ledger of a deleted chat: {e}");
            }
        }
        let (gate, approvals) = dn_tools::ApprovalGate::new(16);
        let executor = Arc::new(dn_tools::Executor::new(
            Arc::new(gate),
            Arc::new(Mutex::new(ledger)),
        ));
        let keys = Keys::os_keychain();
        let agent = Agent::load(skills);
        let tools = Arc::new(crate::agent::ManifestTools {
            executor: Arc::clone(&executor),
            manifest: agent.manifest.clone(),
        });
        let mut models = ModelRegistry::with_builtin();
        models.local = Some(crate::local::LocalModels::open());
        let mut state = Self::new(conn, models, keys);
        state.agent = agent;
        state.tools = tools;
        state.executor = Some(Arc::clone(&executor));
        let state = Arc::new(state);
        state.start_approval_bridge(approvals);
        state.connect_mcp_servers(executor);
        Ok(state)
    }

    /// Adds the MCP servers saved in settings (in the background: connecting can be slow).
    fn connect_mcp_servers(self: &Arc<Self>, executor: Arc<dn_tools::Executor>) {
        let servers = match self.settings() {
            Ok(s) => s.mcp_servers,
            Err(e) => {
                eprintln!("could not read MCP servers from settings: {}", e.message);
                return;
            }
        };
        if servers.is_empty() {
            return;
        }
        let secrets: Arc<dyn dn_tools::SecretStore> =
            Arc::new(dn_tools::DnSecrets(Arc::clone(&self.keys.store)));
        for cfg in &servers {
            self.set_mcp_status(McpServerStatus {
                server_id: cfg.id.clone(),
                name: cfg.display_name.clone().unwrap_or_else(|| cfg.id.clone()),
                state: "connecting",
                message: None,
                tools: 0,
            });
        }
        let state = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            for cfg in servers {
                let id = cfg.id.clone();
                let name = cfg.display_name.clone().unwrap_or_else(|| id.clone());
                let status = |state, message: Option<String>, tools| McpServerStatus {
                    server_id: id.clone(),
                    name: name.clone(),
                    state,
                    message,
                    tools,
                };
                let client = match dn_tools::McpClient::new(cfg, Arc::clone(&secrets)) {
                    Ok(c) => {
                        c.with_auth(Arc::new(dn_tools::OAuthManager::new(Arc::clone(&secrets))))
                    }
                    Err(e) => {
                        eprintln!("MCP server `{id}` is misconfigured: {e}");
                        state.set_mcp_status(status(
                            "failed",
                            Some(
                                "Its settings are invalid. Check the server configuration.".into(),
                            ),
                            0,
                        ));
                        continue;
                    }
                };
                match executor.add_server(client).await {
                    Ok(caps) => state.set_mcp_status(status("connected", None, caps.len())),
                    Err(e) => {
                        eprintln!("MCP server `{id}` could not be added: {e}");
                        let msg = crate::agent::model_error(e).message;
                        state.set_mcp_status(status("failed", Some(msg), 0));
                    }
                }
            }
        });
    }

    #[cfg(test)]
    pub fn in_memory() -> dn_store::Result<Self> {
        Ok(Self::new(
            dn_store::open_in_memory(&migration_sets())?,
            ModelRegistry::with_builtin(),
            Keys::memory(),
        ))
    }

    pub fn db(&self) -> IpcResult<MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| IpcError::new("storage", "Local database is unavailable."))
    }

    pub fn settings(&self) -> IpcResult<Settings> {
        Ok(settings::load(&*self.db()?)?)
    }

    pub fn save_settings(&self, s: &Settings) -> IpcResult<()> {
        s.validate().map_err(IpcError::invalid)?;
        Ok(settings::save(&*self.db()?, s)?)
    }

    pub fn create_conversation(&self) -> IpcResult<store::Conversation> {
        Ok(store::create_conversation(&*self.db()?, "New chat")?)
    }

    // ---- generations

    /// Reserves the conversation's single generation slot *before* anything is written.
    pub fn reserve(self: &Arc<Self>, conversation_id: Id) -> IpcResult<Reservation> {
        let mut running = lock(&self.running);
        if running.contains_key(&conversation_id) {
            return Err(IpcError::new(
                "busy",
                "An answer is already streaming in this chat.",
            ));
        }
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let cancel = CancelToken::new();
        running.insert(
            conversation_id,
            Running {
                token,
                cancel: cancel.clone(),
            },
        );
        Ok(Reservation {
            state: Arc::clone(self),
            conversation_id,
            token,
            cancel,
            armed: true,
        })
    }

    /// Frees the slot if it still belongs to `token`.
    pub fn release(&self, conversation_id: Id, token: u64) {
        let mut running = lock(&self.running);
        if running
            .get(&conversation_id)
            .is_some_and(|r| r.token == token)
        {
            running.remove(&conversation_id);
        }
    }

    /// Stops the running answer. The slot is freed at once so a new message can be sent;
    /// the stopped run still stores its partial text.
    pub fn stop(&self, conversation_id: Id) -> bool {
        match lock(&self.running).remove(&conversation_id) {
            Some(r) => {
                r.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancels every running reply.
    pub fn stop_all(&self) {
        for (_, r) in lock(&self.running).drain() {
            r.cancel.cancel();
        }
    }

    /// What "Delete all local data" would remove from the keychain, in the user's words
    /// (provider names, "Hugging Face token", tool-server secrets). Blocking.
    pub fn keychain_entries(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in crate::models::PROVIDERS {
            if self.keys.has(p.id).unwrap_or(false) {
                out.push(format!("{} key", p.name));
            }
        }
        if self.keys.has(HF_TOKEN_PROVIDER_ID).unwrap_or(false) {
            out.push("Hugging Face token".into());
        }
        let servers = self.settings().map(|s| s.mcp_servers).unwrap_or_default();
        let mcp = mcp_secret_ids(&servers)
            .into_iter()
            .filter(|id| {
                self.keys
                    .store
                    .get(&dn_secrets::SecretId::mcp(id))
                    .is_ok_and(|v| v.is_some())
            })
            .count();
        match mcp {
            0 => {}
            1 => out.push("1 tool-server secret or sign-in".into()),
            n => out.push(format!("{n} tool-server secrets or sign-ins")),
        }
        out
    }

    /// "Delete all local data" for everything this state owns: chats, tombstones,
    /// settings, the tool ledger, approval grants, and keychain entries (provider keys, the
    /// Hugging Face token, MCP secrets and sign-ins). Blocking. Keeps going after a failure
    /// and returns the parts it could not delete, in the user's words.
    pub fn wipe_local_data(&self) -> Vec<&'static str> {
        let mut failed = Vec::new();
        let mcp_servers = self.settings().map(|s| s.mcp_servers).unwrap_or_default();

        // Keychain.
        let mut keys_ok = true;
        let mut provider_ids: Vec<&str> = crate::models::PROVIDERS.iter().map(|p| p.id).collect();
        provider_ids.push(HF_TOKEN_PROVIDER_ID);
        for id in provider_ids {
            if let Err(e) = self.keys.delete(id) {
                eprintln!("could not delete the `{id}` key: {e}");
                keys_ok = false;
            }
            self.models.invalidate(id);
        }
        for id in mcp_secret_ids(&mcp_servers) {
            if let Err(e) = self.keys.store.delete(&dn_secrets::SecretId::mcp(&id)) {
                eprintln!("could not delete MCP secret `{id}`: {e}");
                keys_ok = false;
            }
        }
        lock(&self.removed_keys).clear();
        if !keys_ok {
            failed.push("keys in the system keychain");
        }

        // Database.
        let conversations = self
            .db()
            .ok()
            .and_then(|db| store::list_conversations(&db).ok())
            .unwrap_or_default();
        if let Some(exec) = &self.executor {
            for c in &conversations {
                exec.gate().clear_conversation(&format!("conv:{}", c.id));
            }
            if let Err(e) = exec.forget_all() {
                eprintln!("could not clear the tool ledger: {e}");
                failed.push("tool history");
            }
        }
        match self.db() {
            Ok(db) => {
                let r = store::delete_all(&db)
                    .and_then(|()| settings::delete_all(&db))
                    // Rewrite the file so deleted rows don't linger in free pages or the WAL.
                    .and_then(|()| {
                        db.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
                        Ok(())
                    });
                if let Err(e) = r {
                    eprintln!("could not clear the database: {e}");
                    failed.push("chats and settings");
                }
            }
            Err(_) => failed.push("chats and settings"),
        }
        lock(&self.live).clear();
        failed
    }

    pub fn is_generating(&self, conversation_id: Id) -> bool {
        lock(&self.running).contains_key(&conversation_id)
    }

    /// Appends streamed text; returns the UTF-16 length before it (the UI's string unit),
    /// which lets the UI skip deltas it already has after reopening a chat.
    pub fn append_live(&self, message_id: Id, text: &str) -> u32 {
        let mut live = lock(&self.live);
        let l = live.entry(message_id).or_default();
        let before = l.utf16_len;
        l.text.push_str(text);
        l.utf16_len += text.encode_utf16().count() as u32;
        before
    }

    pub fn live_len(&self, message_id: Id) -> u32 {
        lock(&self.live).get(&message_id).map_or(0, |l| l.utf16_len)
    }

    pub fn live_text(&self, message_id: Id) -> String {
        lock(&self.live)
            .get(&message_id)
            .map(|l| l.text.clone())
            .unwrap_or_default()
    }

    pub fn end_live(&self, message_id: Id) {
        lock(&self.live).remove(&message_id);
    }

    /// Messages of a conversation, with streaming answers filled in from memory.
    pub fn list_messages(&self, conversation_id: Id) -> IpcResult<Vec<store::StoredMessage>> {
        let mut msgs = store::list_messages(&*self.db()?, conversation_id)?;
        let live = lock(&self.live);
        for m in &mut msgs {
            if m.status == store::MessageStatus::Streaming
                && let Some(l) = live.get(&m.id)
            {
                m.content = vec![dn_core::chat::ContentPart::Text {
                    text: l.text.clone(),
                }];
            }
        }
        Ok(msgs)
    }

    // ---- approvals (dn-tools gate ↔ UI)

    pub(crate) fn start_approval_bridge(
        self: &Arc<Self>,
        mut rx: tokio::sync::mpsc::Receiver<ApprovalRequest>,
    ) {
        let state = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                // Sweep for expired requests only while some are waiting; otherwise just
                // wait for the next request.
                let waiting = !lock(&state.approvals).is_empty();
                let req = if waiting {
                    tokio::select! {
                        req = rx.recv() => req,
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {
                            state.prune_closed_approvals();
                            continue;
                        }
                    }
                } else {
                    rx.recv().await
                };
                match req {
                    Some(req) => state.add_approval(req),
                    None => break,
                }
            }
        });
    }

    /// Holds a pending request and shows it in the UI. Never answered here: no
    /// auto-allow, the user decides (or the gate's timeout denies).
    pub fn add_approval(&self, req: ApprovalRequest) {
        self.prune_closed_approvals();
        if req.is_closed() {
            return;
        }
        let prompt = req.prompt.clone();
        lock(&self.approvals).insert(prompt.request_id, req);
        self.emit_approval(ToolEvent::Requested { prompt });
    }

    /// Requests whose call is gone (timed out, stopped, run ended) leave the UI as expired.
    /// Keyed by request, so another run's pending approvals are never touched.
    pub fn prune_closed_approvals(&self) {
        let gone: Vec<Id> = {
            let mut m = lock(&self.approvals);
            let ids: Vec<Id> = m
                .iter()
                .filter(|(_, r)| r.is_closed())
                .map(|(id, _)| *id)
                .collect();
            for id in &ids {
                m.remove(id);
            }
            ids
        };
        for request_id in gone {
            self.emit_approval(ToolEvent::Resolved {
                request_id,
                expired: true,
            });
        }
    }

    fn emit_approval(&self, ev: ToolEvent) {
        let sink = lock(&self.approval_sink).clone();
        if let Some(sink) = sink {
            sink(ev);
        }
    }

    /// The UI subscribes once; it gets the requests already waiting and server states.
    pub fn watch_tools(&self, sink: ToolSink) -> ToolSnapshot {
        *lock(&self.approval_sink) = Some(sink);
        ToolSnapshot {
            approvals: self.pending_approvals(),
            servers: lock(&self.mcp_status).values().cloned().collect(),
        }
    }

    pub fn set_mcp_status(&self, status: McpServerStatus) {
        lock(&self.mcp_status).insert(status.server_id.clone(), status.clone());
        self.emit_approval(ToolEvent::McpServer { status });
    }

    pub fn pending_approvals(&self) -> Vec<ApprovalPrompt> {
        self.prune_closed_approvals();
        let mut v: Vec<ApprovalPrompt> = lock(&self.approvals)
            .values()
            .map(|r| r.prompt.clone())
            .collect();
        v.sort_by_key(|p| p.request_id);
        v
    }

    /// Answers a request. "Allow for this chat" is downgraded to "allow once" for
    /// destructive and sensitive capabilities (design-system §4). A request whose call is
    /// already gone is `expired`, never reported as approved.
    pub fn respond_approval(&self, request_id: Id, decision: ApprovalDecision) -> IpcResult<()> {
        let expired = || IpcError::new("expired", "This request expired before you answered.");
        let req = lock(&self.approvals)
            .remove(&request_id)
            .ok_or_else(expired)?;
        if req.is_closed() {
            self.emit_approval(ToolEvent::Resolved {
                request_id,
                expired: true,
            });
            return Err(expired());
        }
        let decision = match decision {
            ApprovalDecision::ApproveForConversation
                if req.prompt.risk >= dn_core::capability::RiskTier::Destructive =>
            {
                ApprovalDecision::Approve
            }
            d => d,
        };
        // The call can still go away between the check above and here: only a delivered
        // answer counts.
        let delivered = req.respond(decision);
        self.emit_approval(ToolEvent::Resolved {
            request_id,
            expired: !delivered,
        });
        if delivered { Ok(()) } else { Err(expired()) }
    }

    // ---- provider keys with Undo

    pub fn remember_removed_key(
        &self,
        provider_id: &str,
        secret: Secret,
        slots: Vec<(String, String)>,
    ) {
        let mut m = lock(&self.removed_keys);
        m.retain(|_, k| k.at.elapsed() < KEY_UNDO_WINDOW);
        m.insert(
            provider_id.to_string(),
            RemovedKey {
                secret,
                slots,
                at: Instant::now(),
            },
        );
    }

    /// The removed key and the slots it was cleared from, if still within the window.
    pub fn take_removed_key(&self, provider_id: &str) -> Option<(Secret, Vec<(String, String)>)> {
        let mut m = lock(&self.removed_keys);
        m.retain(|_, k| k.at.elapsed() < KEY_UNDO_WINDOW);
        m.remove(provider_id).map(|k| (k.secret, k.slots))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::capability::{ApprovalPolicy, RiskTier};

    fn prompt(risk: RiskTier) -> ApprovalPrompt {
        ApprovalPrompt {
            request_id: Id::new(),
            scope: "conv:c1".into(),
            capability_id: "mcp.fs.write_file".into(),
            title: "Write a file".into(),
            description: "Writes notes/todo.md".into(),
            risk,
            policy: ApprovalPolicy::AskOncePerConversation,
            arguments: serde_json::json!({ "path": "notes/todo.md" }),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approvals_wait_for_the_user_and_reach_the_ui() {
        let st = Arc::new(AppState::in_memory().unwrap());
        let seen = Arc::new(Mutex::new(Vec::<ToolEvent>::new()));
        let s2 = Arc::clone(&seen);
        assert!(
            st.watch_tools(Arc::new(move |e| s2.lock().unwrap().push(e)))
                .approvals
                .is_empty()
        );

        let (gate, rx) = dn_tools::ApprovalGate::new(4);
        let gate = Arc::new(gate);
        st.start_approval_bridge(rx);
        let p = prompt(RiskTier::LowWrite);
        let id = p.request_id;
        let g = Arc::clone(&gate);
        let waiting = tokio::spawn(async move { g.check(p).await });

        for _ in 0..100 {
            if !st.pending_approvals().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(st.pending_approvals().len(), 1, "never auto-allowed");
        assert!(!waiting.is_finished());
        st.respond_approval(id, ApprovalDecision::ApproveForConversation)
            .unwrap();
        assert!(waiting.await.unwrap().allowed());
        assert!(gate.is_satisfied(
            "conv:c1",
            "mcp.fs.write_file",
            ApprovalPolicy::AskOncePerConversation
        ));
        let evs = seen.lock().unwrap();
        assert!(matches!(evs[0], ToolEvent::Requested { .. }));
        assert!(
            matches!(evs[1], ToolEvent::Resolved { request_id, expired: false } if request_id == id)
        );
        assert!(
            st.respond_approval(id, ApprovalDecision::Approve).is_err(),
            "answered once"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn destructive_calls_are_never_granted_for_the_whole_chat() {
        let st = Arc::new(AppState::in_memory().unwrap());
        let (gate, rx) = dn_tools::ApprovalGate::new(4);
        let gate = Arc::new(gate);
        st.start_approval_bridge(rx);
        let p = prompt(RiskTier::Destructive);
        let id = p.request_id;
        let g = Arc::clone(&gate);
        let waiting = tokio::spawn(async move { g.check(p).await });
        for _ in 0..100 {
            if !st.pending_approvals().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        st.respond_approval(id, ApprovalDecision::ApproveForConversation)
            .unwrap();
        assert!(waiting.await.unwrap().allowed());
        assert!(!gate.is_satisfied(
            "conv:c1",
            "mcp.fs.write_file",
            ApprovalPolicy::AskOncePerConversation
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn closed_requests_expire_and_are_never_approved() {
        let st = Arc::new(AppState::in_memory().unwrap());
        let seen = Arc::new(Mutex::new(Vec::<ToolEvent>::new()));
        let s2 = Arc::clone(&seen);
        st.watch_tools(Arc::new(move |e| s2.lock().unwrap().push(e)));
        let (gate, rx) = dn_tools::ApprovalGate::new(4);
        let gate = Arc::new(gate.with_timeout(Duration::from_millis(100)));
        st.start_approval_bridge(rx);
        let p = prompt(RiskTier::LowWrite);
        let id = p.request_id;
        let g = Arc::clone(&gate);
        let waiting = tokio::spawn(async move { g.check(p).await });
        assert!(!waiting.await.unwrap().allowed(), "timeout denies");
        // The request is still listed until the next sweep; answering it now must fail.
        let err = st
            .respond_approval(id, ApprovalDecision::Approve)
            .unwrap_err();
        assert_eq!(err.code, "expired");
        for _ in 0..40 {
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, ToolEvent::Resolved { request_id, expired: true } if *request_id == id))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(seen.lock().unwrap().iter().any(
            |e| matches!(e, ToolEvent::Resolved { request_id, expired: true } if *request_id == id)
        ));
        assert!(st.pending_approvals().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_ending_run_does_not_touch_another_runs_approval() {
        let st = Arc::new(AppState::in_memory().unwrap());
        let (gate, rx) = dn_tools::ApprovalGate::new(4);
        let gate = Arc::new(gate);
        st.start_approval_bridge(rx);
        // Run A's call is cancelled (its future dropped); run B's call keeps waiting.
        let (ga, gb) = (Arc::clone(&gate), Arc::clone(&gate));
        let a = tokio::spawn(async move { ga.check(prompt(RiskTier::LowWrite)).await });
        let pb = prompt(RiskTier::LowWrite);
        let b_id = pb.request_id;
        let b = tokio::spawn(async move { gb.check(pb).await });
        for _ in 0..100 {
            if st.pending_approvals().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        a.abort();
        let _ = a.await;
        st.prune_closed_approvals();
        let pending = st.pending_approvals();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, b_id);
        st.respond_approval(b_id, ApprovalDecision::Approve)
            .unwrap();
        assert!(b.await.unwrap().allowed());
    }

    #[test]
    fn open_wires_the_ledger_and_marks_interrupted_calls() {
        let dir = std::env::temp_dir().join(format!("dn-app-open-{}", Id::new()));
        let path = dir.join("db.sqlite3");
        {
            let c = dn_store::open(&path, &migration_sets()).unwrap();
            assert!(dn_store::migrate::applied_version(&c, "tool_ledger").unwrap() > 0);
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let st = AppState::open(&path, None).unwrap();
            assert!(st.executor.is_some());
            assert!(
                st.executor
                    .as_ref()
                    .unwrap()
                    .unknown_outcomes()
                    .unwrap()
                    .is_empty()
            );
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn open_purges_deleted_chats_and_their_finished_tool_calls() {
        let dir = std::env::temp_dir().join(format!("dn-app-purge-{}", Id::new()));
        let path = dir.join("db.sqlite3");
        let (gone, kept) = {
            let c = dn_store::open(&path, &migration_sets()).unwrap();
            let gone = store::create_conversation(&c, "private title").unwrap();
            let kept = store::create_conversation(&c, "kept").unwrap();
            store::delete_conversation(&c, gone.id).unwrap();
            let l = dn_store::open_connection(&path).unwrap();
            let args = serde_json::json!({});
            for (key, conv) in [("done", gone.id), ("unk", gone.id), ("live", kept.id)] {
                dn_tools::ledger::claim(&l, key, "cap", &format!("conv:{conv}"), &args).unwrap();
            }
            dn_tools::ledger::succeed(&l, "done", &serde_json::json!("ok")).unwrap();
            dn_tools::ledger::mark_unknown(&l, "unk", "cancelled").unwrap();
            dn_tools::ledger::succeed(&l, "live", &serde_json::json!("ok")).unwrap();
            (gone, kept)
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let st = AppState::open(&path, None).unwrap();
            let exec = st.executor.as_ref().unwrap();
            assert!(exec.ledger_entry("done").unwrap().is_none());
            assert!(exec.ledger_entry("live").unwrap().is_some());
            // The unknown write waits for the user, then goes once resolved.
            assert!(exec.ledger_entry("unk").unwrap().is_some());
            {
                let db = st.db().unwrap();
                assert!(store::is_purged(&db, &gone.id.to_string()).unwrap());
                assert!(store::get_conversation(&db, kept.id).is_ok());
                let titles: i64 = db
                    .query_row(
                        "SELECT COUNT(*) FROM store_conversations WHERE title = 'private title'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(titles, 0);
            }
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wipe_local_data_clears_chats_settings_ledger_and_keys() {
        let conn = dn_store::open_in_memory(&migration_sets()).unwrap();
        let keys = Keys::memory();
        let mut st = AppState::new(conn, ModelRegistry::default(), keys.clone());
        let ledger = dn_store::open_in_memory(&migration_sets()).unwrap();
        let (gate, _approvals) = dn_tools::ApprovalGate::new(1);
        let exec = Arc::new(dn_tools::Executor::new(
            Arc::new(gate),
            Arc::new(Mutex::new(ledger)),
        ));
        st.executor = Some(Arc::clone(&exec));

        let c = st.create_conversation().unwrap();
        let mut settings = st.settings().unwrap();
        settings
            .slots
            .insert("chat".into(), "byok:openai:gpt-5".into());
        settings.mcp_servers = vec![
            serde_json::from_value(serde_json::json!({
                "id": "gh",
                "transport": {
                    "type": "streamable_http",
                    "url": "https://example.test/mcp",
                    "headers": {},
                    "oauth": { "credential_secret_id": "mcp.gh.oauth" }
                }
            }))
            .unwrap(),
        ];
        st.save_settings(&settings).unwrap();
        keys.set("openai", &Secret::new("test-key")).unwrap();
        keys.set(HF_TOKEN_PROVIDER_ID, &Secret::new("test-hf"))
            .unwrap();
        keys.store
            .set(
                &dn_secrets::SecretId::mcp("mcp.gh.oauth"),
                &Secret::new("{}"),
            )
            .unwrap();

        assert_eq!(
            st.keychain_entries(),
            [
                "OpenAI key",
                "Hugging Face token",
                "1 tool-server secret or sign-in"
            ]
        );
        assert!(st.wipe_local_data().is_empty());
        assert!(st.keychain_entries().is_empty());
        assert!(
            store::list_conversations(&st.db().unwrap())
                .unwrap()
                .is_empty()
        );
        assert!(store::get_conversation(&st.db().unwrap(), c.id).is_err());
        assert_eq!(st.settings().unwrap(), Settings::default());
        assert!(!keys.has("openai").unwrap());
        assert!(!keys.has(HF_TOKEN_PROVIDER_ID).unwrap());
        assert!(
            keys.store
                .get(&dn_secrets::SecretId::mcp("mcp.gh.oauth"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mcp_secret_ids_cover_env_headers_and_oauth() {
        let servers: Vec<dn_tools::McpServerConfig> = serde_json::from_value(serde_json::json!([
            {
                "id": "a",
                "transport": {
                    "type": "stdio",
                    "command": "x",
                    "env": {
                        "API_TOKEN": { "kind": "secret", "secret_id": "mcp.a.token" },
                        "LOG": { "kind": "plain", "value": "debug" }
                    }
                }
            },
            {
                "id": "b",
                "transport": {
                    "type": "streamable_http",
                    "url": "https://example.test/mcp",
                    "headers": { "Authorization": { "kind": "secret", "secret_id": "mcp.b.auth" } },
                    "oauth": { "credential_secret_id": "mcp.b.oauth" }
                }
            }
        ]))
        .unwrap();
        assert_eq!(
            mcp_secret_ids(&servers),
            ["mcp.a.token", "mcp.b.auth", "mcp.b.oauth"]
        );
    }
}

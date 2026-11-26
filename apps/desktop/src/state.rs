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

/// dn-tools' secret store over the keychain that also records each id it writes in
/// `app_mcp_secret_ids` (T-STORE-11), so a later "Delete all local data" can find it.
struct TrackedMcpSecrets {
    inner: dn_tools::DnSecrets,
    state: std::sync::Weak<AppState>,
    /// `AppState::mcp_epoch` when made; writes are refused once it changes.
    epoch: u64,
}

impl dn_tools::SecretStore for TrackedMcpSecrets {
    fn get(&self, id: &str) -> Result<Option<String>, dn_tools::secrets::SecretError> {
        self.inner.get(id)
    }
    fn set(&self, id: &str, value: &str) -> Result<(), dn_tools::secrets::SecretError> {
        // Record first: an id we fail to record could outlive a wipe.
        if let Some(st) = self.state.upgrade() {
            if st.mcp_epoch.load(Ordering::SeqCst) != self.epoch {
                return Err(dn_tools::secrets::SecretError(
                    "local data was deleted; not storing tool-server credentials".into(),
                ));
            }
            let db = st
                .db()
                .map_err(|e| dn_tools::secrets::SecretError(e.message))?;
            settings::record_mcp_secret_id(&db, id)
                .map_err(|e| dn_tools::secrets::SecretError(e.to_string()))?;
        }
        self.inner.set(id, value)
    }
    fn delete(&self, id: &str) -> Result<(), dn_tools::secrets::SecretError> {
        self.inner.delete(id)
    }
}

/// How long "Delete all local data" waits for cancelled replies to end (shorter in tests).
const REPLY_STOP_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    Duration::from_secs(5)
};

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
        MigrationSet::new("app_skills", crate::skills::migrations()),
        MigrationSet::new("tool_ledger", dn_tools::migrations()),
        MigrationSet::new("artifact", dn_artifacts::migrations()),
        MigrationSet::new("knowledge", dn_knowledge::migrations()),
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
    /// The server was removed from settings.
    McpServerRemoved {
        server_id: String,
    },
}

/// Connection state of one configured MCP server, for toasts and the Models page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct McpServerStatus {
    pub server_id: String,
    pub name: String,
    /// `connecting`, `connected`, `needs_auth` (an OAuth server without a valid sign-in),
    /// `signing_in` (waiting for the browser) or `failed`.
    pub state: &'static str,
    /// User-facing reason when `failed` or `needs_auth`.
    pub message: Option<String>,
    pub tools: usize,
    /// Tools that are new or changed since they were reviewed (they always ask meanwhile).
    pub review: usize,
    /// Bumped each time the server changed its tools mid-session (#178), so the Tools view
    /// reloads even when the counts stay the same.
    pub revision: u32,
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

/// A removed OpenAI-compatible server, kept for [`KEY_UNDO_WINDOW`] so Undo can put it back
/// with its key and the tasks that used it (T-PRV-05).
pub struct RemovedServer {
    pub server: settings::RemoteServer,
    pub key: Option<Secret>,
    pub slots: Vec<(String, String)>,
    at: Instant,
}

pub struct AppState {
    db: Mutex<Connection>,
    pub models: ModelRegistry,
    pub keys: Keys,
    /// The keychain check Settings › Models waits for (#200).
    pub key_checks: crate::secrets::KeyChecks,
    pub agent: Agent,
    /// Chat skills the user can pick per chat (T-SKL-01).
    pub skills: crate::skills::Library,
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
    removed_servers: Mutex<HashMap<String, RemovedServer>>,
    next_token: AtomicU64,
    /// Bumped by "Delete all local data": a background MCP connect started before it must
    /// not add its server afterwards.
    mcp_epoch: AtomicU64,
    /// Whether this build may start local (stdio) MCP servers (not in Flatpak).
    pub local_servers: bool,
    /// Running Hugging Face downloads (add-model screen).
    pub hub_pulls: Arc<crate::hub::HubPulls>,
    /// `demios://models/add` links waiting for or shown on the confirmation screen.
    pub add_links: crate::deeplink::AddLinks,
    /// Asks before a local MCP server starts (native dialog in the app; tests set a fake).
    pub launch_approver: std::sync::OnceLock<Arc<dyn crate::mcp::LaunchApprover>>,
    /// Servers whose tools are hidden from the model while they connect (T-SHELL-11).
    pub tools_settling: Arc<crate::tool_review::Settling>,
    /// Local copies of project files (`<data dir>/project-files`). `None` in tests that
    /// don't use them.
    pub project_files: Option<std::path::PathBuf>,
}

/// [`AppState::keychain_entries`]: what was found, and why the list may be incomplete.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct KeychainEntries {
    pub entries: Vec<String>,
    /// The keychain failed: the entries may be incomplete, and this says why.
    pub problem: Option<String>,
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
        // The registry serves the OpenAI-compatible servers saved in settings.
        if let Ok(s) = settings::load(&conn) {
            models.set_remote_servers(&s.remote_servers);
        }
        Self {
            db: Mutex::new(conn),
            models,
            keys,
            key_checks: Default::default(),
            agent: Agent::empty(),
            skills: Default::default(),
            tools: Arc::new(NoTools),
            executor: None,
            approvals: Mutex::new(HashMap::new()),
            approval_sink: Mutex::new(None),
            mcp_status: Mutex::new(Default::default()),
            running: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            removed_keys: Mutex::new(HashMap::new()),
            removed_servers: Mutex::new(HashMap::new()),
            next_token: AtomicU64::new(1),
            mcp_epoch: AtomicU64::new(0),
            local_servers: crate::mcp::local_servers_allowed(),
            hub_pulls: Default::default(),
            add_links: Default::default(),
            launch_approver: std::sync::OnceLock::new(),
            tools_settling: Default::default(),
            project_files: None,
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
        Self::open_with(path, skills, None)
    }

    /// [`Self::open`] with the launch approver for local MCP servers, set before the saved
    /// servers start connecting.
    pub fn open_with(
        path: &std::path::Path,
        skills: Option<&std::path::Path>,
        launch_approver: Option<Arc<dyn crate::mcp::LaunchApprover>>,
    ) -> dn_store::Result<Arc<Self>> {
        let conn = dn_store::open(path, &migration_sets())?;
        let n = store::recover_interrupted(&conn)?;
        if n > 0 {
            eprintln!("marked {n} interrupted answer(s) as cancelled");
        }
        store::purge_deleted(&conn, dn_store::now_ms())?;
        // Then projects and project files deleted earlier (their chats went just above).
        let project_files = path.parent().map(|d| d.join(crate::projects::FILES_DIR));
        if let Some(root) = &project_files {
            crate::projects::purge_on_start(&conn, root)?;
        }
        // Every purged chat, including ones purged by older builds that kept ledger rows.
        let tombstones = store::list_tombstones(&conn)?;
        // A purged chat keeps no artifacts either (idempotent: runs on every start).
        let artifacts = dn_artifacts::ArtifactStore::new(&conn);
        for (id, _) in &tombstones {
            if let Err(e) = artifacts.delete_conversation(*id) {
                eprintln!("could not delete the artifacts of a deleted chat: {e}");
            }
            if let Err(e) = crate::skills::forget_chat(&conn, *id) {
                eprintln!("could not forget the skill of a deleted chat: {e}");
            }
        }

        let ledger = dn_store::open_connection(path)?;
        match dn_tools::ledger::mark_interrupted(&ledger) {
            Ok(0) => {}
            Ok(n) => eprintln!("{n} tool call(s) were interrupted; their outcome is unknown"),
            Err(e) => eprintln!("could not check the tool ledger: {e}"),
        }
        let (gate, approvals) = dn_tools::ApprovalGate::new(16);
        let executor = Arc::new(dn_tools::Executor::new(
            Arc::new(gate),
            Arc::new(Mutex::new(ledger)),
        ));
        // A purged chat keeps no tool history either (unknown writes stay until resolved).
        for (id, _) in &tombstones {
            if let Err(e) = executor.forget_conversation(&id.to_string()) {
                eprintln!("could not clear the tool ledger of a deleted chat: {e}");
            }
        }
        let keys = Keys::os_keychain();
        let agent = Agent::load(skills);
        let settling: Arc<crate::tool_review::Settling> = Default::default();
        let tools = Arc::new(crate::agent::ManifestTools {
            executor: Arc::clone(&executor),
            manifest: agent.manifest.clone(),
            settling: Arc::clone(&settling),
        });
        let mut models = ModelRegistry::with_builtin();
        models.local = Some(crate::local::LocalModels::open());
        let mut state = Self::new(conn, models, keys);
        state.agent = agent;
        // The user's own skills sit next to the database, in the app's data directory.
        let user_skills = path.parent().map(|d| d.join(crate::skills::USER_DIR));
        state.skills = crate::skills::Library::load(skills, user_skills);
        state.tools = tools;
        state.executor = Some(Arc::clone(&executor));
        state.tools_settling = settling;
        state.project_files = project_files;
        if let Some(a) = launch_approver {
            let _ = state.launch_approver.set(a);
        }
        let state = Arc::new(state);
        state.start_approval_bridge(approvals);
        state.watch_tool_changes(Arc::clone(&executor));
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
        let epoch = self.mcp_epoch.load(Ordering::SeqCst);
        for cfg in &servers {
            self.set_mcp_status(McpServerStatus {
                server_id: cfg.id.clone(),
                name: cfg.display_name.clone().unwrap_or_else(|| cfg.id.clone()),
                state: "connecting",
                message: None,
                tools: 0,
                review: 0,
                revision: 0,
            });
        }
        let state = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            for cfg in servers {
                if !state.connect_mcp_server(&executor, cfg, epoch).await {
                    return; // "Delete all local data" ran meanwhile.
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

    /// Folder of the project files' local copies (`<data dir>/project-files`), if this
    /// state has one. Read-only for everything but [`crate::projects`].
    pub fn project_files_root(&self) -> Option<&std::path::Path> {
        self.project_files.as_deref()
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
        settings::save(&*self.db()?, s)?;
        self.models.set_remote_servers(&s.remote_servers);
        Ok(())
    }

    pub fn create_conversation(&self) -> IpcResult<store::Conversation> {
        self.create_conversation_in(None)
    }

    /// A new chat, in `project_id` when given.
    pub fn create_conversation_in(&self, project_id: Option<Id>) -> IpcResult<store::Conversation> {
        Ok(store::create_conversation_in(
            &*self.db()?,
            "New chat",
            project_id,
        )?)
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

    /// Cancels every running reply and waits (up to `timeout`) until each has ended and
    /// released its slot, so nothing writes after a wipe. Returns whether all ended.
    pub async fn stop_all_and_wait(&self, timeout: Duration) -> bool {
        for r in lock(&self.running).values() {
            r.cancel.cancel();
        }
        let deadline = Instant::now() + timeout;
        loop {
            if lock(&self.running).is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                lock(&self.running).clear();
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Adds an MCP server unless "Delete all local data" ran since `epoch` (before or while
    /// connecting); then the server is removed again and `None` returned.
    pub async fn add_mcp_server_if_current(
        &self,
        executor: &dn_tools::Executor,
        client: dn_tools::McpClient,
        epoch: u64,
    ) -> Option<Result<Vec<dn_tools::McpCapability>, dn_core::CoreError>> {
        if self.mcp_epoch.load(Ordering::SeqCst) != epoch {
            return None;
        }
        let id = client.server_id().to_owned();
        let added = executor.add_server(client).await;
        if self.mcp_epoch.load(Ordering::SeqCst) != epoch {
            executor.remove_server(&id).await;
            return None;
        }
        Some(added)
    }

    /// The secret store MCP servers use: the keychain, recording every id written so
    /// "Delete all local data" finds it later. It refuses writes once a wipe has started
    /// after it was made, so a server still shutting down can't store a token again.
    pub fn mcp_secret_store(self: &Arc<Self>) -> Arc<dyn dn_tools::SecretStore> {
        Arc::new(TrackedMcpSecrets {
            inner: dn_tools::DnSecrets(Arc::clone(&self.keys.store)),
            state: Arc::downgrade(self),
            epoch: self.mcp_epoch.load(Ordering::SeqCst),
        })
    }

    /// Saves a new MCP server list and deletes the keychain secrets no remaining server
    /// refers to (a removed server's tokens and sign-ins). Blocking.
    pub fn set_mcp_servers(&self, servers: Vec<dn_tools::McpServerConfig>) -> IpcResult<()> {
        let mut settings = self.settings()?;
        let before = mcp_secret_ids(&settings.mcp_servers);
        let after = mcp_secret_ids(&servers);
        settings.mcp_servers = servers;
        crate::tool_review::forget_removed(&mut settings);
        self.save_settings(&settings)?;
        for id in before.iter().filter(|id| !after.contains(id)) {
            self.keys.store.delete(&dn_secrets::SecretId::mcp(id))?;
            settings::forget_mcp_secret_id(&*self.db()?, id)?;
        }
        Ok(())
    }

    /// "Delete all local data", in order: stop replies (and wait for them), remove and
    /// stop every MCP server (so none can write a refreshed token back), delete the models,
    /// then the database and keychain entries ([`Self::wipe_local_data`]). Keeps going after
    /// a failure and returns the parts it could not delete, in the user's words.
    pub async fn delete_all_local_data(self: &Arc<Self>) -> Vec<&'static str> {
        let mut failed = Vec::new();
        if !self.stop_all_and_wait(REPLY_STOP_TIMEOUT).await {
            eprintln!("some replies did not stop in time; deleting anyway");
            failed.push("an answer that was still being written (it may have been saved again)");
        }
        self.mcp_epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(exec) = &self.executor {
            let stuck = exec.remove_all_servers(Duration::from_secs(2)).await;
            if stuck > 0 {
                eprintln!("{stuck} tool server(s) did not stop in time");
            }
            exec.gate().clear_all();
        }
        lock(&self.mcp_status).clear();
        if let Some(local) = self.models.local.clone()
            && let Err(e) = local.delete_all().await
        {
            eprintln!("could not delete the models folder: {e}");
            failed.push("downloaded models");
        }
        let st = Arc::clone(self);
        match tokio::task::spawn_blocking(move || st.wipe_local_data()).await {
            Ok(more) => failed.extend(more),
            Err(e) => {
                eprintln!("wipe task failed: {e}");
                failed.push("chats, settings and keys");
            }
        }
        failed
    }

    /// What "Delete all local data" would remove from the keychain, in the user's words
    /// (provider names, "Hugging Face token", tool-server secrets). Blocking. If the keychain
    /// fails, the list stops there and `problem` says why (#200): never "none saved".
    pub fn keychain_entries(&self) -> KeychainEntries {
        use dn_secrets::SecretId;
        let mut problem: Option<crate::secrets::KeychainStatus> = None;
        let mut has = |id: SecretId| -> bool {
            if problem.is_some() {
                return false;
            }
            match self.keys.store.get(&id) {
                Ok(v) => v.is_some(),
                Err(e) => {
                    problem = Some(crate::secrets::KeychainStatus::from_error(&e));
                    false
                }
            }
        };
        let mut out = Vec::new();
        for p in crate::models::PROVIDERS {
            if has(SecretId::provider(p.id)) {
                out.push(format!("{} key", p.name));
            }
        }
        if has(SecretId::provider(HF_TOKEN_PROVIDER_ID)) {
            out.push("Hugging Face token".into());
        }
        for r in self
            .settings()
            .map(|s| s.remote_servers)
            .unwrap_or_default()
        {
            if has(SecretId::provider(r.key_id())) {
                out.push(format!("{} server key", r.name));
            }
        }
        let servers = self.settings().map(|s| s.mcp_servers).unwrap_or_default();
        let mut ids = mcp_secret_ids(&servers);
        if let Ok(db) = self.db()
            && let Ok(known) = settings::known_mcp_secret_ids(&db)
        {
            ids.extend(known);
        }
        ids.sort();
        ids.dedup();
        let mcp = ids.into_iter().filter(|id| has(SecretId::mcp(id))).count();
        match mcp {
            0 => {}
            1 => out.push("1 tool-server secret or sign-in".into()),
            n => out.push(format!("{n} tool-server secrets or sign-ins")),
        }
        KeychainEntries {
            entries: out,
            problem: problem.and_then(|p| p.message().map(String::from)),
        }
    }

    /// "Delete all local data" for everything this state owns: chats, tombstones,
    /// settings, the tool ledger, approval grants, and keychain entries (provider keys, the
    /// Hugging Face token, MCP secrets and sign-ins). Blocking. Keeps going after a failure
    /// and returns the parts it could not delete, in the user's words.
    pub fn wipe_local_data(&self) -> Vec<&'static str> {
        let mut failed = Vec::new();
        // MCP secrets: those the saved servers name, plus every id ever written (servers
        // removed earlier). If settings can't be read, report it: their secrets may remain.
        let (mut mcp_ids, remote_key_ids) = match self.settings() {
            Ok(s) => (
                mcp_secret_ids(&s.mcp_servers),
                s.remote_servers
                    .iter()
                    .map(|r| r.key_id())
                    .collect::<Vec<_>>(),
            ),
            Err(e) => {
                eprintln!("could not read settings: {}", e.message);
                failed.push("tool-server sign-ins and server keys (settings unreadable)");
                (Vec::new(), Vec::new())
            }
        };
        match self
            .db()
            .map_err(|e| e.message)
            .and_then(|db| settings::known_mcp_secret_ids(&db).map_err(|e| e.to_string()))
        {
            Ok(ids) => mcp_ids.extend(ids),
            Err(e) => {
                eprintln!("could not read the MCP secret list: {e}");
                failed.push("tool-server sign-ins (list unreadable)");
            }
        }
        mcp_ids.sort();
        mcp_ids.dedup();

        // Keychain.
        let mut keys_ok = true;
        let mut provider_ids: Vec<&str> = crate::models::PROVIDERS.iter().map(|p| p.id).collect();
        provider_ids.push(HF_TOKEN_PROVIDER_ID);
        // Keys of OpenAI-compatible servers (removing a server deletes its key at once).
        provider_ids.extend(remote_key_ids.iter().map(String::as_str));
        for id in provider_ids {
            if let Err(e) = self.keys.delete(id) {
                eprintln!("could not delete the `{id}` key: {e}");
                keys_ok = false;
            }
            self.models.invalidate(id);
        }
        for id in mcp_ids {
            if let Err(e) = self.keys.store.delete(&dn_secrets::SecretId::mcp(&id)) {
                eprintln!("could not delete MCP secret `{id}`: {e}");
                keys_ok = false;
            }
        }
        lock(&self.removed_keys).clear();
        lock(&self.removed_servers).clear();
        if !keys_ok {
            failed.push("keys in the system keychain");
        }

        // Database.
        if let Some(exec) = &self.executor {
            exec.gate().clear_all();
            if let Err(e) = exec.forget_all() {
                eprintln!("could not clear the tool ledger: {e}");
                failed.push("tool history");
            }
        }
        match self.db() {
            Ok(db) => {
                let r = store::delete_all(&db)
                    .and_then(|()| settings::delete_all(&db))
                    .and_then(|()| crate::skills::delete_all(&db));
                let artifacts = dn_artifacts::ArtifactStore::new(&db).delete_all();
                if let Err(e) = &artifacts {
                    eprintln!("could not delete artifacts: {e}");
                    failed.push("artifacts");
                }
                if r.is_ok() {
                    self.models.set_remote_servers(&[]);
                }
                if let Err(e) = dn_knowledge::delete_all(&db) {
                    eprintln!("could not delete the knowledge index: {e}");
                    failed.push("knowledge index");
                }
                if let Some(root) = &self.project_files {
                    match std::fs::remove_dir_all(root) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            eprintln!("could not delete project files: {e}");
                            failed.push("project files");
                        }
                    }
                }
                if let Err(e) = r {
                    eprintln!("could not clear the database: {e}");
                    failed.push("chats and settings");
                } else if artifacts.is_ok()
                    // Rewrite the file so deleted rows don't linger in free pages or the WAL.
                    && let Err(e) = db.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
                {
                    // The rows are gone (secure_delete zeroed them); only compaction failed.
                    eprintln!("could not compact the database: {e}");
                    failed.push("compacting the database file");
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

    /// Updates the "to review" count of a connected server's status.
    pub fn set_mcp_review_count(&self, server_id: &str, review: usize) {
        let updated = {
            let mut all = lock(&self.mcp_status);
            match all.get_mut(server_id) {
                Some(st) if st.review != review => {
                    st.review = review;
                    Some(st.clone())
                }
                _ => None,
            }
        };
        if let Some(status) = updated {
            self.emit_approval(ToolEvent::McpServer { status });
        }
    }

    pub fn mcp_statuses(&self) -> std::collections::BTreeMap<String, McpServerStatus> {
        lock(&self.mcp_status).clone()
    }

    pub fn clear_mcp_status(&self, server_id: &str) {
        lock(&self.mcp_status).remove(server_id);
        self.emit_approval(ToolEvent::McpServerRemoved {
            server_id: server_id.to_owned(),
        });
    }

    /// Re-reviews a server's tools whenever it sends `tools/list_changed` (#178). The
    /// executor has already marked the server stale (its tools left out and refused) when the
    /// notice arrives. Holds only a weak reference, so it ends with the app state.
    pub(crate) fn watch_tool_changes(self: &Arc<Self>, executor: Arc<dn_tools::Executor>) {
        let mut changes = executor.tool_changes();
        let state = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                let ids: Vec<String> = match changes.recv().await {
                    Ok(id) => vec![id],
                    // Notices were dropped: every server marked stale is reviewed again.
                    Err(RecvError::Lagged(_)) => executor.stale_servers(),
                    Err(RecvError::Closed) => return,
                };
                let Some(state) = state.upgrade() else {
                    return;
                };
                for id in ids {
                    state.tools_changed(&executor, &id).await;
                }
            }
        });
    }

    /// One server changed its tools: list them again, flag new and changed ones (they
    /// always ask until reviewed, as on connect), and only then offer them again. A failed
    /// re-list leaves the server stale, with its tools refused, and says so in its status.
    pub async fn tools_changed(&self, executor: &dn_tools::Executor, id: &str) {
        if executor.client(id).is_none() {
            // Removed meanwhile ("Delete all", or the user removed it); removal cleared it.
            return;
        }
        let _settling = self.tools_settling.hold(id);
        // Before listing: a change that arrives during this review bumps it, and `settled`
        // then leaves the server stale for the next review.
        let generation = executor.tool_list_generation(id);
        let previous = lock(&self.mcp_status).get(id).cloned();
        let name = previous
            .as_ref()
            .map_or_else(|| id.to_owned(), |p| p.name.clone());
        let revision = previous.as_ref().map_or(0, |p| p.revision) + 1;
        let failed = |message: &str| McpServerStatus {
            server_id: id.to_owned(),
            name: name.clone(),
            state: "failed",
            message: Some(message.into()),
            tools: 0,
            review: 0,
            revision,
        };
        let caps = match executor.refresh(id).await {
            Ok(caps) => caps,
            Err(e) => {
                eprintln!("MCP server `{id}` changed its tools, and listing them failed: {e:?}");
                self.set_mcp_status(failed(
                    "It changed its tools, and they couldn't be read again. Use Reconnect.",
                ));
                return;
            }
        };
        match self.apply_tool_policy(executor, Some(id)) {
            Ok(review) => {
                executor.settled(id, generation);
                self.set_mcp_status(McpServerStatus {
                    server_id: id.to_owned(),
                    name,
                    state: "connected",
                    message: None,
                    tools: caps.len(),
                    review,
                    revision,
                });
            }
            Err(e) => {
                eprintln!("could not apply tool settings for `{id}`: {}", e.message);
                self.set_mcp_status(failed(
                    "It changed its tools, and its tool settings couldn't be applied. Use Reconnect.",
                ));
            }
        }
    }

    /// Connects one server (replacing a running client of the same id) and reports its
    /// status. Returns false when "Delete all local data" ran since `epoch`.
    pub async fn connect_mcp_server(
        self: &Arc<Self>,
        executor: &dn_tools::Executor,
        cfg: dn_tools::McpServerConfig,
        epoch: u64,
    ) -> bool {
        let id = cfg.id.clone();
        let name = cfg.display_name.clone().unwrap_or_else(|| id.clone());
        let status = |state, message: Option<String>, tools| McpServerStatus {
            server_id: id.clone(),
            name: name.clone(),
            state,
            message,
            tools,
            review: 0,
            revision: 0,
        };
        // Connect only what is saved now: the server may have been removed or edited while
        // this was waiting (a save's background connect, a sign-in).
        match self.mcp_server(&id) {
            Ok(current) if current == cfg => {}
            _ => return true,
        }
        executor.remove_server(&id).await;
        if let Some(req) = crate::mcp::LaunchRequest::of(&cfg) {
            if !self.local_servers {
                self.set_mcp_status(status(
                    "failed",
                    Some(crate::mcp::LOCAL_SERVERS_UNAVAILABLE.into()),
                    0,
                ));
                return true;
            }
            if !self.approve_launch(&id, req).await {
                self.set_mcp_status(status(
                    "failed",
                    Some(
                        "Not started: you didn't allow it to run. Use Reconnect to be asked again."
                            .into(),
                    ),
                    0,
                ));
                return true;
            }
        }
        self.set_mcp_status(status("connecting", None, 0));
        let secrets = self.mcp_secret_store();
        let client = match dn_tools::McpClient::new(cfg, Arc::clone(&secrets)) {
            Ok(c) => c.with_auth(Arc::new(dn_tools::OAuthManager::new(secrets))),
            Err(e) => {
                eprintln!("MCP server `{id}` is misconfigured: {e}");
                self.set_mcp_status(status(
                    "failed",
                    Some("Its settings are invalid. Edit the server to fix them.".into()),
                    0,
                ));
                return true;
            }
        };
        // Hidden from the model until the review policy below is applied, so a tool that
        // changed since it was reviewed can't auto-run in between.
        let _settling = self.tools_settling.hold(&id);
        let Some(added) = self
            .add_mcp_server_if_current(executor, client, epoch)
            .await
        else {
            return false;
        };
        match added {
            Ok(caps) => match self.apply_tool_policy(executor, Some(&id)) {
                Ok(review) => self.set_mcp_status(McpServerStatus {
                    review,
                    revision: 0,
                    ..status("connected", None, caps.len())
                }),
                Err(e) => {
                    eprintln!("could not apply tool settings for `{id}`: {}", e.message);
                    executor.remove_server(&id).await;
                    self.set_mcp_status(status(
                        "failed",
                        Some("Couldn't load its tool settings. Use Reconnect to try again.".into()),
                        0,
                    ));
                }
            },
            Err(e) => {
                let needs_auth = executor
                    .client(&id)
                    .is_some_and(|c| c.status() == dn_tools::ConnectionStatus::NeedsAuth);
                if needs_auth {
                    self.set_mcp_status(status(
                        "needs_auth",
                        Some("Sign in to use this server's tools.".into()),
                        0,
                    ));
                } else {
                    eprintln!("MCP server `{id}` could not be added: {e}");
                    let msg = crate::agent::model_error(e).message;
                    self.set_mcp_status(status("failed", Some(msg), 0));
                }
            }
        }
        true
    }

    /// Whether the user allowed this exact command line for this server, asking (native
    /// dialog, outside the webview) when they haven't yet. A "yes" is remembered.
    async fn approve_launch(&self, id: &str, req: crate::mcp::LaunchRequest) -> bool {
        let line = req.command_line();
        let known = self
            .db()
            .ok()
            .and_then(|db| settings::mcp_launch_approved(&db, id, &line).ok())
            .unwrap_or(false);
        if known {
            return true;
        }
        let Some(approver) = self.launch_approver.get() else {
            return false;
        };
        if !approver.ask(req).await {
            return false;
        }
        match self.db() {
            Ok(db) => {
                if let Err(e) = settings::record_mcp_launch(&db, id, &line) {
                    eprintln!("could not remember a launch approval: {e}");
                }
            }
            Err(_) => eprintln!("could not remember a launch approval"),
        }
        true
    }

    /// The current `mcp_epoch` (see [`Self::delete_all_local_data`]).
    pub fn mcp_epoch_now(&self) -> u64 {
        self.mcp_epoch.load(Ordering::SeqCst)
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

    pub fn remember_removed_server(
        &self,
        server: settings::RemoteServer,
        key: Option<Secret>,
        slots: Vec<(String, String)>,
    ) {
        let mut m = lock(&self.removed_servers);
        m.retain(|_, r| r.at.elapsed() < KEY_UNDO_WINDOW);
        m.insert(
            server.id.clone(),
            RemovedServer {
                server,
                key,
                slots,
                at: Instant::now(),
            },
        );
    }

    /// The removed server, if still within the undo window.
    pub fn take_removed_server(&self, server_id: &str) -> Option<RemovedServer> {
        let mut m = lock(&self.removed_servers);
        m.retain(|_, r| r.at.elapsed() < KEY_UNDO_WINDOW);
        m.remove(server_id)
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
            let arts = dn_artifacts::ArtifactStore::new(&c);
            let ctx = dn_artifacts::OpContext::model(None);
            let html = dn_core::artifact::ArtifactType::Html;
            for conv in [gone.id, kept.id] {
                arts.create(conv, &html, "Page", "<p>x</p>", &ctx).unwrap();
            }
            let l = dn_store::open_connection(&path).unwrap();
            let args = serde_json::json!({});
            for (key, conv) in [("done", gone.id), ("unk", gone.id), ("live", kept.id)] {
                dn_tools::ledger::claim(&l, key, "cap", &format!("conv:{conv}"), &args).unwrap();
            }
            dn_tools::ledger::succeed(&l, "done", &serde_json::json!("ok")).unwrap();
            dn_tools::ledger::mark_unknown(&l, "unk", "cancelled").unwrap();
            dn_tools::ledger::succeed(&l, "live", &serde_json::json!("ok")).unwrap();
            // Purged by an older build: tombstone present, ledger rows left behind.
            let old = Id::new();
            c.execute(
                "INSERT INTO store_tombstones (id, deleted_at) VALUES (?1, 1)",
                [old.to_string()],
            )
            .unwrap();
            dn_tools::ledger::claim(&l, "old", "cap", &format!("conv:{old}"), &args).unwrap();
            dn_tools::ledger::succeed(&l, "old", &serde_json::json!("ok")).unwrap();
            (gone, kept)
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let st = AppState::open(&path, None).unwrap();
            let exec = st.executor.as_ref().unwrap();
            assert!(exec.ledger_entry("done").unwrap().is_none());
            assert!(
                exec.ledger_entry("old").unwrap().is_none(),
                "older purge cleaned up"
            );
            assert!(exec.ledger_entry("live").unwrap().is_some());
            // The unknown write waits for the user, then goes once resolved.
            assert!(exec.ledger_entry("unk").unwrap().is_some());
            {
                let db = st.db().unwrap();
                assert!(store::is_purged(&db, &gone.id.to_string()).unwrap());
                assert!(store::get_conversation(&db, kept.id).is_ok());
                let arts = dn_artifacts::ArtifactStore::new(&db);
                assert!(
                    arts.list(gone.id).unwrap().is_empty(),
                    "a purged chat's artifacts go"
                );
                assert_eq!(arts.list(kept.id).unwrap().len(), 1);
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
    fn open_purges_deleted_projects_with_their_chats_and_file_copies() {
        use dn_store::project;
        let dir = std::env::temp_dir().join(format!("dn-app-projects-{}", Id::new()));
        let path = dir.join("db.sqlite3");
        let root = dir.join(crate::projects::FILES_DIR);
        let src = dir.join("notes.md");
        let (gone, kept, gone_chat, gone_file, kept_file) = {
            let c = dn_store::open(&path, &migration_sets()).unwrap();
            std::fs::write(&src, b"secret notes").unwrap();
            let gone = project::create_project(&c, "private project").unwrap();
            let kept = project::create_project(&c, "kept").unwrap();
            let chat = store::create_conversation_in(&c, "in project", Some(gone.id)).unwrap();
            let gf = crate::projects::import(&c, &root, gone.id, &src).unwrap();
            let kf = crate::projects::import(&c, &root, kept.id, &src).unwrap();
            project::delete_project(&c, gone.id).unwrap();
            (gone, kept, chat, gf, kf)
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let st = AppState::open(&path, None).unwrap();
            assert_eq!(st.project_files_root(), Some(root.as_path()));
            let db = st.db().unwrap();
            assert!(store::is_purged(&db, &gone_chat.id.to_string()).unwrap());
            assert_eq!(project::project_tombstones(&db).unwrap()[0].0, gone.id);
            assert_eq!(project::file_tombstones(&db).unwrap()[0].0, gone_file.id);
            assert_eq!(project::list_projects(&db).unwrap()[0].project.id, kept.id);
            assert!(!project::file_path(&root, &gone_file).exists());
            assert!(project::file_path(&root, &kept_file).exists());
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keychain_list_says_when_the_keychain_failed_instead_of_none_saved() {
        use crate::secrets::{SecretError, tests::failing};
        for (err, word) in [
            (
                (|| SecretError::Unavailable("no secret service".into())) as fn() -> SecretError,
                "keychain",
            ),
            (|| SecretError::Denied("locked".into()), "wasn't allowed"),
        ] {
            let conn = dn_store::open_in_memory(&migration_sets()).unwrap();
            let st = AppState::new(conn, ModelRegistry::default(), failing(err));
            let e = st.keychain_entries();
            assert!(e.entries.is_empty());
            assert!(e.problem.expect("says why").contains(word));
        }
        let conn = dn_store::open_in_memory(&migration_sets()).unwrap();
        let st = AppState::new(conn, ModelRegistry::default(), Keys::memory());
        assert_eq!(st.keychain_entries(), KeychainEntries::default());
    }

    #[test]
    fn removed_servers_wait_for_undo_and_delete_all_drops_them_and_their_keys() {
        let keys = Keys::memory();
        let conn = dn_store::open_in_memory(&migration_sets()).unwrap();
        let st = AppState::new(conn, ModelRegistry::default(), keys.clone());
        let server = settings::RemoteServer {
            id: "box".into(),
            name: "Box".into(),
            base_url: "http://localhost:11434/v1".into(),
            family: Default::default(),
            models: vec![],
        };
        st.remember_removed_server(
            server.clone(),
            Some(Secret::new("k")),
            vec![("chat".into(), "remote:box:m".into())],
        );
        let r = st.take_removed_server("box").expect("within the window");
        assert_eq!(r.server, server);
        assert_eq!(r.key.unwrap().expose(), "k");
        assert!(st.take_removed_server("box").is_none(), "taken once");
        // A saved server's key is deleted by Delete all local data, and so is a pending undo.
        keys.set(&server.key_id(), &Secret::new("saved")).unwrap();
        st.save_settings(&Settings {
            remote_servers: vec![server.clone()],
            ..Default::default()
        })
        .unwrap();
        st.remember_removed_server(server.clone(), None, vec![]);
        assert_eq!(st.keychain_entries().entries, ["Box server key"]);
        assert!(st.wipe_local_data().is_empty());
        assert!(!keys.has(&server.key_id()).unwrap());
        assert!(st.take_removed_server("box").is_none());
        assert!(st.settings().unwrap().remote_servers.is_empty());
    }

    #[test]
    fn wipe_local_data_removes_projects_and_their_files() {
        use dn_store::project;
        let dir = std::env::temp_dir().join(format!("dn-app-wipe-projects-{}", Id::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.txt");
        std::fs::write(&src, b"a").unwrap();
        let root = dir.join(crate::projects::FILES_DIR);
        let conn = dn_store::open_in_memory(&migration_sets()).unwrap();
        let mut st = AppState::new(conn, ModelRegistry::default(), Keys::memory());
        st.project_files = Some(root.clone());
        {
            let db = st.db().unwrap();
            let p = project::create_project(&db, "p").unwrap();
            crate::projects::import(&db, &root, p.id, &src).unwrap();
        }
        assert!(st.wipe_local_data().is_empty());
        assert!(!root.exists());
        assert!(
            project::list_projects(&st.db().unwrap())
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wipe_local_data_clears_chats_settings_ledger_and_keys() {
        let conn = dn_store::open_in_memory(&migration_sets()).unwrap();
        let keys = Keys::memory();
        let mut st = AppState::new(conn, ModelRegistry::default(), keys.clone());
        // Tests run local (stdio) servers, also in a `--features flatpak` build.
        st.local_servers = true;
        let ledger = dn_store::open_in_memory(&migration_sets()).unwrap();
        let (gate, _approvals) = dn_tools::ApprovalGate::new(1);
        let exec = Arc::new(dn_tools::Executor::new(
            Arc::new(gate),
            Arc::new(Mutex::new(ledger)),
        ));
        st.executor = Some(Arc::clone(&exec));

        let c = st.create_conversation().unwrap();
        dn_artifacts::ArtifactStore::new(&st.db().unwrap())
            .create(
                c.id,
                &dn_core::artifact::ArtifactType::Html,
                "Page",
                "<p>x</p>",
                &dn_artifacts::OpContext::model(None),
            )
            .unwrap();
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
            st.keychain_entries().entries,
            [
                "OpenAI key",
                "Hugging Face token",
                "1 tool-server secret or sign-in"
            ]
        );
        {
            let db = st.db().unwrap();
            dn_knowledge::Index::new(&db)
                .sync_source(
                    "project",
                    &[dn_knowledge::SourceFile {
                        file_id: "f".into(),
                        display_path: "notes.md".into(),
                        content: b"private project notes".to_vec(),
                    }],
                    None,
                )
                .unwrap();
            dn_knowledge::record_sources(
                &db,
                "m",
                &c.id.to_string(),
                &dn_knowledge::Sources {
                    mode: dn_knowledge::SearchMode::Keyword,
                    citations: vec![],
                },
            )
            .unwrap();
        }
        assert!(st.wipe_local_data().is_empty());
        {
            let db = st.db().unwrap();
            assert_eq!(
                dn_knowledge::Index::new(&db).counts().unwrap(),
                dn_knowledge::Counts::default(),
                "the knowledge index is deleted too"
            );
            assert!(dn_knowledge::sources_for(&db, "m").unwrap().is_none());
        }
        assert!(st.keychain_entries().entries.is_empty());
        assert!(
            store::list_conversations(&st.db().unwrap())
                .unwrap()
                .is_empty()
        );
        assert!(store::get_conversation(&st.db().unwrap(), c.id).is_err());
        assert_eq!(
            dn_artifacts::ArtifactStore::new(&st.db().unwrap())
                .counts()
                .unwrap(),
            (0, 0),
            "artifacts are deleted too"
        );
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

    fn state_with_executor() -> (Arc<AppState>, Arc<dn_tools::Executor>, Keys) {
        let conn = dn_store::open_in_memory(&migration_sets()).unwrap();
        let keys = Keys::memory();
        let mut st = AppState::new(conn, ModelRegistry::default(), keys.clone());
        // Tests run local (stdio) servers, also in a `--features flatpak` build.
        st.local_servers = true;
        let ledger = dn_store::open_in_memory(&migration_sets()).unwrap();
        let (gate, _approvals) = dn_tools::ApprovalGate::new(1);
        let exec = Arc::new(dn_tools::Executor::new(
            Arc::new(gate),
            Arc::new(Mutex::new(ledger)),
        ));
        st.executor = Some(Arc::clone(&exec));
        (Arc::new(st), exec, keys)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_all_removes_mcp_servers_before_wiping_keys() {
        let (st, exec, _keys) = state_with_executor();
        // Unreachable server: add_server fails to connect but registers the client.
        let cfg = dn_tools::McpServerConfig::http("gh", "http://127.0.0.1:9/mcp");
        let client = dn_tools::McpClient::new(cfg, st.mcp_secret_store()).unwrap();
        let _ = exec.add_server(client).await;
        assert!(exec.client("gh").is_some());
        st.set_mcp_status(McpServerStatus {
            server_id: "gh".into(),
            name: "gh".into(),
            state: "failed",
            message: None,
            tools: 0,
            review: 0,
            revision: 0,
        });

        assert!(st.delete_all_local_data().await.is_empty());
        assert!(exec.client("gh").is_none(), "server removed and stopped");
        assert!(lock(&st.mcp_status).is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_all_waits_for_running_replies() {
        let (st, _exec, _keys) = state_with_executor();
        let c = st.create_conversation().unwrap();
        let r = st.reserve(c.id).unwrap();
        let (token, cancel) = r.into_parts();
        let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (st2, ended2) = (Arc::clone(&st), Arc::clone(&ended));
        tokio::spawn(async move {
            // A reply that takes a moment to notice the cancel and store its text.
            while !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            ended2.store(true, Ordering::SeqCst);
            st2.release(c.id, token);
        });
        assert!(st.delete_all_local_data().await.is_empty());
        assert!(
            ended.load(Ordering::SeqCst),
            "wiped only after the reply ended"
        );
    }

    #[test]
    fn removed_and_orphaned_mcp_secrets_are_deleted() {
        let (st, _exec, keys) = state_with_executor();
        let store = st.mcp_secret_store();
        let server = |id: &str| -> dn_tools::McpServerConfig {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "transport": {
                    "type": "streamable_http",
                    "url": "https://example.test/mcp",
                    "oauth": { "credential_secret_id": format!("mcp.{id}.oauth") }
                }
            }))
            .unwrap()
        };
        st.set_mcp_servers(vec![server("a"), server("b")]).unwrap();
        // dn-tools writes sign-ins through the tracked store.
        store.set("mcp.a.oauth", "{}").unwrap();
        store.set("mcp.b.oauth", "{}").unwrap();
        // An id no saved server names any more (e.g. from an older build's server list).
        store.set("mcp.old.token", "x").unwrap();
        let has = |id: &str| {
            keys.store
                .get(&dn_secrets::SecretId::mcp(id))
                .unwrap()
                .is_some()
        };

        // Removing server b deletes its sign-in only.
        st.set_mcp_servers(vec![server("a")]).unwrap();
        assert!(has("mcp.a.oauth"));
        assert!(!has("mcp.b.oauth"));

        assert_eq!(
            st.keychain_entries().entries,
            ["2 tool-server secrets or sign-ins"]
        );
        assert!(st.wipe_local_data().is_empty());
        assert!(!has("mcp.a.oauth"));
        assert!(
            !has("mcp.old.token"),
            "orphaned secret found via the registry"
        );
    }

    #[test]
    fn an_unreadable_settings_row_is_reported() {
        let (st, _exec, _keys) = state_with_executor();
        st.db()
            .unwrap()
            .execute(
                "INSERT INTO app_settings (key, value, updated_at) VALUES ('settings', 'not json', 0)",
                [],
            )
            .unwrap();
        let failed = st.wipe_local_data();
        assert!(
            failed.iter().any(|f| f.contains("settings unreadable")),
            "{failed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_a_wipe_old_mcp_connects_and_token_writes_are_refused() {
        let (st, exec, keys) = state_with_executor();
        let epoch = st.mcp_epoch.load(Ordering::SeqCst);
        let store = st.mcp_secret_store();
        assert!(st.delete_all_local_data().await.is_empty());

        // A token refresh from a server that was still shutting down.
        assert!(store.set("mcp.gh.oauth", "{}").is_err());
        assert!(
            keys.store
                .get(&dn_secrets::SecretId::mcp("mcp.gh.oauth"))
                .unwrap()
                .is_none()
        );
        // A store made after the wipe works again.
        st.mcp_secret_store().set("mcp.new.token", "x").unwrap();

        // A connect that started before the wipe doesn't add its server.
        let cfg = dn_tools::McpServerConfig::http("gh", "http://127.0.0.1:9/mcp");
        let client = dn_tools::McpClient::new(cfg, st.mcp_secret_store()).unwrap();
        assert!(
            st.add_mcp_server_if_current(&exec, client, epoch)
                .await
                .is_none()
        );
        assert!(exec.client("gh").is_none());
        // With the current epoch it is added (connect fails, the server is registered).
        let cfg = dn_tools::McpServerConfig::http("gh", "http://127.0.0.1:9/mcp");
        let client = dn_tools::McpClient::new(cfg, st.mcp_secret_store()).unwrap();
        let now = st.mcp_epoch.load(Ordering::SeqCst);
        assert!(
            st.add_mcp_server_if_current(&exec, client, now)
                .await
                .is_some()
        );
        assert!(exec.client("gh").is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reply_that_does_not_stop_is_reported() {
        let (st, _exec, _keys) = state_with_executor();
        let c = st.create_conversation().unwrap();
        // Reserved and never released: the reply ignores the cancel.
        let (_token, _cancel) = st.reserve(c.id).unwrap().into_parts();
        let failed = st.delete_all_local_data().await;
        assert!(
            failed.iter().any(|f| f.contains("still being written")),
            "{failed:?}"
        );
    }
}

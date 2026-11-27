//! The `Host`: the only way an add-on reaches anything (ADR-0013 §4).
//!
//! [`Host`] is a concrete type. Every method checks, on every call, that:
//!
//! 1. the handle hasn't been revoked ("Delete all local data" revokes every handle first);
//! 2. the add-on still holds the permission the method needs (the user can turn any
//!    permission off at any time, and the next call fails with a clear error, never with
//!    empty data);
//! 3. the call fits the handler's access, which follows the tier the handler (tool or job)
//!    declared (ADR-0013 §9.1):
//!    - `read`: reads its own data only, no `http()` (not even GET), tools up to `read`;
//!    - `low_write`: reads and writes its own data, no `http()`, tools up to `low_write`;
//!    - `external_write` and above: also `http()` to listed hosts (from a job, anything but
//!      GET is held), tools up to the handler's tier;
//!
//!    and the `Host` passed to `Plugin::delete_all` can only delete the add-on's own data.
//!
//! Only then does it ask the [`HostBackend`], which the app (or a Demios Node) implements.
//! Results that carry content come back as [`Untrusted`]. The backend gets the [`Caller`]
//! the host built for the call, so it can apply what only it can see: the approval gate, the
//! target tool's tier, the chats in scope, and holding a background job's calls for the user.

use crate::data::{
    ChatMessage, ConversationSummary, HeldCall, HttpMethod, HttpRequest, HttpResponse,
    KnowledgeHit, MemoryItem, ModelPrompt, ModelSlot, NewMemory, Row, SqlValue, ToolResult,
};
use crate::error::{ErrorKind, PluginError};
use crate::id::{PluginId, check_name};
use crate::manifest::{Permission, PluginManifest, check_host};
use crate::pattern::matches_any;
use crate::{BoxFuture, Id, RiskTier, Untrusted};
use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

/// The longest path in an add-on's file area, in bytes.
pub const MAX_PATH_LEN: usize = 255;
/// The deepest path in an add-on's file area, in folders.
pub const MAX_PATH_DEPTH: usize = 8;

/// What a `Host` may do, set by the host from the tier the handler declared (ADR-0013
/// §9.1). It follows the declared tier, never the user's approval choice: a user who makes a
/// `read` tool ask every time makes it ask more often, and its handler still can't write.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// A `read`-tier handler: reads its own data only, no `http()`, tools up to `read`.
    ReadOnly,
    /// A `low_write`-tier handler: reads and writes its own data (memory, settings, files,
    /// database), no `http()`, tools up to `low_write`.
    LowWrite,
    /// An `external_write`-tier handler or above, or a job declared without a tier: also
    /// `http()` to the hosts the add-on listed, and tools up to its tier.
    ReadWrite,
    /// `Plugin::delete_all`: read and delete the add-on's own data, nothing else.
    DeleteOnly,
}

impl Access {
    /// The access a handler (tool or job) of tier `t` gets.
    pub fn for_tier(t: RiskTier) -> Self {
        match t {
            RiskTier::Read => Self::ReadOnly,
            RiskTier::LowWrite => Self::LowWrite,
            _ => Self::ReadWrite,
        }
    }

    /// True when `http()` is allowed at all (`external_write` and above).
    pub fn may_reach_network(self) -> bool {
        self == Self::ReadWrite
    }
}

/// Where a call comes from.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// One of the add-on's tools, run by the executor.
    Tool {
        /// The tool's capability id (`plugin.<id>.<name>`).
        capability_id: String,
    },
    /// A background job.
    Job {
        /// The job's id.
        job_id: String,
    },
    /// `Plugin::forget_conversation` or `Plugin::delete_all`.
    Deletion,
}

/// Who is calling, built by the host for each run. The add-on can read it through
/// [`Host::caller`] but never builds or changes the one it is given.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The add-on.
    pub plugin_id: PluginId,
    /// Its name, for messages and approval prompts.
    pub plugin_name: String,
    /// This run. Approval grants are kept per run ([`Caller::approval_scope`]).
    pub run: Id,
    /// Where the call comes from.
    pub origin: Origin,
    /// What the `Host` may do.
    pub access: Access,
    /// The highest tier of tool this run may call through [`Host::tools`]: the handler's own
    /// tier, so a handler can't reach further through another tool than it could itself.
    pub max_tool_tier: RiskTier,
    /// Allowlists inherited from the run that called this one (for example the chat skill's
    /// `allowed-tools`). A tool call must match every list; a run can add lists, never
    /// remove one.
    pub allowed_tools: Vec<Vec<String>>,
}

impl Caller {
    /// The caller for one of the add-on's tools of tier `tier`, run from a chat whose skill
    /// allows `inherited` (empty when there is no narrowing).
    pub fn for_tool(
        manifest: &PluginManifest,
        capability_id: impl Into<String>,
        tier: RiskTier,
        run: Id,
        inherited: Vec<Vec<String>>,
    ) -> Self {
        Self {
            plugin_id: manifest.id.clone(),
            plugin_name: manifest.name.clone(),
            run,
            origin: Origin::Tool {
                capability_id: capability_id.into(),
            },
            access: Access::for_tier(tier),
            max_tool_tier: tier,
            allowed_tools: inherited,
        }
    }

    /// The caller for one run of a background job declared without a tier: today's access
    /// (`ReadWrite`, tools up to `sensitive`). Kept for compatibility within API major 1
    /// (ADR-0013 §9.1, #300); a host should use [`Caller::for_job_at`] with the job's
    /// [`JobDecl::tier`](crate::JobDecl::tier).
    pub fn for_job(manifest: &PluginManifest, job_id: impl Into<String>, run: Id) -> Self {
        Self::for_job_at(manifest, job_id, run, None)
    }

    /// The caller for one run of a background job of tier `tier` (from
    /// [`JobDecl::tier`](crate::JobDecl::tier)). Its `Host` is limited like a tool handler's
    /// of that tier. `None`, a job declared without a tier, keeps today's access: `ReadWrite`
    /// with tools up to `sensitive` (until the next major version, #300).
    pub fn for_job_at(
        manifest: &PluginManifest,
        job_id: impl Into<String>,
        run: Id,
        tier: Option<RiskTier>,
    ) -> Self {
        let (access, max_tool_tier) = match tier {
            Some(t) => (Access::for_tier(t), t),
            None => (Access::ReadWrite, RiskTier::Sensitive),
        };
        Self {
            plugin_id: manifest.id.clone(),
            plugin_name: manifest.name.clone(),
            run,
            origin: Origin::Job {
                job_id: job_id.into(),
            },
            access,
            max_tool_tier,
            allowed_tools: Vec::new(),
        }
    }

    /// The caller for `forget_conversation` and `delete_all`.
    pub fn for_deletion(manifest: &PluginManifest, run: Id) -> Self {
        Self {
            plugin_id: manifest.id.clone(),
            plugin_name: manifest.name.clone(),
            run,
            origin: Origin::Deletion,
            access: Access::DeleteOnly,
            max_tool_tier: RiskTier::Read,
            allowed_tools: Vec::new(),
        }
    }

    /// The approval scope of this run, `plugin:<id>:<run>`. It never equals a chat's scope,
    /// so "allow for this chat" grants aren't shared with chats.
    pub fn approval_scope(&self) -> String {
        self.plugin_id.approval_scope(self.run)
    }

    /// True when the call comes from a background job.
    pub fn is_job(&self) -> bool {
        matches!(self.origin, Origin::Job { .. })
    }

    /// True when every inherited allowlist allows `capability_id`.
    pub fn narrowing_allows(&self, capability_id: &str) -> bool {
        self.allowed_tools
            .iter()
            .all(|l| matches_any(l, capability_id))
    }
}

/// Which chats `chats.read` covers, as the user chose.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatScope {
    /// Every chat.
    All,
    /// The chats in these projects.
    Projects(Vec<Id>),
}

/// What the user agreed to on the consent screen.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consent {
    /// The permissions the user allowed.
    pub permissions: BTreeSet<Permission>,
    /// The chats `chats.read` covers.
    pub chats: ChatScope,
    /// The projects `knowledge.search` covers.
    pub knowledge_projects: Vec<Id>,
}

impl Consent {
    /// Consent to `permissions`, with no chats and no projects chosen yet.
    pub fn new(permissions: impl IntoIterator<Item = Permission>) -> Self {
        Self {
            permissions: permissions.into_iter().collect(),
            chats: ChatScope::Projects(Vec::new()),
            knowledge_projects: Vec::new(),
        }
    }

    /// Sets the chats `chats.read` covers.
    pub fn with_chats(mut self, chats: ChatScope) -> Self {
        self.chats = chats;
        self
    }

    /// Sets the projects `knowledge.search` covers.
    pub fn with_knowledge_projects(mut self, projects: Vec<Id>) -> Self {
        self.knowledge_projects = projects;
        self
    }
}

/// The permissions an add-on holds: what it asked for and the user allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grants {
    allowed: BTreeSet<Permission>,
    chats: ChatScope,
    knowledge_projects: Vec<Id>,
    tools: Vec<String>,
    hosts: Vec<String>,
}

impl Grants {
    /// The grants for `manifest` under `consent`. Refuses consent to a permission the
    /// manifest doesn't ask for.
    pub fn new(manifest: &PluginManifest, consent: Consent) -> Result<Self, PluginError> {
        if let Some(p) = consent.permissions.iter().find(|p| !manifest.asks_for(**p)) {
            return Err(PluginError::invalid(format!(
                "{} doesn't ask for `{}`, so it can't be granted",
                manifest.name,
                p.as_str()
            )));
        }
        Ok(Self {
            allowed: consent.permissions,
            chats: consent.chats,
            knowledge_projects: consent.knowledge_projects,
            tools: manifest.tool_patterns().to_vec(),
            hosts: manifest.network_hosts().to_vec(),
        })
    }

    /// No permissions.
    pub fn none() -> Self {
        Self {
            allowed: BTreeSet::new(),
            chats: ChatScope::Projects(Vec::new()),
            knowledge_projects: Vec::new(),
            tools: Vec::new(),
            hosts: Vec::new(),
        }
    }

    /// True when `p` was allowed.
    pub fn allows(&self, p: Permission) -> bool {
        self.allowed.contains(&p)
    }
}

#[derive(Debug)]
struct GrantState {
    grants: Grants,
    revoked: BTreeSet<Permission>,
    closed: bool,
}

/// The live grants of one add-on, shared by every `Host` the host hands it. Turning a
/// permission off, or closing the handle, takes effect on the next call of every `Host`
/// that shares it.
#[derive(Clone, Debug)]
pub struct GrantsHandle(Arc<RwLock<GrantState>>);

impl GrantsHandle {
    /// A handle holding `grants`.
    pub fn new(grants: Grants) -> Self {
        Self(Arc::new(RwLock::new(GrantState {
            grants,
            revoked: BTreeSet::new(),
            closed: false,
        })))
    }

    /// Turns `p` off. Calls that need it fail with [`ErrorKind::PermissionDenied`].
    pub fn revoke(&self, p: Permission) {
        if let Ok(mut s) = self.0.write() {
            s.revoked.insert(p);
        }
    }

    /// Turns `p` back on, if it was granted.
    pub fn restore(&self, p: Permission) {
        if let Ok(mut s) = self.0.write() {
            s.revoked.remove(&p);
        }
    }

    /// Replaces the grants, for example after the user consented to a new version.
    pub fn replace(&self, grants: Grants) {
        if let Ok(mut s) = self.0.write() {
            s.grants = grants;
            s.revoked.clear();
        }
    }

    /// Revokes every `Host` that shares this handle, for good. Every later call fails with
    /// [`ErrorKind::Revoked`].
    pub fn close(&self) {
        match self.0.write() {
            Ok(mut s) => s.closed = true,
            Err(p) => p.into_inner().closed = true,
        }
    }

    /// True after [`GrantsHandle::close`].
    pub fn is_closed(&self) -> bool {
        self.0.read().map_or(true, |s| s.closed)
    }

    fn with<T>(&self, f: impl FnOnce(&GrantState) -> T) -> Result<T, PluginError> {
        let s = self
            .0
            .read()
            .map_err(|_| PluginError::new(ErrorKind::Revoked, "The add-on's access is broken"))?;
        Ok(f(&s))
    }
}

/// The app's side of the boundary, implemented by the desktop app and by a Demios Node.
///
/// [`Host`] calls it only after its own checks have passed. Every method gets the
/// [`Caller`]; the notes on each say what the backend still has to apply, because only it
/// can see it.
pub trait HostBackend: Send + Sync {
    /// The conversations in `scope`, newest first.
    fn list_conversations<'a>(
        &'a self,
        caller: &'a Caller,
        scope: &'a ChatScope,
    ) -> BoxFuture<'a, Result<Vec<ConversationSummary>, PluginError>>;

    /// The messages of `conversation`, oldest first. Must refuse a conversation outside
    /// `scope` with [`ErrorKind::NotAllowed`].
    fn read_messages<'a>(
        &'a self,
        caller: &'a Caller,
        scope: &'a ChatScope,
        conversation: Id,
    ) -> BoxFuture<'a, Result<Vec<ChatMessage>, PluginError>>;

    /// The add-on's memory items.
    fn memory_list<'a>(
        &'a self,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<Vec<MemoryItem>, PluginError>>;

    /// Adds a memory item and returns its id.
    fn memory_add<'a>(
        &'a self,
        caller: &'a Caller,
        item: NewMemory,
    ) -> BoxFuture<'a, Result<Id, PluginError>>;

    /// Changes a memory item's text. Must refuse an item of another add-on.
    fn memory_update<'a>(
        &'a self,
        caller: &'a Caller,
        id: Id,
        text: String,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Deletes a memory item. Must refuse an item of another add-on.
    fn memory_delete<'a>(
        &'a self,
        caller: &'a Caller,
        id: Id,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Deletes all of the add-on's memory items.
    fn memory_clear<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Searches the local index in `projects`.
    fn knowledge_search<'a>(
        &'a self,
        caller: &'a Caller,
        projects: &'a [Id],
        query: String,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<KnowledgeHit>, PluginError>>;

    /// Runs `prompt` on the model the user chose for `slot`, through the app's model
    /// resolution (no silent fallback to another model), rendering each quoted block as
    /// cited data.
    fn run_model<'a>(
        &'a self,
        caller: &'a Caller,
        slot: ModelSlot,
        prompt: ModelPrompt,
    ) -> BoxFuture<'a, Result<String, PluginError>>;

    /// Calls a tool through the executor, so the approval gate always runs. The backend must
    /// use [`Caller::approval_scope`] for grants, refuse a tool above
    /// [`Caller::max_tool_tier`], keep [`Caller::allowed_tools`] for any nested call, and
    /// name the add-on (and the job) in the approval prompt. For a job, only tools the user
    /// set to run without asking for this add-on may run at once; every other call is held
    /// for the user ([`ErrorKind::Held`]).
    fn call_tool<'a>(
        &'a self,
        caller: &'a Caller,
        capability_id: String,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<ToolResult, PluginError>>;

    /// Shows an OS notification (on a Node, on the user's paired devices).
    fn notify<'a>(
        &'a self,
        caller: &'a Caller,
        title: String,
        body: String,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// A value in the add-on's settings namespace.
    fn setting_get<'a>(
        &'a self,
        caller: &'a Caller,
        key: String,
    ) -> BoxFuture<'a, Result<Option<serde_json::Value>, PluginError>>;

    /// Stores a value in the add-on's settings namespace.
    fn setting_set<'a>(
        &'a self,
        caller: &'a Caller,
        key: String,
        value: serde_json::Value,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Removes a value from the add-on's settings namespace.
    fn setting_remove<'a>(
        &'a self,
        caller: &'a Caller,
        key: String,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// One of the add-on's secrets, from the keychain under an id the host owns for it.
    fn secret_get<'a>(
        &'a self,
        caller: &'a Caller,
        name: String,
    ) -> BoxFuture<'a, Result<Option<String>, PluginError>>;

    /// Stores one of the add-on's secrets in the keychain.
    fn secret_set<'a>(
        &'a self,
        caller: &'a Caller,
        name: String,
        value: String,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Deletes one of the add-on's secrets.
    fn secret_delete<'a>(
        &'a self,
        caller: &'a Caller,
        name: String,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Sends a request through the app's HTTP client, honouring the user's proxy settings.
    /// The host has checked the URL is `https://` on a listed host.
    fn http<'a>(
        &'a self,
        caller: &'a Caller,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, PluginError>>;

    /// Puts a background job's request (anything but GET) in the held-approvals queue, with
    /// the add-on, the job and the request, for the user to answer. It is never approved on
    /// its own and doesn't time out into a silent denial.
    fn hold_http<'a>(
        &'a self,
        caller: &'a Caller,
        request: HttpRequest,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// The calls of `caller`'s add-on still waiting in the held queue (ADR-0013 §9.2), and
    /// only those: the queue records which add-on put each call there. A host without a
    /// held queue keeps this default.
    fn held_list<'a>(
        &'a self,
        _caller: &'a Caller,
    ) -> BoxFuture<'a, Result<Vec<HeldCall>, PluginError>> {
        Box::pin(async { Err(no_held_queue()) })
    }

    /// Withdraws the add-on's held call `id`: it never runs, and its history says it was
    /// withdrawn by the add-on. Refused for another add-on's call, and for a call that is
    /// no longer held (approved, running or finished).
    fn held_withdraw<'a>(
        &'a self,
        _caller: &'a Caller,
        _id: Id,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async { Err(no_held_queue()) })
    }

    /// Replaces the add-on's held call `id` with the same call and new `arguments`, in one
    /// step: the old one is withdrawn and the new one held, linked to it. The host refuses
    /// it when the tool is above what `caller` may call. Returns the new call.
    fn held_replace<'a>(
        &'a self,
        _caller: &'a Caller,
        _id: Id,
        _arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<HeldCall, PluginError>> {
        Box::pin(async { Err(no_held_queue()) })
    }

    /// Reads a file in the add-on's area. `path` is checked ([`check_path`]).
    fn file_read<'a>(
        &'a self,
        caller: &'a Caller,
        path: String,
    ) -> BoxFuture<'a, Result<Vec<u8>, PluginError>>;

    /// Writes a file in the add-on's area, creating folders as needed.
    fn file_write<'a>(
        &'a self,
        caller: &'a Caller,
        path: String,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// The files in a folder of the add-on's area (`""` for the top), as paths.
    fn file_list<'a>(
        &'a self,
        caller: &'a Caller,
        folder: String,
    ) -> BoxFuture<'a, Result<Vec<String>, PluginError>>;

    /// Deletes a file or folder in the add-on's area.
    fn file_delete<'a>(
        &'a self,
        caller: &'a Caller,
        path: String,
    ) -> BoxFuture<'a, Result<(), PluginError>>;

    /// Runs a statement on the add-on's own database file and returns the rows changed.
    /// The connection's authorizer refuses `ATTACH` and other schemas. When
    /// [`Caller::access`] is [`Access::DeleteOnly`], it must also refuse anything but
    /// `DELETE` (and `DROP`), so deletion can't write.
    fn db_execute<'a>(
        &'a self,
        caller: &'a Caller,
        sql: String,
        params: Vec<SqlValue>,
    ) -> BoxFuture<'a, Result<u64, PluginError>>;

    /// Runs a query on the add-on's own database file. Must refuse a statement that isn't
    /// read-only.
    fn db_query<'a>(
        &'a self,
        caller: &'a Caller,
        sql: String,
        params: Vec<SqlValue>,
    ) -> BoxFuture<'a, Result<Vec<Row>, PluginError>>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Read the add-on's own data.
    ReadOwn,
    /// Write the add-on's own data.
    WriteOwn,
    /// Delete the add-on's own data.
    DeleteOwn,
    /// Anything that reaches beyond the add-on's own data (chats, search, model, tools,
    /// notifications, network).
    Reach,
}

/// The services an add-on may use. See the module documentation for what each call checks.
#[derive(Clone)]
pub struct Host {
    backend: Arc<dyn HostBackend>,
    grants: GrantsHandle,
    caller: Caller,
}

impl Host {
    /// A `Host` for one run, built by the host.
    pub fn new(backend: Arc<dyn HostBackend>, grants: GrantsHandle, caller: Caller) -> Self {
        Self {
            backend,
            grants,
            caller,
        }
    }

    /// Who this `Host` acts for.
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// The checks every call makes, in order: revoked handle, access, permission.
    fn gate(&self, perm: Option<Permission>, op: Op) -> Result<(), PluginError> {
        let name = &self.caller.plugin_name;
        let state = self.grants.with(|s| {
            (
                s.closed,
                perm.map(|p| (s.grants.allows(p), s.revoked.contains(&p))),
            )
        })?;
        if state.0 {
            return Err(PluginError::new(
                ErrorKind::Revoked,
                format!("{name} can't do this now: its access was closed"),
            ));
        }
        match (self.caller.access, op) {
            (Access::ReadOnly, Op::WriteOwn | Op::DeleteOwn) => {
                return Err(PluginError::new(
                    ErrorKind::TierLimit,
                    format!("This part of {name} can only read, so it can't change anything"),
                ));
            }
            (Access::DeleteOnly, Op::WriteOwn | Op::Reach) => {
                return Err(PluginError::new(
                    ErrorKind::TierLimit,
                    format!("While its data is deleted, {name} can only delete its own data"),
                ));
            }
            _ => {}
        }
        // Deleting its own data never needs a permission: the user can always remove it.
        let deleting = self.caller.access == Access::DeleteOnly;
        if let (Some(p), Some((granted, revoked))) = (perm, state.1)
            && !deleting
        {
            if revoked {
                return Err(PluginError::new(
                    ErrorKind::PermissionDenied,
                    format!(
                        "{name} can't use `{}` any more: it was turned off in Settings › Add-ons",
                        p.as_str()
                    ),
                ));
            }
            if !granted {
                return Err(PluginError::new(
                    ErrorKind::PermissionDenied,
                    format!("{name} doesn't have the `{}` permission", p.as_str()),
                ));
            }
        }
        Ok(())
    }

    fn scope<T>(&self, f: impl FnOnce(&Grants) -> T) -> Result<T, PluginError> {
        self.grants.with(|s| f(&s.grants))
    }

    /// The add-on's own calls waiting for the user's answer (ADR-0013 §9.2). Like its own
    /// data, it needs no permission: it only ever covers calls this add-on made.
    pub fn held(&self) -> Held<'_> {
        Held(self)
    }

    /// Conversations and messages (`chats.read`).
    pub fn chats(&self) -> Chats<'_> {
        Chats(self)
    }

    /// The add-on's own memory (`memory`).
    pub fn memory(&self) -> Memory<'_> {
        Memory(self)
    }

    /// Project search (`knowledge.search`).
    pub fn knowledge(&self) -> Knowledge<'_> {
        Knowledge(self)
    }

    /// The user's chosen models.
    pub fn model(&self) -> Model<'_> {
        Model(self)
    }

    /// Tool calls through the executor.
    pub fn tools(&self) -> Tools<'_> {
        Tools(self)
    }

    /// OS notifications (`notify`).
    pub fn notifications(&self) -> Notifications<'_> {
        Notifications(self)
    }

    /// The add-on's settings namespace.
    pub fn settings(&self) -> Settings<'_> {
        Settings(self)
    }

    /// The add-on's secrets in the keychain.
    pub fn secrets(&self) -> Secrets<'_> {
        Secrets(self)
    }

    /// Outbound HTTPS to listed hosts (`network`).
    pub fn http(&self) -> Http<'_> {
        Http(self)
    }

    /// The add-on's own file area.
    pub fn files(&self) -> Files<'_> {
        Files(self)
    }

    /// The add-on's own database file.
    pub fn db(&self) -> Db<'_> {
        Db(self)
    }
}

/// See [`Host::chats`].
pub struct Chats<'h>(&'h Host);

impl Chats<'_> {
    /// The conversations the user let the add-on read, newest first.
    pub async fn list(&self) -> Result<Vec<Untrusted<ConversationSummary>>, PluginError> {
        let h = self.0;
        h.gate(Some(Permission::ChatsRead), Op::Reach)?;
        let scope = h.scope(|g| g.chats.clone())?;
        let v = h.backend.list_conversations(&h.caller, &scope).await?;
        Ok(v.into_iter().map(Untrusted::new).collect())
    }

    /// The messages of one conversation in scope, oldest first.
    pub async fn messages(
        &self,
        conversation: Id,
    ) -> Result<Vec<Untrusted<ChatMessage>>, PluginError> {
        let h = self.0;
        h.gate(Some(Permission::ChatsRead), Op::Reach)?;
        let scope = h.scope(|g| g.chats.clone())?;
        let v = h
            .backend
            .read_messages(&h.caller, &scope, conversation)
            .await?;
        Ok(v.into_iter().map(Untrusted::new).collect())
    }
}

/// See [`Host::memory`].
pub struct Memory<'h>(&'h Host);

impl Memory<'_> {
    /// Everything the add-on remembers.
    pub async fn list(&self) -> Result<Vec<Untrusted<MemoryItem>>, PluginError> {
        let h = self.0;
        h.gate(Some(Permission::Memory), Op::ReadOwn)?;
        let v = h.backend.memory_list(&h.caller).await?;
        Ok(v.into_iter().map(Untrusted::new).collect())
    }

    /// Remembers something. `sources` are the chats it came from.
    pub async fn add(&self, item: NewMemory) -> Result<Id, PluginError> {
        let h = self.0;
        h.gate(Some(Permission::Memory), Op::WriteOwn)?;
        if item.text.trim().is_empty() {
            return Err(PluginError::invalid("A memory item can't be empty"));
        }
        h.backend.memory_add(&h.caller, item).await
    }

    /// Changes what an item says.
    pub async fn update(&self, id: Id, text: impl Into<String>) -> Result<(), PluginError> {
        let h = self.0;
        h.gate(Some(Permission::Memory), Op::WriteOwn)?;
        h.backend.memory_update(&h.caller, id, text.into()).await
    }

    /// Forgets one item.
    pub async fn delete(&self, id: Id) -> Result<(), PluginError> {
        let h = self.0;
        h.gate(Some(Permission::Memory), Op::DeleteOwn)?;
        h.backend.memory_delete(&h.caller, id).await
    }

    /// Forgets everything.
    pub async fn clear(&self) -> Result<(), PluginError> {
        let h = self.0;
        h.gate(Some(Permission::Memory), Op::DeleteOwn)?;
        h.backend.memory_clear(&h.caller).await
    }
}

/// See [`Host::knowledge`].
pub struct Knowledge<'h>(&'h Host);

impl Knowledge<'_> {
    /// The best `limit` passages for `query`, in the projects the user chose.
    pub async fn search(
        &self,
        query: impl Into<String>,
        limit: u32,
    ) -> Result<Vec<Untrusted<KnowledgeHit>>, PluginError> {
        let h = self.0;
        h.gate(Some(Permission::KnowledgeSearch), Op::Reach)?;
        let projects = h.scope(|g| g.knowledge_projects.clone())?;
        let v = h
            .backend
            .knowledge_search(&h.caller, &projects, query.into(), limit.clamp(1, 50))
            .await?;
        Ok(v.into_iter().map(Untrusted::new).collect())
    }
}

/// See [`Host::model`].
pub struct Model<'h>(&'h Host);

impl Model<'_> {
    /// Runs `prompt` on the model the user chose for `slot`. The add-on never sees provider
    /// keys. The answer is untrusted: it may repeat what the quoted material said.
    pub async fn run(
        &self,
        slot: ModelSlot,
        prompt: ModelPrompt,
    ) -> Result<Untrusted<String>, PluginError> {
        let h = self.0;
        h.gate(None, Op::Reach)?;
        h.backend
            .run_model(&h.caller, slot, prompt)
            .await
            .map(Untrusted::new)
    }
}

/// See [`Host::tools`].
pub struct Tools<'h>(&'h Host);

impl Tools<'_> {
    /// Asks the executor to call a tool. The approval gate always runs, with this run's own
    /// scope; the prompt names the add-on.
    ///
    /// The add-on's own tools (`plugin.<id>.<name>`) need no permission. Any other tool
    /// needs `tools.call` and must match the manifest's list. Either way the tool must be
    /// allowed by every inherited allowlist, and its tier must be at most the caller's.
    pub async fn call(
        &self,
        capability_id: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Result<Untrusted<ToolResult>, PluginError> {
        let h = self.0;
        let id = capability_id.into();
        let own = id
            .strip_prefix(&format!("plugin.{}.", h.caller.plugin_id))
            .is_some_and(|n| check_name("tool name", n).is_ok());
        if own {
            h.gate(None, Op::Reach)?;
        } else {
            h.gate(Some(Permission::ToolsCall), Op::Reach)?;
            if !h.scope(|g| matches_any(&g.tools, &id))? {
                return Err(PluginError::not_allowed(format!(
                    "{} can't call `{id}`: it isn't in the tools the add-on listed",
                    h.caller.plugin_name
                )));
            }
        }
        if !h.caller.narrowing_allows(&id) {
            return Err(PluginError::not_allowed(format!(
                "`{id}` isn't available with the skill chosen for this chat"
            )));
        }
        h.backend
            .call_tool(&h.caller, id, arguments)
            .await
            .map(Untrusted::new)
    }
}

/// See [`Host::notifications`].
pub struct Notifications<'h>(&'h Host);

impl Notifications<'_> {
    /// Shows a notification. Title and body are shown as plain text.
    pub async fn send(
        &self,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<(), PluginError> {
        let h = self.0;
        h.gate(Some(Permission::Notify), Op::Reach)?;
        h.backend.notify(&h.caller, title.into(), body.into()).await
    }
}

/// See [`Host::settings`].
pub struct Settings<'h>(&'h Host);

impl Settings<'_> {
    /// The value stored under `key`, if any.
    pub async fn get(&self, key: &str) -> Result<Option<serde_json::Value>, PluginError> {
        let h = self.0;
        check_name("setting key", key)?;
        h.gate(None, Op::ReadOwn)?;
        h.backend.setting_get(&h.caller, key.to_owned()).await
    }

    /// Stores `value` under `key`.
    pub async fn set(&self, key: &str, value: serde_json::Value) -> Result<(), PluginError> {
        let h = self.0;
        check_name("setting key", key)?;
        h.gate(None, Op::WriteOwn)?;
        h.backend
            .setting_set(&h.caller, key.to_owned(), value)
            .await
    }

    /// Removes the value under `key`.
    pub async fn remove(&self, key: &str) -> Result<(), PluginError> {
        let h = self.0;
        check_name("setting key", key)?;
        h.gate(None, Op::DeleteOwn)?;
        h.backend.setting_remove(&h.caller, key.to_owned()).await
    }
}

/// See [`Host::secrets`].
pub struct Secrets<'h>(&'h Host);

impl Secrets<'_> {
    /// The secret stored under `name`, if any.
    pub async fn get(&self, name: &str) -> Result<Option<String>, PluginError> {
        let h = self.0;
        check_name("secret name", name)?;
        h.gate(None, Op::ReadOwn)?;
        h.backend.secret_get(&h.caller, name.to_owned()).await
    }

    /// Stores a secret under `name`.
    pub async fn set(&self, name: &str, value: impl Into<String>) -> Result<(), PluginError> {
        let h = self.0;
        check_name("secret name", name)?;
        h.gate(None, Op::WriteOwn)?;
        h.backend
            .secret_set(&h.caller, name.to_owned(), value.into())
            .await
    }

    /// Deletes the secret under `name`.
    pub async fn delete(&self, name: &str) -> Result<(), PluginError> {
        let h = self.0;
        check_name("secret name", name)?;
        h.gate(None, Op::DeleteOwn)?;
        h.backend.secret_delete(&h.caller, name.to_owned()).await
    }
}

fn no_held_queue() -> PluginError {
    PluginError::unavailable("Held calls aren't available in this version of Demios")
}

/// See [`Host::held`].
pub struct Held<'h>(&'h Host);

impl Held<'_> {
    /// The add-on's calls still waiting for the user's answer, oldest first. Only calls this
    /// add-on put in the queue.
    pub async fn list(&self) -> Result<Vec<HeldCall>, PluginError> {
        let h = self.0;
        h.gate(None, Op::ReadOwn)?;
        h.backend.held_list(&h.caller).await
    }

    /// Takes back the held call `id`, for example when the draft it would send was
    /// discarded. It never runs. Only while it is held: once the user answered, this fails.
    pub async fn withdraw(&self, id: Id) -> Result<(), PluginError> {
        let h = self.0;
        h.gate(None, Op::WriteOwn)?;
        h.backend.held_withdraw(&h.caller, id).await
    }

    /// Replaces the held call `id` with the same call and new `arguments`, for example
    /// after the draft it would send was edited. Every device then shows only the new
    /// version, with what changed. Only while it is held, and only for a tool this handler
    /// may call itself.
    pub async fn replace(
        &self,
        id: Id,
        arguments: serde_json::Value,
    ) -> Result<HeldCall, PluginError> {
        let h = self.0;
        h.gate(None, Op::WriteOwn)?;
        h.backend.held_replace(&h.caller, id, arguments).await
    }
}

/// See [`Host::http`].
pub struct Http<'h>(&'h Host);

impl Http<'_> {
    /// Sends `request` to a listed host. Only a handler of tier `external_write` or above
    /// may: below that, even GET is refused, since a GET can carry data out in its address
    /// (ADR-0013 §9.1). From a background job, anything but GET is held for the user and
    /// this returns [`ErrorKind::Held`].
    pub async fn send(&self, request: HttpRequest) -> Result<Untrusted<HttpResponse>, PluginError> {
        let h = self.0;
        h.gate(Some(Permission::Network), Op::Reach)?;
        if !h.caller.access.may_reach_network() {
            return Err(PluginError::new(
                ErrorKind::TierLimit,
                format!(
                    "This part of {} can't reach the network: only a tool or job of tier \
                     external_write or above may",
                    h.caller.plugin_name
                ),
            ));
        }
        let host = url_host(&request.url)?;
        if !h.scope(|g| g.hosts.contains(&host))? {
            return Err(PluginError::not_allowed(format!(
                "{} can't reach {host}: it isn't one of the hosts the add-on listed",
                h.caller.plugin_name
            )));
        }
        if request.method != HttpMethod::Get && h.caller.is_job() {
            h.backend.hold_http(&h.caller, request).await?;
            return Err(PluginError::new(
                ErrorKind::Held,
                format!("The request to {host} waits for your answer in Demios before it is sent"),
            ));
        }
        h.backend.http(&h.caller, request).await.map(Untrusted::new)
    }
}

/// The host of an `https://` URL, lowercased. Refuses other schemes, a user part and a port.
pub fn url_host(url: &str) -> Result<String, PluginError> {
    let rest = url
        .get(..8)
        .filter(|s| s.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])
        .ok_or_else(|| PluginError::invalid(format!("`{url}` isn't an https:// address")))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains(['@', ':', '\\']) {
        return Err(PluginError::invalid(format!(
            "`{url}` has a user name or a port; use the plain host"
        )));
    }
    let host = authority.to_ascii_lowercase();
    check_host(&host)?;
    Ok(host)
}

/// See [`Host::files`].
pub struct Files<'h>(&'h Host);

impl Files<'_> {
    /// Reads a file.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>, PluginError> {
        let h = self.0;
        check_path(path)?;
        h.gate(None, Op::ReadOwn)?;
        h.backend.file_read(&h.caller, path.to_owned()).await
    }

    /// Writes a file, replacing it if it exists.
    pub async fn write(&self, path: &str, bytes: Vec<u8>) -> Result<(), PluginError> {
        let h = self.0;
        check_path(path)?;
        h.gate(None, Op::WriteOwn)?;
        h.backend
            .file_write(&h.caller, path.to_owned(), bytes)
            .await
    }

    /// The files in `folder` (`""` for the top).
    pub async fn list(&self, folder: &str) -> Result<Vec<String>, PluginError> {
        let h = self.0;
        if !folder.is_empty() {
            check_path(folder)?;
        }
        h.gate(None, Op::ReadOwn)?;
        h.backend.file_list(&h.caller, folder.to_owned()).await
    }

    /// Deletes a file or folder.
    pub async fn delete(&self, path: &str) -> Result<(), PluginError> {
        let h = self.0;
        check_path(path)?;
        h.gate(None, Op::DeleteOwn)?;
        h.backend.file_delete(&h.caller, path.to_owned()).await
    }
}

/// Checks a path in an add-on's file area: relative, `/`-separated parts of letters, digits,
/// `.`, `_` and `-`, none of them `.` or `..`, at most [`MAX_PATH_DEPTH`] parts and
/// [`MAX_PATH_LEN`] bytes. The host hands out no real path, so an add-on can't name a file
/// outside its area.
pub fn check_path(path: &str) -> Result<(), PluginError> {
    let parts: Vec<&str> = path.split('/').collect();
    let ok = !path.is_empty()
        && path.len() <= MAX_PATH_LEN
        && parts.len() <= MAX_PATH_DEPTH
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
    if ok {
        Ok(())
    } else {
        Err(PluginError::invalid(format!(
            "`{path}` isn't a path in the add-on's area: use names of letters, digits, `.`, \
             `_` and `-` separated by `/`"
        )))
    }
}

/// See [`Host::db`].
pub struct Db<'h>(&'h Host);

impl Db<'_> {
    /// Runs a statement and returns the rows it changed. Needs write access (a `read`-tier
    /// handler can only query).
    pub async fn execute(&self, sql: &str, params: Vec<SqlValue>) -> Result<u64, PluginError> {
        let h = self.0;
        let op = if h.caller.access == Access::DeleteOnly {
            Op::DeleteOwn
        } else {
            Op::WriteOwn
        };
        h.gate(None, op)?;
        h.backend
            .db_execute(&h.caller, sql.to_owned(), params)
            .await
    }

    /// Runs a read-only query.
    pub async fn query(&self, sql: &str, params: Vec<SqlValue>) -> Result<Vec<Row>, PluginError> {
        let h = self.0;
        h.gate(None, Op::ReadOwn)?;
        h.backend.db_query(&h.caller, sql.to_owned(), params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_must_be_https_on_a_plain_host() {
        assert_eq!(
            url_host("https://API.example.com/v1?x=1").unwrap(),
            "api.example.com"
        );
        for bad in [
            "http://api.example.com/",
            "https://user@api.example.com/",
            "https://api.example.com:8443/",
            "https://api.example.com\\@evil.com/",
            "ftp://x.com",
            "https://",
            "api.example.com",
        ] {
            assert!(url_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paths_stay_in_the_area() {
        for ok in ["notes.txt", "cache/2026-11-26.json", "a/b/c"] {
            assert!(check_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a/./b",
            "a\\b",
            "C:x",
            "a/b/",
            "a/b/c/d/e/f/g/h/i",
        ] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn consent_only_covers_what_was_asked() {
        let m = PluginManifest::new(PluginId::new("brief").unwrap(), "Brief", "1");
        assert!(Grants::new(&m, Consent::new([Permission::Notify])).is_err());
        let m = m.with_permission(crate::PermissionRequest::Notify);
        assert!(Grants::new(&m, Consent::new([Permission::Notify])).is_ok());
    }

    #[test]
    fn narrowing_only_adds_lists() {
        let m = PluginManifest::new(PluginId::new("brief").unwrap(), "Brief", "1");
        let c = Caller::for_tool(
            &m,
            "plugin.brief.sum",
            RiskTier::LowWrite,
            Id::new(),
            vec![vec!["mcp.files.*".into()], vec!["mcp.files.read".into()]],
        );
        assert!(c.narrowing_allows("mcp.files.read"));
        assert!(!c.narrowing_allows("mcp.files.write"));
        assert!(!c.narrowing_allows("plugin.brief.sum"));
        let free = Caller::for_job(&m, "daily", Id::new());
        assert!(free.narrowing_allows("anything.at.all"));
    }
}

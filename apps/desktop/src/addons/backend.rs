//! The app's side of the add-on boundary: the `HostBackend` an add-on's `Host` reaches, and
//! the bridge that lets the executor run an add-on's tool (ADR-0013 §2, §4).
//!
//! `dn_plugin_api::Host` has already checked the handle, the access and the permission
//! before any method here runs. This side applies what only the app can see: which chats
//! are in scope, the executor's approval gate and tier checks, holding a job's calls for
//! the person, and the ownership of keychain ids.
//!
//! Services this version doesn't offer add-ons yet fail with a clear message naming the
//! service (`not_offered`); each has a follow-up issue listed in the PR that added this.

use super::{Addons, not_offered, store};
use crate::state::AppState;
use dn_core::chat::{Role as CoreRole, ToolCall};
use dn_plugin_api::{
    BoxFuture, Caller, ChatMessage, ChatScope, ConversationSummary, ErrorKind, Grants,
    GrantsHandle, Host, HostBackend, HttpRequest, HttpResponse, Id, KnowledgeHit, MemoryItem,
    ModelPrompt, ModelSlot, NewMemory, Origin, PluginError, PluginManifest, Role, Row, SqlValue,
    ToolHandler, ToolInput, ToolResult,
};
use dn_tools::error::{ToolError, ToolErrorKind};
use dn_tools::{CallContext, JobCall, McpCallOutput, PluginCall};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

/// Runs `fut`; `None` when it panicked, instead of unwinding into the app.
pub async fn guarded<T>(fut: impl Future<Output = T> + Send) -> Option<T> {
    struct CatchUnwind<F>(Pin<Box<F>>);
    impl<F: Future> Future for CatchUnwind<F> {
        type Output = std::thread::Result<F::Output>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let inner = self.0.as_mut();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(v)) => Poll::Ready(Ok(v)),
                Err(p) => Poll::Ready(Err(p)),
            }
        }
    }
    CatchUnwind(Box::pin(fut)).await.ok()
}

/// The `HostBackend` of one add-on.
pub struct AppBackend {
    state: Weak<AppState>,
}

impl AppBackend {
    pub fn new(state: &Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::downgrade(state),
        })
    }

    fn app(&self) -> Result<Arc<AppState>, PluginError> {
        self.state
            .upgrade()
            .ok_or_else(|| PluginError::unavailable("Demios is closing"))
    }

    fn db<T, E: std::fmt::Display>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<T, E>,
    ) -> Result<T, PluginError> {
        let app = self.app()?;
        let db = app.db().map_err(|e| PluginError::unavailable(e.message))?;
        f(&db).map_err(|e| PluginError::failed(format!("the local database failed: {e}")))
    }
}

fn ready<'a, T: Send + 'a>(v: T) -> BoxFuture<'a, T> {
    Box::pin(std::future::ready(v))
}

fn in_scope(c: &dn_store::chat::Conversation, scope: &ChatScope) -> bool {
    match scope {
        ChatScope::All => true,
        ChatScope::Projects(p) => c.project_id.is_some_and(|id| p.contains(&id)),
        _ => false,
    }
}

impl HostBackend for AppBackend {
    fn list_conversations<'a>(
        &'a self,
        _caller: &'a Caller,
        scope: &'a ChatScope,
    ) -> BoxFuture<'a, Result<Vec<ConversationSummary>, PluginError>> {
        let r = self.db(dn_store::chat::list_conversations).map(|v| {
            v.into_iter()
                .filter(|c| in_scope(c, scope))
                .map(|c| ConversationSummary::new(c.id, c.project_id, c.title, c.updated_at))
                .collect()
        });
        ready(r)
    }

    fn read_messages<'a>(
        &'a self,
        _caller: &'a Caller,
        scope: &'a ChatScope,
        conversation: Id,
    ) -> BoxFuture<'a, Result<Vec<ChatMessage>, PluginError>> {
        let r = self
            .db(|db| {
                let c = dn_store::chat::list_conversations(db)?
                    .into_iter()
                    .find(|c| c.id == conversation);
                match c {
                    Some(c) if in_scope(&c, scope) => {
                        dn_store::chat::list_messages(db, conversation).map(Some)
                    }
                    _ => Ok(None),
                }
            })
            .and_then(|m| {
                m.ok_or_else(|| PluginError::not_allowed("That chat isn't one the add-on may read"))
            })
            .map(|msgs| {
                msgs.into_iter()
                    .filter_map(|m| {
                        let role = match m.role {
                            CoreRole::User => Role::User,
                            CoreRole::Assistant => Role::Assistant,
                            _ => return None,
                        };
                        Some(ChatMessage::new(m.id, role, m.text(), m.created_at))
                    })
                    .collect()
            });
        ready(r)
    }

    fn memory_list<'a>(
        &'a self,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<Vec<MemoryItem>, PluginError>> {
        ready(Err(not_offered(caller, "memory")))
    }

    fn memory_add<'a>(
        &'a self,
        caller: &'a Caller,
        _item: NewMemory,
    ) -> BoxFuture<'a, Result<Id, PluginError>> {
        ready(Err(not_offered(caller, "memory")))
    }

    fn memory_update<'a>(
        &'a self,
        caller: &'a Caller,
        _id: Id,
        _text: String,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(Err(not_offered(caller, "memory")))
    }

    fn memory_delete<'a>(
        &'a self,
        caller: &'a Caller,
        _id: Id,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(Err(not_offered(caller, "memory")))
    }

    fn memory_clear<'a>(&'a self, caller: &'a Caller) -> BoxFuture<'a, Result<(), PluginError>> {
        // Nothing can have been stored: there is nothing to clear.
        if caller.origin == Origin::Deletion {
            return ready(Ok(()));
        }
        ready(Err(not_offered(caller, "memory")))
    }

    fn knowledge_search<'a>(
        &'a self,
        caller: &'a Caller,
        _projects: &'a [Id],
        _query: String,
        _limit: u32,
    ) -> BoxFuture<'a, Result<Vec<KnowledgeHit>, PluginError>> {
        ready(Err(not_offered(caller, "project search")))
    }

    fn run_model<'a>(
        &'a self,
        caller: &'a Caller,
        _slot: ModelSlot,
        _prompt: ModelPrompt,
    ) -> BoxFuture<'a, Result<String, PluginError>> {
        ready(Err(not_offered(caller, "your models")))
    }

    fn call_tool<'a>(
        &'a self,
        caller: &'a Caller,
        capability_id: String,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<ToolResult, PluginError>> {
        Box::pin(async move {
            let app = self.app()?;
            let exec = app
                .executor
                .clone()
                .ok_or_else(|| PluginError::unavailable("Tools aren't available"))?;
            let job = match &caller.origin {
                Origin::Job { job_id } => Some(job_id.clone()),
                _ => None,
            };
            // A job's call needs a person, unless the person set that tool to run without
            // asking for this add-on (not offered yet, so every call is held).
            if let Some(job_id) = &job {
                super::jobs::hold_tool(&app, caller, job_id, &capability_id, &arguments)?;
                return Err(PluginError::new(
                    ErrorKind::Held,
                    "This call waits for your answer in Settings › Add-ons before it runs",
                ));
            }
            let ctx = CallContext::for_plugin(
                caller.run,
                PluginCall {
                    plugin_id: caller.plugin_id.to_string(),
                    name: caller.plugin_name.clone(),
                    max_tier: caller.max_tool_tier,
                    job: job.map(|id| JobCall {
                        name: id.clone(),
                        id,
                        auto_tools: Vec::new(),
                    }),
                },
                caller.allowed_tools.clone(),
            );
            let call = ToolCall {
                id: Id::new().to_string(),
                name: capability_id,
                arguments,
            };
            match exec.execute(&ctx, &call).await {
                Ok(out) => Ok(tool_result(&out.output)),
                Err(dn_core::CoreError::Cancelled) => {
                    Err(PluginError::cancelled("The tool call was stopped"))
                }
                Err(e) => Err(PluginError::failed(e.to_string())),
            }
        })
    }

    fn notify<'a>(
        &'a self,
        caller: &'a Caller,
        _title: String,
        _body: String,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(Err(not_offered(caller, "notifications")))
    }

    fn setting_get<'a>(
        &'a self,
        caller: &'a Caller,
        key: String,
    ) -> BoxFuture<'a, Result<Option<serde_json::Value>, PluginError>> {
        ready(self.db(|db| store::setting_get(db, &caller.plugin_id, &key)))
    }

    fn setting_set<'a>(
        &'a self,
        caller: &'a Caller,
        key: String,
        value: serde_json::Value,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(self.db(|db| store::setting_set(db, &caller.plugin_id, &key, &value)))
    }

    fn setting_remove<'a>(
        &'a self,
        caller: &'a Caller,
        key: String,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(self.db(|db| store::setting_remove(db, &caller.plugin_id, &key)))
    }

    fn secret_get<'a>(
        &'a self,
        caller: &'a Caller,
        name: String,
    ) -> BoxFuture<'a, Result<Option<String>, PluginError>> {
        let r = self.app().and_then(|app| {
            app.keys
                .store
                .get(&dn_secrets::SecretId::addon(
                    caller.plugin_id.as_str(),
                    &name,
                ))
                .map(|v| v.map(|s| s.expose().to_owned()))
                .map_err(|e| PluginError::unavailable(format!("The system keychain failed: {e}")))
        });
        ready(r)
    }

    fn secret_set<'a>(
        &'a self,
        caller: &'a Caller,
        name: String,
        value: String,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        // Claimed first, so "Delete all" finds it even if the keychain write half-fails.
        let r = self
            .db(|db| {
                Ok::<_, rusqlite::Error>(store::claim(
                    db,
                    store::Owned::Secret,
                    &format!("{}/{name}", caller.plugin_id),
                    &caller.plugin_id,
                ))
            })
            .and_then(|c| c.map_err(PluginError::not_allowed))
            .and_then(|()| self.app())
            .and_then(|app| {
                app.keys
                    .store
                    .set(
                        &dn_secrets::SecretId::addon(caller.plugin_id.as_str(), &name),
                        &dn_secrets::Secret::new(value),
                    )
                    .map_err(|e| {
                        PluginError::unavailable(format!("The system keychain failed: {e}"))
                    })
            });
        ready(r)
    }

    fn secret_delete<'a>(
        &'a self,
        caller: &'a Caller,
        name: String,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        let r = self.app().and_then(|app| {
            app.keys
                .store
                .delete(&dn_secrets::SecretId::addon(
                    caller.plugin_id.as_str(),
                    &name,
                ))
                .map(|_| ())
                .map_err(|e| PluginError::unavailable(format!("The system keychain failed: {e}")))
        });
        ready(r)
    }

    fn http<'a>(
        &'a self,
        caller: &'a Caller,
        _request: HttpRequest,
    ) -> BoxFuture<'a, Result<HttpResponse, PluginError>> {
        ready(Err(not_offered(caller, "the internet")))
    }

    fn hold_http<'a>(
        &'a self,
        caller: &'a Caller,
        _request: HttpRequest,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(Err(not_offered(caller, "the internet")))
    }

    fn file_read<'a>(
        &'a self,
        caller: &'a Caller,
        _path: String,
    ) -> BoxFuture<'a, Result<Vec<u8>, PluginError>> {
        ready(Err(not_offered(caller, "files")))
    }

    fn file_write<'a>(
        &'a self,
        caller: &'a Caller,
        _path: String,
        _bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(Err(not_offered(caller, "files")))
    }

    fn file_list<'a>(
        &'a self,
        caller: &'a Caller,
        _folder: String,
    ) -> BoxFuture<'a, Result<Vec<String>, PluginError>> {
        // Nothing can have been written: a deletion finds no files.
        if caller.origin == Origin::Deletion {
            return ready(Ok(Vec::new()));
        }
        ready(Err(not_offered(caller, "files")))
    }

    fn file_delete<'a>(
        &'a self,
        caller: &'a Caller,
        _path: String,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        ready(Err(not_offered(caller, "files")))
    }

    fn db_execute<'a>(
        &'a self,
        caller: &'a Caller,
        _sql: String,
        _params: Vec<SqlValue>,
    ) -> BoxFuture<'a, Result<u64, PluginError>> {
        ready(Err(not_offered(caller, "its own database")))
    }

    fn db_query<'a>(
        &'a self,
        caller: &'a Caller,
        _sql: String,
        _params: Vec<SqlValue>,
    ) -> BoxFuture<'a, Result<Vec<Row>, PluginError>> {
        ready(Err(not_offered(caller, "its own database")))
    }
}

/// MCP-shaped output → what the add-on sees.
fn tool_result(out: &McpCallOutput) -> ToolResult {
    let mut r = if out.is_error {
        ToolResult::error(out.text())
    } else {
        ToolResult::text(out.text())
    };
    if let Some(s) = &out.structured_content {
        r = r.with_structured(s.clone());
    }
    r
}

/// What the add-on returned → the executor's output.
fn mcp_output(r: ToolResult) -> McpCallOutput {
    McpCallOutput {
        content: serde_json::json!([{ "type": "text", "text": r.text }]),
        structured_content: r.structured,
        is_error: r.is_error,
    }
}

/// A `Host` for `delete_all` or `forget_conversation`: a fresh handle (the add-on's own was
/// closed first) that can only read and delete its own data.
pub fn deletion_host(state: &Arc<AppState>, manifest: &PluginManifest) -> Host {
    Host::new(
        AppBackend::new(state),
        GrantsHandle::new(Grants::none()),
        Caller::for_deletion(manifest, Id::new()),
    )
}

/// Runs one add-on tool for the executor: builds the add-on's `Host` for this call (the
/// tool's current tier, the run's allowlists) and calls its handler.
pub struct ToolBridge {
    state: Weak<AppState>,
    manifest: PluginManifest,
    capability_id: String,
    /// The tier the add-on declared (at least its floor). The handler's access follows this,
    /// never the person's override: raising a tool's tier makes it ask more, and must not
    /// give its handler more access.
    declared: dn_plugin_api::RiskTier,
    handler: Arc<dyn ToolHandler>,
    grants: GrantsHandle,
}

impl ToolBridge {
    pub fn new(
        state: &Arc<AppState>,
        manifest: PluginManifest,
        capability_id: String,
        declared: dn_plugin_api::RiskTier,
        handler: Arc<dyn ToolHandler>,
        grants: GrantsHandle,
    ) -> Self {
        Self {
            state: Arc::downgrade(state),
            manifest,
            capability_id,
            declared,
            handler,
            grants,
        }
    }
}

impl dn_tools::InProcessTool for ToolBridge {
    fn call<'a>(
        &'a self,
        ctx: &'a CallContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<McpCallOutput, ToolError>> {
        Box::pin(async move {
            let id = &self.capability_id;
            let state = self
                .state
                .upgrade()
                .ok_or_else(|| ToolError::new(id, ToolErrorKind::Unavailable, "closing"))?;
            let caller = Caller::for_tool(
                &self.manifest,
                id.clone(),
                self.declared,
                ctx.run_id,
                ctx.allowed_tools.clone(),
            );
            let host = Host::new(AppBackend::new(&state), self.grants.clone(), caller);
            let cancel = dn_plugin_api::CancelToken::new();
            let _cancel_on_drop = CancelOnDrop(cancel.clone());
            let input = ToolInput::new(arguments, cancel);
            match guarded(self.handler.call(&host, input)).await {
                Some(Ok(r)) => Ok(mcp_output(r)),
                Some(Err(e)) => Err(ToolError::new(id, ToolErrorKind::ToolFailed, &e.message)
                    .with_model_message(e.message)),
                None => {
                    state.addons.stop_after_panic(&self.manifest.id);
                    Err(
                        ToolError::new(id, ToolErrorKind::ToolFailed, "the handler panicked")
                            .with_model_message(format!(
                                "{} stopped after an error and is off until Demios restarts.",
                                self.manifest.name
                            )),
                    )
                }
            }
        })
    }
}

/// Cancels the handler's token when the executor stops waiting (the person stopped the run).
struct CancelOnDrop(dn_plugin_api::CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Addons {
    /// A `Host` for one run of `job` (used by [`super::jobs`]).
    pub(crate) fn job_host(
        state: &Arc<AppState>,
        manifest: &PluginManifest,
        grants: GrantsHandle,
        job_id: &str,
        run: Id,
    ) -> Host {
        Host::new(
            AppBackend::new(state),
            grants,
            Caller::for_job(manifest, job_id, run),
        )
    }
}

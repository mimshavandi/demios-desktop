//! Typed IPC commands. Each one is listed in `build.rs` and granted explicitly in
//! `capabilities/default.json`; nothing else is callable from the webview.
//! The TypeScript side mirrors these signatures in `ui/src/ipc/commands.ts`.
//!
//! SQLite and the key store are blocking, so every command that touches them runs on a
//! blocking thread ([`blocking`]) instead of a tokio worker.

use crate::chat::{self, ChatEvent, TurnStarted};
use crate::error::{IpcError, IpcResult};
use crate::models::{self, ModelCatalog};
use crate::secrets::Secret;
use crate::settings::Settings;
use crate::state::{AppState, ToolEvent, ToolSnapshot};
use dn_core::Id;
use dn_store::chat::{self as store, Conversation, StoredMessage};
use dn_tools::ApprovalDecision;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tauri::State;
use tauri::ipc::Channel;

type AppStateRef<'a> = State<'a, Arc<AppState>>;

pub(crate) async fn blocking<T: Send + 'static>(
    state: &AppStateRef<'_>,
    f: impl FnOnce(&Arc<AppState>) -> IpcResult<T> + Send + 'static,
) -> IpcResult<T> {
    let st = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || f(&st))
        .await
        .map_err(|e| {
            eprintln!("blocking task failed: {e}");
            IpcError::new("internal", "Something went wrong. Try again.")
        })?
}

#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub dev_fake_model: bool,
    pub data_dir: Option<String>,
    /// Where downloaded models live (shared with the `dn` CLI).
    pub models_dir: Option<String>,
    /// How this build gets updates (T-STORE-08).
    pub updates: crate::updates::UpdateMode,
    /// Commit the build was made from, set by CI (`DEMIOS_BUILD_COMMIT`); `None` for local builds.
    pub commit: Option<&'static str>,
    /// Build channel set by CI (`DEMIOS_BUILD_CHANNEL`, e.g. `preview`); `None` for local builds.
    pub channel: Option<&'static str>,
    /// Why the assistant's instructions (`skills/`) didn't load, if they didn't; chats
    /// can't start until this is fixed (#141).
    pub agent_error: Option<String>,
}

#[tauri::command]
pub async fn app_info(app: tauri::AppHandle, state: AppStateRef<'_>) -> IpcResult<AppInfo> {
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        dev_fake_model: cfg!(fake_model),
        data_dir: crate::data_dir().map(|p| p.display().to_string()),
        models_dir: state
            .models
            .local
            .as_ref()
            .map(|l| l.dir().display().to_string()),
        updates: crate::updates::current_mode(app.config()),
        commit: option_env!("DEMIOS_BUILD_COMMIT").filter(|c| !c.is_empty()),
        channel: option_env!("DEMIOS_BUILD_CHANNEL").filter(|c| !c.is_empty()),
        agent_error: state.agent.load_error().map(str::to_owned),
    })
}

/// Opens the "Report a problem" / "Report this answer" form in the system browser
/// (T-STORE-14). The URL is fixed in [`crate::support`]. For an answer the webview passes the
/// message id and the model ref is read from the database, so the webview can't choose what
/// goes into the URL.
#[tauri::command]
pub async fn report_problem(
    app: tauri::AppHandle,
    state: AppStateRef<'_>,
    kind: crate::support::ReportKind,
    message_id: Option<Id>,
) -> IpcResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let model = match (kind, message_id) {
        (crate::support::ReportKind::Answer, Some(id)) => {
            blocking(&state, move |s| {
                let m = store::get_message(&*s.db()?, id)?;
                Ok(crate::support::answer_model(&m))
            })
            .await?
        }
        _ => None,
    };
    let url = crate::support::report_url(
        kind,
        env!("CARGO_PKG_VERSION"),
        &crate::support::os_label(),
        model.as_ref(),
    );
    app.opener().open_url(url, None::<&str>).map_err(|e| {
        eprintln!("could not open the browser: {e}");
        IpcError::new(
            "unavailable",
            "Couldn't open your browser. Report at github.com/mimshavandi/demios-desktop/issues.",
        )
    })
}

/// What "Delete all local data" removes, for the confirm dialog.
#[derive(Debug, Serialize)]
pub struct LocalDataSummary {
    pub conversations: usize,
    pub projects: usize,
    /// Artifacts in the database (all versions go with them).
    pub artifacts: u64,
    pub data_dir: Option<String>,
    pub models_dir: Option<String>,
    pub models_bytes: u64,
    /// Keychain entries in the user's words ("OpenAI key", "Hugging Face token", …).
    pub keychain: Vec<String>,
}

#[tauri::command]
pub async fn local_data_summary(state: AppStateRef<'_>) -> IpcResult<LocalDataSummary> {
    blocking(&state, |s| {
        let (conversations, projects, artifacts) = {
            let db = s.db()?;
            let n = store::list_conversations(&db)?.len();
            let p = dn_store::project::list_projects(&db)?.len();
            let a = dn_artifacts::ArtifactStore::new(&db)
                .counts()
                .map(|(a, _)| a)
                .map_err(|e| {
                    eprintln!("could not count artifacts: {e}");
                    IpcError::new("storage", "Could not read local data.")
                })?;
            (n, p, a)
        };
        let local = s.models.local.as_ref();
        Ok(LocalDataSummary {
            conversations,
            projects,
            artifacts,
            data_dir: crate::data_dir().map(|p| p.display().to_string()),
            models_dir: local.map(|l| l.dir().display().to_string()),
            models_bytes: local.map(|l| l.disk_usage()).unwrap_or(0),
            keychain: s.keychain_entries(),
        })
    })
    .await
}

/// "Delete all local data" (T-STORE-11): stops replies and downloads, then deletes chats,
/// settings, tool history, downloaded models, keychain entries and webview storage. It
/// keeps going after a failure and reports what could not be deleted. The UI reloads after.
#[tauri::command]
pub async fn delete_all_local_data(
    state: AppStateRef<'_>,
    webview: tauri::Webview,
) -> IpcResult<()> {
    let mut failed = state.delete_all_local_data().await;
    if let Err(e) = webview.clear_all_browsing_data() {
        eprintln!("could not clear webview storage: {e}");
        failed.push("window storage");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(IpcError::new(
            "partial",
            format!(
                "Some data could not be deleted: {}. Try again.",
                failed.join(", ")
            ),
        ))
    }
}

#[tauri::command]
pub async fn list_conversations(state: AppStateRef<'_>) -> IpcResult<Vec<Conversation>> {
    blocking(&state, |s| Ok(store::list_conversations(&*s.db()?)?)).await
}

#[tauri::command]
pub async fn create_conversation(
    state: AppStateRef<'_>,
    project_id: Option<Id>,
) -> IpcResult<Conversation> {
    blocking(&state, move |s| s.create_conversation_in(project_id)).await
}

#[tauri::command]
pub async fn rename_conversation(
    state: AppStateRef<'_>,
    conversation_id: Id,
    title: String,
) -> IpcResult<()> {
    let title = title.trim().to_string();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(IpcError::invalid("Title must be 1–200 characters."));
    }
    blocking(&state, move |s| {
        Ok(store::rename_conversation(
            &*s.db()?,
            conversation_id,
            &title,
        )?)
    })
    .await
}

/// The chat skills in the library (T-SKL-01), for the composer's picker.
#[tauri::command]
pub async fn list_skills(state: AppStateRef<'_>) -> IpcResult<Vec<crate::skills::SkillInfo>> {
    Ok(state.skills.list())
}

/// Settings › Skills: every skill, the user's files that didn't load and why, and the
/// user's skills folder.
#[tauri::command]
pub async fn skills_overview(state: AppStateRef<'_>) -> IpcResult<crate::skills::SkillsOverview> {
    Ok(state.skills.overview())
}

/// Reads the skills folders again. Chats keep their choice; one whose skill is gone uses
/// none until the user picks another.
#[tauri::command]
pub async fn reload_skills(state: AppStateRef<'_>) -> IpcResult<crate::skills::SkillsOverview> {
    let st = Arc::clone(&*state);
    tauri::async_runtime::spawn_blocking(move || {
        st.skills.reload();
        st.skills.overview()
    })
    .await
    .map_err(|_| IpcError::new("internal", "Couldn't reload the skills."))
}

/// Opens the user's skills folder in the file manager, creating it first. The path is the
/// app's own; the webview can't pass one.
#[tauri::command]
pub async fn open_skills_folder(app: tauri::AppHandle, state: AppStateRef<'_>) -> IpcResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = state
        .skills
        .user_dir()
        .map(Path::to_path_buf)
        .ok_or_else(|| IpcError::new("unavailable", "There is no skills folder in this build."))?;
    std::fs::create_dir_all(&dir).map_err(|e| {
        eprintln!("could not create the skills folder: {e}");
        IpcError::new("unavailable", "Couldn't create the skills folder.")
    })?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| {
            eprintln!("could not open the skills folder: {e}");
            IpcError::new("unavailable", "Couldn't open the skills folder.")
        })
}

/// The answers of a chat that were written with a skill, with the skill's label then.
#[tauri::command]
pub async fn list_answer_skills(
    state: AppStateRef<'_>,
    conversation_id: Id,
) -> IpcResult<Vec<crate::skills::AnswerSkill>> {
    blocking(&state, move |s| {
        Ok(crate::skills::answer_skills(&*s.db()?, conversation_id)?)
    })
    .await
}

/// The skill a chat uses, or `null`. A saved skill that is no longer in the library is
/// still returned, so the composer can say it is missing (sending then fails clearly).
#[tauri::command]
pub async fn get_chat_skill(
    state: AppStateRef<'_>,
    conversation_id: Id,
) -> IpcResult<Option<String>> {
    blocking(&state, move |s| {
        Ok(crate::skills::chat_skill(&*s.db()?, conversation_id)?)
    })
    .await
}

/// Picks the skill for a chat (`null` = none). It applies from the next answer on.
#[tauri::command]
pub async fn set_chat_skill(
    state: AppStateRef<'_>,
    conversation_id: Id,
    skill: Option<String>,
) -> IpcResult<()> {
    crate::skills::check_choice(&state.skills, skill.as_deref())?;
    blocking(&state, move |s| {
        let db = s.db()?;
        store::get_conversation(&db, conversation_id)?;
        Ok(crate::skills::set_chat_skill(
            &db,
            conversation_id,
            skill.as_deref(),
        )?)
    })
    .await
}

/// Soft delete: the chat can be restored until the next start (design-system §5 Undo).
#[tauri::command]
pub async fn delete_conversation(state: AppStateRef<'_>, conversation_id: Id) -> IpcResult<()> {
    chat::stop(&state, conversation_id);
    // "Always allow in this chat" grants end with the chat.
    if let Some(exec) = &state.executor {
        exec.gate()
            .clear_conversation(&format!("conv:{conversation_id}"));
    }
    blocking(&state, move |s| {
        Ok(store::delete_conversation(&*s.db()?, conversation_id)?)
    })
    .await
}

#[tauri::command]
pub async fn restore_conversation(state: AppStateRef<'_>, conversation_id: Id) -> IpcResult<()> {
    blocking(&state, move |s| {
        Ok(store::restore_conversation(&*s.db()?, conversation_id)?)
    })
    .await
}

#[tauri::command]
pub async fn list_messages(
    state: AppStateRef<'_>,
    conversation_id: Id,
) -> IpcResult<Vec<StoredMessage>> {
    blocking(&state, move |s| s.list_messages(conversation_id)).await
}

fn channel_sink(ch: Channel<ChatEvent>) -> impl chat::EventSink {
    move |ev: ChatEvent| {
        // The window may have gone away; the answer is still stored.
        let _ = ch.send(ev);
    }
}

#[tauri::command]
pub async fn send_message(
    state: AppStateRef<'_>,
    conversation_id: Id,
    text: String,
    on_event: Channel<ChatEvent>,
) -> IpcResult<TurnStarted> {
    if text.len() > 200_000 {
        return Err(IpcError::invalid("Message is too long."));
    }
    let sink = channel_sink(on_event);
    blocking(&state, move |s| chat::send(s, conversation_id, text, sink)).await
}

#[tauri::command]
pub async fn regenerate(
    state: AppStateRef<'_>,
    conversation_id: Id,
    on_event: Channel<ChatEvent>,
) -> IpcResult<TurnStarted> {
    let sink = channel_sink(on_event);
    blocking(&state, move |s| chat::regenerate(s, conversation_id, sink)).await
}

#[tauri::command]
pub async fn stop_generation(state: AppStateRef<'_>, conversation_id: Id) -> IpcResult<bool> {
    Ok(chat::stop(&state, conversation_id))
}

#[tauri::command]
pub async fn get_settings(state: AppStateRef<'_>) -> IpcResult<Settings> {
    blocking(&state, |s| s.settings()).await
}

#[tauri::command]
pub async fn update_settings(
    state: AppStateRef<'_>,
    mut settings: Settings,
) -> IpcResult<Settings> {
    blocking(&state, move |s| {
        // Servers and tool choices have their own commands; a settings save keeps them.
        settings.keep_managed(s.settings()?);
        s.save_settings(&settings)?;
        s.settings()
    })
    .await
}

fn catalog(s: &AppState) -> IpcResult<ModelCatalog> {
    let settings = s.settings()?;
    let mut c = models::catalog(&s.models, &s.keys, &settings);
    let resolved = s.agent.resolve_model(&settings, &s.models, &s.keys).ok();
    c.chat_model_tools = resolved
        .as_ref()
        .is_some_and(|m| s.models.supports_tools(m));
    c.chat_model = resolved.map(|m| m.to_string());
    Ok(c)
}

#[tauri::command]
pub async fn list_models(state: AppStateRef<'_>) -> IpcResult<ModelCatalog> {
    blocking(&state, |s| catalog(s)).await
}

fn known_provider(provider_id: &str) -> IpcResult<()> {
    models::provider(provider_id)
        .map(|_| ())
        .ok_or_else(|| IpcError::invalid("Unknown provider."))
}

#[tauri::command]
pub async fn set_provider_key(
    state: AppStateRef<'_>,
    provider_id: String,
    api_key: String,
) -> IpcResult<ModelCatalog> {
    known_provider(&provider_id)?;
    let key = api_key.trim().to_string();
    if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_whitespace) {
        return Err(IpcError::invalid(
            "Paste the key as one line without spaces.",
        ));
    }
    blocking(&state, move |s| {
        s.keys.set(&provider_id, &Secret::new(key))?;
        s.models.invalidate(&provider_id);
        catalog(s)
    })
    .await
}

/// Removes the key from the key store at once. It is held in memory for
/// [`crate::state::KEY_UNDO_WINDOW`] so [`restore_provider_key`] can undo it.
#[tauri::command]
pub async fn delete_provider_key(
    state: AppStateRef<'_>,
    provider_id: String,
) -> IpcResult<ModelCatalog> {
    known_provider(&provider_id)?;
    blocking(&state, move |s| {
        let secret = s.keys.get(&provider_id)?;
        s.keys.delete(&provider_id)?;
        s.models.invalidate(&provider_id);
        // A slot pointing at this provider would now fail; clear it rather than fall back.
        let mut settings = s.settings()?;
        let prefix = format!("byok:{provider_id}:");
        let cleared: Vec<(String, String)> = settings
            .slots
            .iter()
            .filter(|(_, m)| m.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        settings.slots.retain(|_, m| !m.starts_with(&prefix));
        s.save_settings(&settings)?;
        if let Some(secret) = secret {
            s.remember_removed_key(&provider_id, secret, cleared);
        }
        catalog(s)
    })
    .await
}

#[tauri::command]
pub async fn restore_provider_key(
    state: AppStateRef<'_>,
    provider_id: String,
) -> IpcResult<ModelCatalog> {
    known_provider(&provider_id)?;
    blocking(&state, move |s| {
        let (secret, slots) = s.take_removed_key(&provider_id).ok_or_else(|| {
            IpcError::new(
                "expired",
                "The key can no longer be restored. Paste it again.",
            )
        })?;
        s.keys.set(&provider_id, &secret)?;
        s.models.invalidate(&provider_id);
        let mut settings = s.settings()?;
        for (slot, model) in slots {
            settings.slots.entry(slot).or_insert(model);
        }
        s.save_settings(&settings)?;
        catalog(s)
    })
    .await
}

// ---- tool approvals and outcome-unknown resolution (T-SHELL-04)

/// Subscribes the UI to approval requests and MCP server status; returns the current ones.
#[tauri::command]
pub async fn watch_tools(
    state: AppStateRef<'_>,
    on_event: Channel<ToolEvent>,
) -> IpcResult<ToolSnapshot> {
    Ok(state.watch_tools(Arc::new(move |ev| {
        let _ = on_event.send(ev);
    })))
}

#[tauri::command]
pub async fn respond_approval(
    state: AppStateRef<'_>,
    request_id: Id,
    decision: ApprovalDecision,
) -> IpcResult<()> {
    state.respond_approval(request_id, decision)
}

/// A tool write whose result is not known (crash or stop mid-call).
#[derive(Debug, Serialize)]
pub struct UnknownOutcome {
    pub key: String,
    pub capability_id: String,
    pub arguments: serde_json::Value,
    pub error: Option<String>,
    pub updated_at_ms: i64,
}

#[tauri::command]
pub async fn list_unknown_outcomes(
    state: AppStateRef<'_>,
    conversation_id: Option<Id>,
) -> IpcResult<Vec<UnknownOutcome>> {
    blocking(&state, move |s| {
        let Some(exec) = &s.executor else {
            return Ok(vec![]);
        };
        let want = conversation_id.map(|c| c.to_string());
        Ok(exec
            .unknown_outcomes()
            .map_err(crate::agent::model_error)?
            .into_iter()
            .filter(|e| want.as_ref().is_none_or(|w| &e.conversation_id == w))
            .map(|e| UnknownOutcome {
                key: e.idempotency_key,
                capability_id: e.capability_id,
                arguments: e.arguments,
                error: e.error,
                updated_at_ms: e.updated_at_ms,
            })
            .collect())
    })
    .await
}

/// The user's answer to "did this go through?".
#[tauri::command]
pub async fn resolve_unknown_outcome(
    state: AppStateRef<'_>,
    key: String,
    happened: bool,
) -> IpcResult<()> {
    blocking(&state, move |s| {
        let exec = s
            .executor
            .as_ref()
            .ok_or_else(|| IpcError::new("unavailable", "Tools are not available."))?;
        let resolution = if happened {
            dn_tools::ledger::Resolution::Happened {
                // Replays of this call return this instead of running it again.
                output: serde_json::json!({
                    "content": [{ "type": "text", "text": "The user confirmed this already happened." }]
                }),
            }
        } else {
            dn_tools::ledger::Resolution::DidNotHappen
        };
        exec.resolve_unknown(&key, &resolution)
            .map_err(crate::agent::model_error)?;
        // If its chat was purged meanwhile, the resolved row has nothing left to protect.
        if let Ok(Some(entry)) = exec.ledger_entry(&key)
            && let Some(conv) = entry.conversation_id.strip_prefix("conv:")
            && conversation_purged(s, conv)
            && let Err(e) = exec.forget_call(&key)
        {
            eprintln!("could not clear a resolved tool call: {e}");
        }
        Ok(())
    })
    .await
}

// ---- artifacts (T-ART-06)

#[tauri::command]
pub async fn list_artifacts(
    state: AppStateRef<'_>,
    conversation_id: Id,
) -> IpcResult<Vec<dn_artifacts::ArtifactSummary>> {
    blocking(&state, move |s| crate::artifacts::list(s, conversation_id)).await
}

#[tauri::command]
pub async fn get_artifact(
    state: AppStateRef<'_>,
    conversation_id: Id,
    artifact_id: Id,
) -> IpcResult<crate::artifacts::ArtifactView> {
    blocking(&state, move |s| {
        crate::artifacts::get(s, conversation_id, artifact_id)
    })
    .await
}

/// "Restore this version": appends a new version (by the user) with `to_version`'s content.
#[tauri::command]
pub async fn rollback_artifact(
    state: AppStateRef<'_>,
    conversation_id: Id,
    artifact_id: Id,
    to_version: u32,
) -> IpcResult<crate::artifacts::ArtifactView> {
    blocking(&state, move |s| {
        crate::artifacts::rollback(s, conversation_id, artifact_id, to_version)
    })
    .await
}

/// Whether `conversation_id` was purged (a soft-deleted chat can still come back).
fn conversation_purged(s: &AppState, conversation_id: &str) -> bool {
    s.db()
        .ok()
        .and_then(|db| store::is_purged(&db, conversation_id).ok())
        .unwrap_or(false)
}

// ---- local models (T-SHELL-07)

fn local(state: &AppState) -> IpcResult<Arc<crate::local::LocalModels>> {
    state.models.local.clone().ok_or_else(|| {
        IpcError::new(
            "unavailable",
            "Local models aren't available in this build.",
        )
    })
}

/// Subscribes to download progress and state changes; returns the current rows.
#[tauri::command]
pub async fn watch_local_models(
    state: AppStateRef<'_>,
    on_event: Channel<crate::local::LocalModelEvent>,
) -> IpcResult<Vec<crate::local::LocalModelRow>> {
    Ok(local(&state)?.watch_and_refresh(Arc::new(move |ev| {
        let _ = on_event.send(ev);
    })))
}

/// Starts or resumes a catalogue download (resumable, SHA-256 verified).
#[tauri::command]
pub async fn download_model(state: AppStateRef<'_>, model_id: String) -> IpcResult<()> {
    local(&state)?
        .download(&model_id)
        .map_err(IpcError::invalid)
}

/// Pauses a download; the partial file is kept and resumed by the next download.
#[tauri::command]
pub async fn pause_download(state: AppStateRef<'_>, model_id: String) -> IpcResult<()> {
    local(&state)?.pause(&model_id);
    Ok(())
}

/// Removes a downloaded model and everything Demios wrote for it (#176). Refused, with a
/// user message and nothing deleted, for Hugging Face cache models, a folder the user chose
/// (the row's `remove_blocked` says why) and a model that is answering. The row update
/// arrives through `watch_local_models`.
#[tauri::command]
pub async fn remove_local_model(state: AppStateRef<'_>, model_id: String) -> IpcResult<()> {
    let local = local(&state)?;
    local
        .remove(&model_id)
        .await
        .map_err(crate::agent::model_error)
}

/// Cancels a catalogue download and deletes its partial file (#176).
#[tauri::command]
pub async fn cancel_download(state: AppStateRef<'_>, model_id: String) -> IpcResult<()> {
    let local = local(&state)?;
    local
        .cancel_download(&model_id)
        .await
        .map_err(crate::agent::model_error)
}

// ---- MCP servers screen (T-SHELL-08)

fn tools_executor(s: &AppState) -> IpcResult<Arc<dn_tools::Executor>> {
    s.executor
        .clone()
        .ok_or_else(|| IpcError::new("unavailable", "Tools are not available."))
}

#[tauri::command]
pub async fn list_mcp_servers(state: AppStateRef<'_>) -> IpcResult<crate::mcp::ServersView> {
    blocking(&state, |s| s.mcp_servers_view()).await
}

/// Adds a server (`editing: null`) or saves changes to one, then (re)connects it.
#[tauri::command]
pub async fn save_mcp_server(
    state: AppStateRef<'_>,
    input: crate::mcp::ServerInput,
    editing: Option<String>,
) -> IpcResult<crate::mcp::ServersView> {
    let cfg = blocking(&state, move |s| {
        s.save_mcp_server(&input, editing.as_deref())
    })
    .await?;
    let st = Arc::clone(state.inner());
    let exec = tools_executor(&st)?;
    let epoch = st.mcp_epoch_now();
    tauri::async_runtime::spawn(async move {
        st.connect_mcp_server(&exec, cfg, epoch).await;
    });
    blocking(&state, |s| s.mcp_servers_view()).await
}

/// Stops the server and deletes it with its secrets and sign-in. Can't be undone.
#[tauri::command]
pub async fn remove_mcp_server(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<crate::mcp::ServersView> {
    if let Some(exec) = &state.executor {
        exec.remove_server(&server_id).await;
    }
    blocking(&state, move |s| s.remove_mcp_server_config(&server_id)).await?;
    blocking(&state, |s| s.mcp_servers_view()).await
}

#[tauri::command]
pub async fn reconnect_mcp_server(state: AppStateRef<'_>, server_id: String) -> IpcResult<()> {
    let cfg = blocking(&state, move |s| s.mcp_server(&server_id)).await?;
    let st = Arc::clone(state.inner());
    let exec = tools_executor(&st)?;
    let epoch = st.mcp_epoch_now();
    st.connect_mcp_server(&exec, cfg, epoch).await;
    Ok(())
}

/// Deletes the server's stored OAuth tokens and stops it; it then needs a new sign-in.
#[tauri::command]
pub async fn sign_out_mcp_server(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<crate::mcp::ServersView> {
    let id = server_id.clone();
    let cfg = blocking(&state, move |s| s.sign_out_mcp_server(&id)).await?;
    if let Some(exec) = &state.executor {
        exec.remove_server(&server_id).await;
    }
    state.set_mcp_status(crate::state::McpServerStatus {
        server_id: cfg.id.clone(),
        name: cfg.display_name.clone().unwrap_or_else(|| cfg.id.clone()),
        state: "needs_auth",
        message: Some("Signed out. Sign in to use this server's tools.".into()),
        tools: 0,
        review: 0,
        revision: 0,
    });
    blocking(&state, |s| s.mcp_servers_view()).await
}

/// How long sign-in waits for the browser to come back.
const SIGN_IN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Signs in to an OAuth server: opens the server's sign-in page in the system browser
/// (from Rust; the webview can't open URLs) and waits for the redirect on a loopback port.
#[tauri::command]
pub async fn sign_in_mcp_server(
    app: tauri::AppHandle,
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let id = server_id.clone();
    let cfg = blocking(&state, move |s| s.mcp_server(&id)).await?;
    if crate::mcp::oauth_id(&cfg).is_none() {
        return Err(IpcError::invalid("This server doesn't use sign-in."));
    }
    let st = Arc::clone(state.inner());
    let exec = tools_executor(&st)?;
    let name = cfg.display_name.clone().unwrap_or_else(|| cfg.id.clone());
    let set = |state: &'static str, message: Option<String>| {
        st.set_mcp_status(crate::state::McpServerStatus {
            server_id: cfg.id.clone(),
            name: name.clone(),
            state,
            message,
            tools: 0,
            review: 0,
            revision: 0,
        })
    };
    set(
        "signing_in",
        Some("Finish signing in in your browser.".into()),
    );
    let result = async {
        let redirect = dn_tools::LoopbackRedirect::bind().await.map_err(|e| {
            eprintln!("oauth loopback: {e}");
            IpcError::new("unavailable", "Couldn't start sign-in. Try again.")
        })?;
        let oauth = dn_tools::OAuthManager::new(st.mcp_secret_store());
        let pending = oauth
            .begin(&cfg, redirect.redirect_uri())
            .await
            .map_err(|e| {
                eprintln!("oauth begin for `{}`: {e}", cfg.id);
                IpcError::new(
                    "sign_in",
                    "The server didn't offer a way to sign in. Check its address, then try again.",
                )
            })?;
        let url = pending.authorize_url().to_owned();
        if !crate::mcp::safe_sign_in_url(&url) {
            eprintln!("refusing to open a non-https sign-in URL for `{}`", cfg.id);
            return Err(IpcError::new(
                "sign_in",
                "The server's sign-in page isn't a secure (https) address, so Demios didn't open it.",
            ));
        }
        app.opener().open_url(url, None::<&str>).map_err(|e| {
            eprintln!("could not open the browser: {e}");
            IpcError::new("unavailable", "Couldn't open your browser.")
        })?;
        let callback = redirect
            .wait_timeout(SIGN_IN_TIMEOUT)
            .await
            .map_err(|_| IpcError::new("timeout", "Sign-in took too long. Try again."))?;
        pending.complete(&callback).await.map_err(|e| {
            eprintln!("oauth complete for `{}`: {e}", cfg.id);
            IpcError::new("sign_in", "Sign-in didn't finish. Try again.")
        })?;
        // The token is for the server as it was when sign-in started. If it was removed or
        // changed meanwhile, throw the token away instead of connecting.
        let expected = cfg.clone();
        let st2 = Arc::clone(&st);
        let still_same = tauri::async_runtime::spawn_blocking(move || st2.keep_sign_in_for(&expected))
        .await
        .map_err(|_| IpcError::new("internal", "Something went wrong. Try again."))??;
        if !still_same {
            return Err(IpcError::new(
                "changed",
                "The server was changed or removed while you were signing in, so that sign-in was discarded.",
            ));
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            let epoch = st.mcp_epoch_now();
            st.connect_mcp_server(&exec, cfg.clone(), epoch).await;
            Ok(())
        }
        Err(e) => {
            // A server removed meanwhile gets no status back.
            if e.code != "changed" {
                set("needs_auth", Some(e.message.clone()));
            }
            Err(e)
        }
    }
}

// ---- each server's tools (T-SHELL-11)

#[tauri::command]
pub async fn list_mcp_tools(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<crate::tool_review::ServerToolsView> {
    blocking(&state, move |s| s.mcp_tools_view(&server_id)).await
}

/// Saves the user's choice for one tool ("always ask", "off", ...) and applies it now.
#[tauri::command]
pub async fn set_mcp_tool_choice(
    state: AppStateRef<'_>,
    server_id: String,
    tool: String,
    choice: crate::tool_review::ToolChoice,
) -> IpcResult<crate::tool_review::ServerToolsView> {
    blocking(&state, move |s| {
        s.set_mcp_tool_choice(&server_id, &tool, choice)
    })
    .await
}

/// Marks new or changed tools as reviewed, so they follow their usual approval again.
#[tauri::command]
pub async fn mark_mcp_tools_reviewed(
    state: AppStateRef<'_>,
    server_id: String,
    tools: Vec<crate::tool_review::Reviewed>,
) -> IpcResult<crate::tool_review::ServerToolsView> {
    blocking(&state, move |s| {
        s.mark_mcp_tools_reviewed(&server_id, &tools)
    })
    .await
}

// ---- links in answers (T-SHELL-09)

/// Opens a link from an answer in the system browser after a native confirmation that shows
/// the full URL. Returns whether it was opened (false: the user chose Cancel).
#[tauri::command]
pub async fn open_link(app: tauri::AppHandle, url: String) -> IpcResult<bool> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    use tauri_plugin_opener::OpenerExt;
    let u = crate::links::check_link(&url).map_err(|r| IpcError::invalid(r.0))?;
    use tauri::Manager;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(crate::links::confirm_text(&u))
        .title("Open link?");
    // Attached to the main window, so it can't be mistaken for another app's prompt.
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.parent(&w);
    }
    dialog
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Open in browser".into(),
            "Cancel".into(),
        ))
        .show(move |ok| {
            let _ = tx.send(ok);
        });
    if !rx.await.unwrap_or(false) {
        return Ok(false);
    }
    app.opener()
        .open_url(u.as_str(), None::<&str>)
        .map_err(|e| {
            eprintln!("could not open the browser: {e}");
            IpcError::new("unavailable", "Couldn't open your browser.")
        })?;
    Ok(true)
}

// ---- add a model from Hugging Face (T-INF-10)

fn local_models(s: &AppState) -> IpcResult<Arc<crate::local::LocalModels>> {
    s.models.local.clone().ok_or_else(|| {
        IpcError::new(
            "unavailable",
            "Local models aren't available in this build.",
        )
    })
}

#[tauri::command]
pub async fn hub_search(
    state: AppStateRef<'_>,
    text: String,
    sort: dn_models::hub::SearchSort,
    cursor: Option<String>,
) -> IpcResult<dn_models::hub::SearchPage> {
    let hub = blocking(&state, |s| crate::hub::client(&s.keys)).await?;
    crate::hub::search(&hub, &text, sort, cursor.as_deref()).await
}

#[tauri::command]
pub async fn hub_repo(state: AppStateRef<'_>, repo: String) -> IpcResult<crate::hub::RepoView> {
    let hub = blocking(&state, |s| crate::hub::client(&s.keys)).await?;
    let local = local_models(&state)?;
    crate::hub::repo(&hub, &local, &state.hub_pulls, &repo).await
}

/// Downloads one quantisation (verified, resumable) and lists it as a local model.
#[tauri::command]
pub async fn hub_pull(
    state: AppStateRef<'_>,
    repo: String,
    sha: String,
    variant: String,
    on_event: Channel<crate::hub::PullEvent>,
) -> IpcResult<String> {
    let hub = blocking(&state, |s| crate::hub::client(&s.keys)).await?;
    let local = local_models(&state)?;
    let pulls = Arc::clone(&state.hub_pulls);
    crate::hub::pull(hub, local, pulls, repo, sha, variant, move |p| {
        let _ = on_event.send(crate::hub::PullEvent::Progress(p));
    })
    .await
}

/// Progress of the running downloads of `repo` at `sha`, by variant name (for a screen
/// that came back while they run).
#[tauri::command]
pub async fn hub_pull_status(
    state: AppStateRef<'_>,
    repo: String,
    sha: String,
    variants: Vec<String>,
) -> IpcResult<std::collections::BTreeMap<String, dn_models::hub::PullProgress>> {
    Ok(variants
        .into_iter()
        .filter_map(|v| {
            let p = state
                .hub_pulls
                .progress_of(&crate::hub::pull_key(&repo, &sha, &v))?;
            Some((v, p))
        })
        .collect())
}

/// Stops a running download; the partial file stays and a new download resumes it.
#[tauri::command]
pub async fn hub_cancel(
    state: AppStateRef<'_>,
    repo: String,
    sha: String,
    variant: String,
) -> IpcResult<bool> {
    Ok(state
        .hub_pulls
        .cancel(&crate::hub::pull_key(&repo, &sha, &variant)))
}

/// Whether a Hugging Face token is saved (never the token itself).
#[tauri::command]
pub async fn hf_token_status(state: AppStateRef<'_>) -> IpcResult<bool> {
    blocking(&state, |s| {
        Ok(s.keys.has(dn_models::hub::HF_TOKEN_SECRET)?)
    })
    .await
}

/// Saves the Hugging Face token (for gated models) in the system keychain.
#[tauri::command]
pub async fn set_hf_token(state: AppStateRef<'_>, token: String) -> IpcResult<bool> {
    let token = token.trim().to_string();
    if token.is_empty() || token.len() > 512 || token.chars().any(char::is_whitespace) {
        return Err(IpcError::invalid(
            "Paste the token as one line without spaces.",
        ));
    }
    blocking(&state, move |s| {
        s.keys
            .set(dn_models::hub::HF_TOKEN_SECRET, &Secret::new(token))?;
        Ok(true)
    })
    .await
}

#[tauri::command]
pub async fn delete_hf_token(state: AppStateRef<'_>) -> IpcResult<bool> {
    blocking(&state, |s| {
        s.keys.delete(dn_models::hub::HF_TOKEN_SECRET)?;
        Ok(false)
    })
    .await
}

// ---- demios://models/add links (T-INF-12)

/// The add-model screen subscribes to links from the OS; returns one that arrived first.
#[tauri::command]
pub async fn watch_add_links(
    state: AppStateRef<'_>,
    on_event: Channel<dn_models::link::AddModelLink>,
) -> IpcResult<Option<dn_models::link::AddModelLink>> {
    Ok(state
        .add_links
        .watch(Arc::new(move |l| on_event.send(l).is_ok())))
}

/// The link's confirmation was closed: the next link may open it again.
#[tauri::command]
pub async fn add_link_done(state: AppStateRef<'_>) -> IpcResult<()> {
    state.add_links.done();
    Ok(())
}

/// What a link asks for, resolved for the confirmation screen (no download).
#[tauri::command]
pub async fn hub_resolve_link(
    state: AppStateRef<'_>,
    link: dn_models::link::AddModelLink,
) -> IpcResult<crate::hub::RepoView> {
    let hub = blocking(&state, |s| crate::hub::client(&s.keys)).await?;
    let local = local_models(&state)?;
    crate::hub::resolve_link(&hub, &local, &state.hub_pulls, &link).await
}

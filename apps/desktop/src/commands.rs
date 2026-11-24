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
use std::sync::Arc;
use tauri::State;
use tauri::ipc::Channel;

type AppStateRef<'a> = State<'a, Arc<AppState>>;

async fn blocking<T: Send + 'static>(
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
}

#[tauri::command]
pub async fn app_info(state: AppStateRef<'_>) -> IpcResult<AppInfo> {
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        dev_fake_model: cfg!(fake_model),
        data_dir: crate::data_dir().map(|p| p.display().to_string()),
        models_dir: state
            .models
            .local
            .as_ref()
            .map(|l| l.dir().display().to_string()),
    })
}

/// What "Delete all local data" removes, for the confirm dialog.
#[derive(Debug, Serialize)]
pub struct LocalDataSummary {
    pub conversations: usize,
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
        let (conversations, artifacts) = {
            let db = s.db()?;
            let n = store::list_conversations(&db)?.len();
            let a = dn_artifacts::ArtifactStore::new(&db)
                .counts()
                .map(|(a, _)| a)
                .map_err(|e| {
                    eprintln!("could not count artifacts: {e}");
                    IpcError::new("storage", "Could not read local data.")
                })?;
            (n, a)
        };
        let local = s.models.local.as_ref();
        Ok(LocalDataSummary {
            conversations,
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
pub async fn create_conversation(state: AppStateRef<'_>) -> IpcResult<Conversation> {
    blocking(&state, |s| s.create_conversation()).await
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
        // No screen edits MCP servers yet; never let a settings save drop them.
        settings.mcp_servers = s.settings()?.mcp_servers;
        s.save_settings(&settings)?;
        s.settings()
    })
    .await
}

fn catalog(s: &AppState) -> IpcResult<ModelCatalog> {
    let settings = s.settings()?;
    let mut c = models::catalog(&s.models, &s.keys, &settings);
    c.chat_model = s
        .agent
        .resolve_model(&settings, &s.models, &s.keys)
        .ok()
        .map(|m| m.to_string());
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
    Ok(local(&state)?.watch(Arc::new(move |ev| {
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

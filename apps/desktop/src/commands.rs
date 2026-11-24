//! Typed IPC commands. Each one is listed in `build.rs` and granted explicitly in
//! `capabilities/default.json`; nothing else is callable from the webview.
//! The TypeScript side mirrors these signatures in `ui/src/ipc/commands.ts`.

use crate::chat::{self, ChatEvent, TurnStarted};
use crate::error::{IpcError, IpcResult};
use crate::models::{self, ModelCatalog};
use crate::secrets::provider_account;
use crate::settings::Settings;
use crate::state::AppState;
use dn_core::Id;
use dn_store::chat::{self as store, Conversation, StoredMessage};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;
use tauri::ipc::Channel;

type AppStateRef<'a> = State<'a, Arc<AppState>>;

#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub dev_fake_model: bool,
    pub data_dir: Option<String>,
}

#[tauri::command]
pub async fn app_info() -> IpcResult<AppInfo> {
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        dev_fake_model: cfg!(feature = "dev-fake-model"),
        data_dir: crate::data_dir().map(|p| p.display().to_string()),
    })
}

#[tauri::command]
pub async fn list_conversations(state: AppStateRef<'_>) -> IpcResult<Vec<Conversation>> {
    Ok(store::list_conversations(&*state.db()?)?)
}

#[tauri::command]
pub async fn create_conversation(state: AppStateRef<'_>) -> IpcResult<Conversation> {
    state.create_conversation()
}

#[tauri::command]
pub async fn rename_conversation(
    state: AppStateRef<'_>,
    conversation_id: Id,
    title: String,
) -> IpcResult<()> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(IpcError::invalid("Title must be 1–200 characters."));
    }
    Ok(store::rename_conversation(
        &*state.db()?,
        conversation_id,
        title,
    )?)
}

#[tauri::command]
pub async fn delete_conversation(state: AppStateRef<'_>, conversation_id: Id) -> IpcResult<()> {
    chat::stop(&state, conversation_id);
    Ok(store::delete_conversation(&*state.db()?, conversation_id)?)
}

#[tauri::command]
pub async fn list_messages(
    state: AppStateRef<'_>,
    conversation_id: Id,
) -> IpcResult<Vec<StoredMessage>> {
    Ok(store::list_messages(&*state.db()?, conversation_id)?)
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
    chat::send(&state, conversation_id, text, channel_sink(on_event))
}

#[tauri::command]
pub async fn regenerate(
    state: AppStateRef<'_>,
    conversation_id: Id,
    on_event: Channel<ChatEvent>,
) -> IpcResult<TurnStarted> {
    chat::regenerate(&state, conversation_id, channel_sink(on_event))
}

#[tauri::command]
pub async fn stop_generation(state: AppStateRef<'_>, conversation_id: Id) -> IpcResult<bool> {
    Ok(chat::stop(&state, conversation_id))
}

#[tauri::command]
pub async fn get_settings(state: AppStateRef<'_>) -> IpcResult<Settings> {
    state.settings()
}

#[tauri::command]
pub async fn update_settings(state: AppStateRef<'_>, settings: Settings) -> IpcResult<Settings> {
    state.save_settings(&settings)?;
    state.settings()
}

#[tauri::command]
pub async fn list_models(state: AppStateRef<'_>) -> IpcResult<ModelCatalog> {
    let settings = state.settings()?;
    Ok(models::catalog(&state.models, state.keys.as_ref(), &settings))
}

#[tauri::command]
pub async fn set_provider_key(
    state: AppStateRef<'_>,
    provider_id: String,
    api_key: String,
) -> IpcResult<ModelCatalog> {
    if models::provider(&provider_id).is_none() {
        return Err(IpcError::invalid("Unknown provider."));
    }
    let key = api_key.trim();
    if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_whitespace) {
        return Err(IpcError::invalid(
            "Paste the key as one line without spaces.",
        ));
    }
    state.keys.set(&provider_account(&provider_id), key)?;
    list_models(state).await
}

#[tauri::command]
pub async fn delete_provider_key(
    state: AppStateRef<'_>,
    provider_id: String,
) -> IpcResult<ModelCatalog> {
    if models::provider(&provider_id).is_none() {
        return Err(IpcError::invalid("Unknown provider."));
    }
    state.keys.delete(&provider_account(&provider_id))?;
    // A slot pointing at this provider would now fail; clear it rather than fall back.
    let mut settings = state.settings()?;
    let prefix = format!("byok:{provider_id}:");
    settings.slots.retain(|_, m| !m.starts_with(&prefix));
    state.save_settings(&settings)?;
    list_models(state).await
}

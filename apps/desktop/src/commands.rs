//! Typed IPC commands. Each one is listed in `build.rs` and granted explicitly in
//! `capabilities/default.json`; nothing else is callable from the webview.
//! The TypeScript side mirrors these signatures in `ui/src/ipc/commands.ts`.
//!
//! SQLite and the key store are blocking, so every command that touches them runs on a
//! blocking thread ([`blocking`]) instead of a tokio worker.

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
}

#[tauri::command]
pub async fn app_info() -> IpcResult<AppInfo> {
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        dev_fake_model: cfg!(fake_model),
        data_dir: crate::data_dir().map(|p| p.display().to_string()),
    })
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
pub async fn update_settings(state: AppStateRef<'_>, settings: Settings) -> IpcResult<Settings> {
    blocking(&state, move |s| {
        s.save_settings(&settings)?;
        s.settings()
    })
    .await
}

fn catalog(s: &AppState) -> IpcResult<ModelCatalog> {
    let settings = s.settings()?;
    Ok(models::catalog(&s.models, s.keys.as_ref(), &settings))
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
        s.keys.set(&provider_account(&provider_id), &key)?;
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
        let account = provider_account(&provider_id);
        let secret = s.keys.get(&account)?;
        s.keys.delete(&account)?;
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
        s.keys.set(&provider_account(&provider_id), &secret)?;
        let mut settings = s.settings()?;
        for (slot, model) in slots {
            settings.slots.entry(slot).or_insert(model);
        }
        s.save_settings(&settings)?;
        catalog(s)
    })
    .await
}

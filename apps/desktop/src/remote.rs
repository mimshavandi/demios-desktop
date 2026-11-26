//! OpenAI-compatible servers (T-PRV-05): Ollama, LM Studio, llama.cpp `llama-server`, vLLM
//! or a hosted gateway, added by base URL with an optional key in the keychain.
//!
//! - The address must pass [`dn_providers::check_server_url`]: https, or http only for this
//!   computer and the local network. Redirects never leave the server's origin.
//! - Adding or refreshing lists the server's models and what it says about tool calls.
//!   A model whose server doesn't say gets no tools until the user turns them on; nothing
//!   falls back to prompt-based tool calls or non-streaming answers.
//! - Chat goes through `dn-providers`' OpenAI-compatible client (see `ModelRegistry`).

use crate::commands::{AppStateRef, catalog};
use crate::error::{IpcError, IpcResult};
use crate::models::{ModelCatalog, remote_config};
use crate::secrets::{Keys, Secret, SecretError};
use crate::settings::{RemoteModel, RemoteServer, remote_key_id};
use crate::state::AppState;
use dn_core::CoreError;
use dn_providers::{ProviderConfig, ProviderKind, ServerFamily};
use std::sync::Arc;

const MAX_NAME: usize = 60;

#[tauri::command]
pub async fn add_remote_server(
    state: AppStateRef<'_>,
    name: String,
    base_url: String,
    api_key: Option<String>,
) -> IpcResult<ModelCatalog> {
    let url = dn_providers::check_server_url(&base_url).map_err(|e| IpcError::invalid(e.0))?;
    let key = clean_key(api_key)?;
    let name = clean_name(&name, url.host_str().unwrap_or("Server"))?;
    let existing = blocking(&state, |s| Ok(s.settings()?.remote_servers)).await?;
    if let Some(r) = existing.iter().find(|r| r.base_url == url.as_str()) {
        return Err(IpcError::invalid(format!(
            "This server is already added as “{}”.",
            r.name
        )));
    }
    let id = unique_id(&name, &existing);
    let mut cfg =
        ProviderConfig::new(&id, ProviderKind::OpenAiCompatible).with_base_url(url.as_str());
    if let Some(k) = &key {
        cfg = cfg.with_key(k.clone());
    }
    let (family, models) = probe(cfg).await?;
    let server = RemoteServer {
        id,
        name,
        base_url: url.to_string(),
        family,
        models,
    };
    blocking(&state, move |s| save_new_server(s, server, key)).await
}

/// Stores a newly listed server and its key (if any) and returns the catalog.
fn save_new_server(
    s: &AppState,
    server: RemoteServer,
    key: Option<Secret>,
) -> IpcResult<ModelCatalog> {
    match &key {
        // No keychain: say how to get one. The key is never kept anywhere else.
        Some(k) => s.keys.set(&server.key_id(), k).map_err(key_store_error)?,
        // A key left behind under this id (an earlier server whose removal couldn't
        // delete it) must never be sent to this one.
        None => clear_key(&s.keys, &server.key_id())?,
    }
    let mut settings = s.settings()?;
    let key_id = server.key_id();
    settings.remote_servers.push(server);
    if let Err(e) = s.save_settings(&settings) {
        let _ = s.keys.delete(&key_id);
        return Err(e);
    }
    catalog(s)
}

/// Deletes any key stored for a server. With no keychain at all (`Unavailable`) no key can
/// have been stored, so there is nothing to delete: a server without a key works on a
/// system without a keychain. Every other keychain error (denied, failed) still fails.
fn clear_key(keys: &Keys, key_id: &str) -> Result<(), SecretError> {
    match keys.delete(key_id) {
        Ok(_) | Err(SecretError::Unavailable(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The key stored for a server; none when there is no keychain at all (see [`clear_key`]).
pub(crate) fn stored_key(keys: &Keys, key_id: &str) -> Result<Option<Secret>, SecretError> {
    match keys.get(key_id) {
        Err(SecretError::Unavailable(_)) => Ok(None),
        r => r,
    }
}

/// A server key can't be saved: with no keychain at all, name what to install.
fn key_store_error(e: SecretError) -> IpcError {
    match e {
        SecretError::Unavailable(_) => IpcError::new("key_store_unavailable", NO_KEYCHAIN),
        e => e.into(),
    }
}

#[cfg(target_os = "linux")]
const NO_KEYCHAIN: &str = "No system keychain found. Install GNOME Keyring or KWallet to save API keys, or add the server without a key.";
#[cfg(not(target_os = "linux"))]
const NO_KEYCHAIN: &str = "Your system keychain isn't available, so the API key can't be saved. Add the server without a key, or try again.";

/// Lists the server's models again (new or removed models, changed tool support). The
/// user's tool-call answers are kept for models still there.
#[tauri::command]
pub async fn refresh_remote_server(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<ModelCatalog> {
    let id = server_id.clone();
    let (server, cfg) = blocking(&state, move |s| {
        let server = find(&s.settings()?.remote_servers, &id)?.clone();
        let cfg = remote_config(&server, &s.keys).map_err(core_error)?;
        Ok((server, cfg))
    })
    .await?;
    let (family, mut models) = probe(cfg).await?;
    for m in &mut models {
        m.tool_calls_user = server.model(&m.id).and_then(|o| o.tool_calls_user);
    }
    blocking(&state, move |s| {
        let mut settings = s.settings()?;
        let r = find_mut(&mut settings.remote_servers, &server_id)?;
        r.family = family;
        r.models = models;
        s.save_settings(&settings)?;
        s.models.invalidate(&remote_key_id(&server_id));
        catalog(s)
    })
    .await
}

/// Removes the server, its key and any task that used one of its models (the task then
/// shows "choose a model" instead of quietly using another). [`restore_remote_server`] puts
/// all of it back within [`crate::state::KEY_UNDO_WINDOW`].
#[tauri::command]
pub async fn remove_remote_server(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<ModelCatalog> {
    blocking(&state, move |s| {
        let mut settings = s.settings()?;
        let server = find(&settings.remote_servers, &server_id)?.clone();
        let key = stored_key(&s.keys, &server.key_id())?;
        clear_key(&s.keys, &server.key_id())?;
        settings.remote_servers.retain(|r| r.id != server_id);
        let prefix = format!("remote:{server_id}:");
        let cleared: Vec<(String, String)> = settings
            .slots
            .iter()
            .filter(|(_, m)| m.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        settings.slots.retain(|_, m| !m.starts_with(&prefix));
        s.save_settings(&settings)?;
        s.models.invalidate(&remote_key_id(&server_id));
        s.remember_removed_server(server, key, cleared);
        catalog(s)
    })
    .await
}

/// Undo for [`remove_remote_server`]: the server, its key and its tasks come back.
#[tauri::command]
pub async fn restore_remote_server(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<ModelCatalog> {
    blocking(&state, move |s| {
        let removed = s.take_removed_server(&server_id).ok_or_else(|| {
            IpcError::new(
                "expired",
                "The server can no longer be restored. Add it again.",
            )
        })?;
        let mut settings = s.settings()?;
        if settings
            .remote_servers
            .iter()
            .any(|r| r.id == server_id || r.base_url == removed.server.base_url)
        {
            return Err(IpcError::invalid(
                "This server was added again in the meantime.",
            ));
        }
        if let Some(k) = &removed.key {
            s.keys
                .set(&removed.server.key_id(), k)
                .map_err(key_store_error)?;
        }
        settings.remote_servers.push(removed.server);
        for (slot, model) in removed.slots {
            settings.slots.entry(slot).or_insert(model);
        }
        s.save_settings(&settings)?;
        catalog(s)
    })
    .await
}

/// Whether the server answers now: shown as a badge on its row.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ServerStatus {
    Connected {
        models: usize,
    },
    /// 401/403: the server wants a key, or the saved one was refused.
    NeedsKey {
        message: String,
    },
    Unreachable {
        message: String,
    },
}

/// Lists the server's models (no chat request) to see whether it answers. Bounded to 8 s.
#[tauri::command]
pub async fn check_remote_server(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<ServerStatus> {
    let cfg = blocking(&state, move |s| {
        let server = find(&s.settings()?.remote_servers, &server_id)?.clone();
        remote_config(&server, &s.keys).map_err(core_error)
    })
    .await?;
    let adapter = dn_providers::build(cfg).map_err(core_error)?;
    let r = tokio::time::timeout(std::time::Duration::from_secs(8), adapter.list_models()).await;
    Ok(match r {
        Ok(Ok(list)) => ServerStatus::Connected { models: list.len() },
        Ok(Err(e)) => status_for(&e),
        Err(_) => ServerStatus::Unreachable {
            message: "The server didn't answer within 8 seconds.".into(),
        },
    })
}

fn status_for(e: &CoreError) -> ServerStatus {
    let message = core_message(e);
    match e {
        CoreError::ModelUnavailable { recovery, .. }
            if recovery.contains(&dn_core::RecoveryAction::AddProviderKey) =>
        {
            ServerStatus::NeedsKey { message }
        }
        _ => ServerStatus::Unreachable { message },
    }
}

/// The user's answer for a model whose server doesn't say whether it makes tool calls.
/// Refused when the server does say: its answer wins.
#[tauri::command]
pub async fn set_remote_model_tools(
    state: AppStateRef<'_>,
    server_id: String,
    model_id: String,
    enabled: bool,
) -> IpcResult<ModelCatalog> {
    blocking(&state, move |s| {
        let mut settings = s.settings()?;
        let r = find_mut(&mut settings.remote_servers, &server_id)?;
        let m = r
            .models
            .iter_mut()
            .find(|m| m.id == model_id)
            .ok_or_else(|| {
                IpcError::invalid("This model isn't on the server any more. Refresh the list.")
            })?;
        if m.tool_calls.is_some() {
            return Err(IpcError::invalid(
                "The server already says whether this model makes tool calls.",
            ));
        }
        m.tool_calls_user = Some(enabled);
        s.save_settings(&settings)?;
        catalog(s)
    })
    .await
}

/// The server family and its models, or a clear error if it can't be reached or answers
/// with something other than an OpenAI-compatible model list.
async fn probe(mut cfg: ProviderConfig) -> IpcResult<(ServerFamily, Vec<RemoteModel>)> {
    let family = dn_providers::detect_server_family(&cfg).await;
    cfg.server_family = family;
    let adapter = dn_providers::build(cfg).map_err(core_error)?;
    let list = adapter.list_models().await.map_err(|e| {
        IpcError::new(
            "server_unreachable",
            format!("Couldn't list the server's models. {}", core_message(&e)),
        )
    })?;
    let models = list
        .into_iter()
        .map(|m| {
            let embedding = m
                .embedding
                .unwrap_or_else(|| m.id.to_ascii_lowercase().contains("embed"));
            RemoteModel {
                id: m.id,
                // An embedding model never makes tool calls.
                tool_calls: if embedding { Some(false) } else { m.tool_calls },
                tool_calls_user: None,
                embedding,
            }
        })
        .collect();
    Ok((family, models))
}

fn find<'a>(servers: &'a [RemoteServer], id: &str) -> IpcResult<&'a RemoteServer> {
    servers
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| IpcError::invalid("This server was already removed."))
}

fn find_mut<'a>(servers: &'a mut [RemoteServer], id: &str) -> IpcResult<&'a mut RemoteServer> {
    servers
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or_else(|| IpcError::invalid("This server was already removed."))
}

fn clean_key(key: Option<String>) -> IpcResult<Option<Secret>> {
    let Some(k) = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) else {
        return Ok(None);
    };
    if k.len() > 4096 || k.chars().any(char::is_whitespace) {
        return Err(IpcError::invalid(
            "Paste the key as one line without spaces.",
        ));
    }
    Ok(Some(Secret::new(k)))
}

fn clean_name(name: &str, fallback: &str) -> IpcResult<String> {
    let n = name.trim();
    let n = if n.is_empty() { fallback } else { n };
    if n.chars().count() > MAX_NAME || n.chars().any(char::is_control) {
        return Err(IpcError::invalid(format!(
            "Use a name of up to {MAX_NAME} characters."
        )));
    }
    Ok(n.to_string())
}

/// A slug of `name` (`My Ollama` → `my-ollama`) not used by another server.
fn unique_id(name: &str, existing: &[RemoteServer]) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let mut base: String = slug.trim_end_matches('-').chars().take(24).collect();
    base = base.trim_end_matches('-').to_string();
    if base.is_empty() {
        base = "server".into();
    }
    let taken = |id: &str| existing.iter().any(|r| r.id == id);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("an unused id")
}

fn core_message(e: &CoreError) -> String {
    match e {
        CoreError::ModelUnavailable { user_message, .. } => user_message.clone(),
        other => other.to_string(),
    }
}

fn core_error(e: CoreError) -> IpcError {
    IpcError::new("server", core_message(&e))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: &str) -> RemoteServer {
        RemoteServer {
            id: id.into(),
            name: id.into(),
            base_url: "http://localhost:11434/v1".into(),
            family: ServerFamily::Unknown,
            models: vec![],
        }
    }

    /// A keychain that always fails with one error, like Linux without a Secret Service.
    struct Broken(fn() -> SecretError);
    impl dn_secrets::SecretStore for Broken {
        fn get(&self, _: &dn_secrets::SecretId) -> Result<Option<Secret>, SecretError> {
            Err((self.0)())
        }
        fn set(&self, _: &dn_secrets::SecretId, _: &Secret) -> Result<(), SecretError> {
            Err((self.0)())
        }
        fn delete(&self, _: &dn_secrets::SecretId) -> Result<bool, SecretError> {
            Err((self.0)())
        }
    }

    fn state_with(err: fn() -> SecretError) -> (AppState, Keys) {
        let keys = Keys {
            store: Arc::new(Broken(err)),
            description: "broken (tests)",
        };
        let conn = dn_store::open_in_memory(&crate::state::migration_sets()).unwrap();
        let st = AppState::new(conn, crate::models::ModelRegistry::default(), keys.clone());
        (st, keys)
    }

    fn no_keychain() -> SecretError {
        SecretError::Unavailable("no Secret Service".into())
    }

    #[test]
    fn a_server_without_a_key_needs_no_keychain() {
        let (st, keys) = state_with(no_keychain);
        let c = save_new_server(&st, server("ollama"), None).expect("added without a keychain");
        assert_eq!(c.remote_servers.len(), 1);
        // The key state is unknown, with the keychain's reason, not "no key" (#200).
        assert_eq!(c.remote_servers[0].key, crate::secrets::KeyState::Unknown);
        assert!(matches!(
            c.keychain,
            crate::secrets::KeychainStatus::Unavailable { .. }
        ));
        // Chat builds its config without a key, and the server can be removed again.
        let cfg = crate::models::remote_config(&server("ollama"), &keys).expect("no key needed");
        assert!(cfg.api_key.is_none());
        assert!(
            stored_key(&keys, &server("ollama").key_id())
                .unwrap()
                .is_none()
        );
        clear_key(&keys, &server("ollama").key_id()).expect("nothing stored, nothing to delete");
    }

    #[test]
    fn a_key_without_a_keychain_is_refused_and_says_how_to_fix_it() {
        let (st, _) = state_with(no_keychain);
        let e = save_new_server(&st, server("vllm"), Some(Secret::new("k"))).unwrap_err();
        assert_eq!(e.code, "key_store_unavailable");
        #[cfg(target_os = "linux")]
        assert!(
            e.message.contains("GNOME Keyring or KWallet"),
            "{}",
            e.message
        );
        // Not saved anywhere else, and the server isn't added without its key.
        assert!(st.settings().unwrap().remote_servers.is_empty());
    }

    #[test]
    fn other_keychain_errors_still_fail() {
        // Only "no keychain at all" means nothing was stored; a denied or failing keychain
        // may still hold an old key, which must not be sent to a new server.
        let (st, keys) = state_with(|| SecretError::Denied("locked".into()));
        let e = save_new_server(&st, server("ollama"), None).unwrap_err();
        assert_eq!(e.code, "key_store_denied");
        assert!(st.settings().unwrap().remote_servers.is_empty());
        assert!(stored_key(&keys, "x").is_err());
        assert!(crate::models::remote_config(&server("ollama"), &keys).is_err());
    }

    #[test]
    fn ids_are_slugs_and_unique() {
        assert_eq!(unique_id("My Ollama", &[]), "my-ollama");
        assert_eq!(unique_id("  LM Studio (Mac) ", &[]), "lm-studio-mac");
        assert_eq!(unique_id("ÄÖÜ", &[]), "server");
        assert_eq!(unique_id("ollama", &[server("ollama")]), "ollama-2");
        assert_eq!(
            unique_id("ollama", &[server("ollama"), server("ollama-2")]),
            "ollama-3"
        );
        let long = unique_id("a very long server name that goes on", &[]);
        assert!(
            long.len() <= 24 && crate::settings::valid_server_id(&long),
            "{long}"
        );
    }

    #[test]
    fn status_tells_a_missing_key_from_an_unreachable_server() {
        let unavailable = |recovery| CoreError::ModelUnavailable {
            user_message: "msg".into(),
            developer_message: "dev".into(),
            recovery,
        };
        assert!(matches!(
            status_for(&unavailable(vec![dn_core::RecoveryAction::AddProviderKey])),
            ServerStatus::NeedsKey { .. }
        ));
        assert!(matches!(
            status_for(&unavailable(vec![dn_core::RecoveryAction::Retry { after_seconds: None }])),
            ServerStatus::Unreachable { ref message } if message == "msg"
        ));
        let json = serde_json::to_value(ServerStatus::Connected { models: 2 }).unwrap();
        assert_eq!(json, serde_json::json!({"state": "connected", "models": 2}));
    }

    #[test]
    fn keys_and_names_are_checked() {
        assert!(clean_key(None).unwrap().is_none());
        assert!(clean_key(Some("  ".into())).unwrap().is_none());
        assert!(clean_key(Some("sk 1".into())).is_err());
        assert_eq!(
            clean_key(Some(" sk-1 ".into())).unwrap().unwrap().expose(),
            "sk-1"
        );
        assert_eq!(clean_name("", "localhost").unwrap(), "localhost");
        assert!(clean_name(&"x".repeat(61), "h").is_err());
    }
}

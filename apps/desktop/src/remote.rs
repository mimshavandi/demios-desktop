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
use crate::secrets::Secret;
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
    blocking(&state, move |s| {
        if let Some(k) = &key {
            s.keys.set(&server.key_id(), k)?;
        }
        let mut settings = s.settings()?;
        let key_id = server.key_id();
        settings.remote_servers.push(server);
        if let Err(e) = s.save_settings(&settings) {
            let _ = s.keys.delete(&key_id);
            return Err(e);
        }
        catalog(s)
    })
    .await
}

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
/// shows "choose a model" instead of quietly using another).
#[tauri::command]
pub async fn remove_remote_server(
    state: AppStateRef<'_>,
    server_id: String,
) -> IpcResult<ModelCatalog> {
    blocking(&state, move |s| {
        let mut settings = s.settings()?;
        find(&settings.remote_servers, &server_id)?;
        s.keys.delete(&remote_key_id(&server_id))?;
        settings.remote_servers.retain(|r| r.id != server_id);
        let prefix = format!("remote:{server_id}:");
        settings.slots.retain(|_, m| !m.starts_with(&prefix));
        s.save_settings(&settings)?;
        s.models.invalidate(&remote_key_id(&server_id));
        catalog(s)
    })
    .await
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

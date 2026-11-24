//! Demios Desktop shell (ADR-0002, T-SHELL-01..03). The Rust side owns all state and
//! logic; the web UI in `ui/` only renders and calls the typed commands in [`commands`].

// build.rs only sets `fake_model` for debug builds; make that impossible to undo by accident.
#[cfg(all(fake_model, not(debug_assertions)))]
compile_error!("the dev fake model must never be in a release build");

pub mod agent;
pub mod chat;
pub mod commands;
pub mod error;
#[cfg(any(fake_model, test))]
pub mod fake;
pub mod hub;
pub mod local;
pub mod mcp;
pub mod models;
pub mod secrets;
pub mod settings;
pub mod state;
pub mod support;

use std::path::PathBuf;

/// Where the app keeps its database. `DEMIOS_DATA_DIR` overrides the per-OS default
/// (useful for a throwaway profile while developing).
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("DEMIOS_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dn_store::paths::app_data_dir(dn_store::paths::APP_ID))
}

/// Whether the app window may load `url`: only the app's own origin (and the Vite dev
/// server in debug builds). Links in model output never navigate the window.
pub fn allowed_navigation(url: &tauri::Url) -> bool {
    let host = url.host_str().unwrap_or("");
    match url.scheme() {
        // macOS / Linux custom protocol.
        "tauri" => host == "localhost",
        // Windows serves the app from http(s)://tauri.localhost.
        "http" | "https" if host == "tauri.localhost" => true,
        "http" if cfg!(debug_assertions) => {
            (host == "localhost" || host == "127.0.0.1") && url.port() == Some(1420)
        }
        _ => false,
    }
}

fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|_webview, url| {
            let ok = allowed_navigation(url);
            if !ok {
                eprintln!("blocked navigation to {url}");
            }
            ok
        })
        .build()
}

/// Asks with a native dialog (outside the webview) before a local MCP server starts.
struct NativeLaunchApprover(tauri::AppHandle);

impl mcp::LaunchApprover for NativeLaunchApprover {
    fn ask(
        &self,
        request: mcp::LaunchRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.0
            .dialog()
            .message(request.dialog_text())
            .title("Start a tool server?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Allow and start".into(),
                "Don't start".into(),
            ))
            .show(move |allowed| {
                let _ = tx.send(allowed);
            });
        Box::pin(async move { rx.await.unwrap_or(false) })
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(navigation_guard())
        // Used from Rust only (`commands::report_problem`): no link-click handler is injected
        // into the webview, and none of its JS commands are granted (see capabilities).
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        // Native "start this program?" confirm for local MCP servers, shown from Rust only;
        // none of the dialog plugin's JS commands are granted (see capabilities).
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::Manager;
            let dir = data_dir().ok_or("no home directory to keep app data in")?;
            let resources = app.path().resource_dir().ok();
            let skills = agent::skills_dir(resources.as_deref());
            let state = state::AppState::open_with(
                &dir.join(dn_store::paths::DB_FILE),
                skills.as_deref(),
                Some(std::sync::Arc::new(NativeLaunchApprover(
                    app.handle().clone(),
                ))),
            )
            .map_err(|e| {
                format!(
                    "could not open the local database in {}: {e}",
                    dir.display()
                )
            })?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::delete_all_local_data,
            commands::local_data_summary,
            commands::report_problem,
            commands::list_mcp_servers,
            commands::save_mcp_server,
            commands::remove_mcp_server,
            commands::reconnect_mcp_server,
            commands::sign_in_mcp_server,
            commands::sign_out_mcp_server,
            commands::hub_search,
            commands::hub_repo,
            commands::hub_pull,
            commands::hub_cancel,
            commands::hf_token_status,
            commands::set_hf_token,
            commands::delete_hf_token,
            commands::list_conversations,
            commands::create_conversation,
            commands::rename_conversation,
            commands::delete_conversation,
            commands::restore_conversation,
            commands::list_messages,
            commands::send_message,
            commands::regenerate,
            commands::stop_generation,
            commands::get_settings,
            commands::update_settings,
            commands::list_models,
            commands::set_provider_key,
            commands::delete_provider_key,
            commands::restore_provider_key,
            commands::watch_tools,
            commands::respond_approval,
            commands::list_unknown_outcomes,
            commands::resolve_unknown_outcome,
            commands::watch_local_models,
            commands::download_model,
            commands::pause_download,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Demios Desktop")
        .run(|app, event| {
            // Tauri exits without dropping managed state: stop MCP servers (and their
            // process trees) explicitly, with a bound so a stuck server can't hang quit.
            if let tauri::RunEvent::Exit = event {
                use tauri::Manager;
                // Downloads stop; their partial files resume next time.
                if let Some(local) = app
                    .try_state::<std::sync::Arc<state::AppState>>()
                    .and_then(|s| s.models.local.clone())
                {
                    local.pause_all();
                }
                let executor = app
                    .try_state::<std::sync::Arc<state::AppState>>()
                    .and_then(|s| s.executor.clone());
                if let Some(executor) = executor {
                    // Concurrent, each server bounded on its own (one stuck server can't
                    // keep the others alive or hang quit).
                    let stuck = tauri::async_runtime::block_on(
                        executor.shutdown_all(std::time::Duration::from_millis(1500)),
                    );
                    if stuck > 0 {
                        eprintln!("{stuck} MCP server(s) did not stop in time");
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::allowed_navigation;

    #[test]
    fn navigation_is_limited_to_the_app() {
        for ok in [
            "tauri://localhost/",
            "http://tauri.localhost/index.html",
            "https://tauri.localhost/",
        ] {
            assert!(allowed_navigation(&ok.parse().unwrap()), "{ok}");
        }
        for bad in [
            "https://example.com/",
            "http://localhost:8080/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "tauri://evil/",
        ] {
            assert!(!allowed_navigation(&bad.parse().unwrap()), "{bad}");
        }
        assert_eq!(
            allowed_navigation(&"http://localhost:1420/".parse().unwrap()),
            cfg!(debug_assertions)
        );
    }

    /// The webview gets only the app's own commands: no plugin permission at all, in
    /// particular none of the opener's (it is used from Rust only).
    #[test]
    fn capabilities_grant_no_plugin_permissions() {
        let caps: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let perms = caps["permissions"].as_array().unwrap();
        assert!(!perms.is_empty());
        for p in perms {
            let id = p
                .as_str()
                .or_else(|| p["identifier"].as_str())
                .unwrap_or_default();
            assert!(
                !id.starts_with("opener:"),
                "opener permission granted: {id}"
            );
            assert!(
                id.starts_with("allow-") || id.starts_with("core:window:allow-start-dragging"),
                "unexpected permission: {id}"
            );
        }
    }
}

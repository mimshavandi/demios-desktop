//! Demios Desktop shell (ADR-0002, T-SHELL-01..03). The Rust side owns all state and
//! logic; the web UI in `ui/` only renders and calls the typed commands in [`commands`].

// build.rs only sets `fake_model` for debug builds; make that impossible to undo by accident.
#[cfg(all(fake_model, not(debug_assertions)))]
compile_error!("the dev fake model must never be in a release build");

pub mod chat;
pub mod commands;
pub mod error;
#[cfg(any(fake_model, test))]
pub mod fake;
pub mod models;
pub mod secrets;
pub mod settings;
pub mod state;

use std::path::PathBuf;
use std::sync::Arc;

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

pub fn run() {
    let dir = data_dir().expect("no home directory to keep app data in");
    let state = state::AppState::open(&dir.join(dn_store::paths::DB_FILE)).unwrap_or_else(|e| {
        panic!(
            "could not open the local database in {}: {e}",
            dir.display()
        )
    });

    tauri::Builder::default()
        .plugin(navigation_guard())
        .manage(Arc::new(state))
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Demios Desktop");
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
}

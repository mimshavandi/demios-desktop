//! Demios Desktop shell (ADR-0002, T-SHELL-01..03). The Rust side owns all state and
//! logic; the web UI in `ui/` only renders and calls the typed commands in [`commands`].

pub mod chat;
pub mod commands;
pub mod error;
#[cfg(feature = "dev-fake-model")]
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

pub fn run() {
    let dir = data_dir().expect("no home directory to keep app data in");
    let state = state::AppState::open(&dir.join(dn_store::paths::DB_FILE))
        .unwrap_or_else(|e| panic!("could not open the local database in {}: {e}", dir.display()));

    tauri::Builder::default()
        .manage(Arc::new(state))
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::list_conversations,
            commands::create_conversation,
            commands::rename_conversation,
            commands::delete_conversation,
            commands::list_messages,
            commands::send_message,
            commands::regenerate,
            commands::stop_generation,
            commands::get_settings,
            commands::update_settings,
            commands::list_models,
            commands::set_provider_key,
            commands::delete_provider_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Demios Desktop");
}

// Every IPC command is listed here so Tauri generates an `allow-<command>` permission
// for it; only the ones granted in `capabilities/default.json` are callable (ADR-0009).
const COMMANDS: &[&str] = &[
    "app_info",
    "list_conversations",
    "create_conversation",
    "rename_conversation",
    "delete_conversation",
    "list_messages",
    "send_message",
    "regenerate",
    "stop_generation",
    "get_settings",
    "update_settings",
    "list_models",
    "set_provider_key",
    "delete_provider_key",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri build script failed");
}

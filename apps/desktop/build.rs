// Every IPC command is listed here so Tauri generates an `allow-<command>` permission
// for it; only the ones granted in `capabilities/default.json` are callable (ADR-0009).
const COMMANDS: &[&str] = &[
    "app_info",
    "delete_all_local_data",
    "local_data_summary",
    "report_problem",
    "list_mcp_servers",
    "save_mcp_server",
    "remove_mcp_server",
    "reconnect_mcp_server",
    "sign_in_mcp_server",
    "sign_out_mcp_server",
    "hub_search",
    "hub_repo",
    "hub_pull",
    "hub_cancel",
    "hub_pull_status",
    "hf_token_status",
    "set_hf_token",
    "delete_hf_token",
    "list_conversations",
    "create_conversation",
    "rename_conversation",
    "delete_conversation",
    "restore_conversation",
    "list_messages",
    "send_message",
    "regenerate",
    "stop_generation",
    "get_settings",
    "update_settings",
    "list_models",
    "set_provider_key",
    "delete_provider_key",
    "restore_provider_key",
    "watch_tools",
    "respond_approval",
    "list_unknown_outcomes",
    "resolve_unknown_outcome",
    "watch_local_models",
    "download_model",
    "pause_download",
];

fn main() {
    // The dev fake model (T-SHELL-03) exists only with the feature *and* in debug builds.
    println!("cargo::rustc-check-cfg=cfg(fake_model)");
    if std::env::var_os("CARGO_FEATURE_DEV_FAKE_MODEL").is_some()
        && std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some()
    {
        println!("cargo::rustc-cfg=fake_model");
    }
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri build script failed");
}

// Every IPC command is listed here so Tauri generates an `allow-<command>` permission
// for it; only the ones granted in `capabilities/default.json` are callable (ADR-0009).
const COMMANDS: &[&str] = &[
    "app_info",
    "delete_all_local_data",
    "local_data_summary",
    "report_problem",
    "open_link",
    "list_mcp_servers",
    "save_mcp_server",
    "remove_mcp_server",
    "reconnect_mcp_server",
    "sign_in_mcp_server",
    "list_mcp_tools",
    "set_mcp_tool_choice",
    "mark_mcp_tools_reviewed",
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
    "list_artifacts",
    "get_artifact",
    "rollback_artifact",
    "watch_local_models",
    "download_model",
    "pause_download",
];

/// Copies the artifact frame shell (`ui/artifact-sandbox/dist/frame/`) into OUT_DIR for
/// `include_str!` in `src/artifacts.rs`. Without a sandbox build, a placeholder page is used so
/// `cargo test` works on its own; a bundled build (`custom-protocol`) must have the real one.
fn artifact_frame() {
    use std::path::PathBuf;
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let frame = manifest.join("../../ui/artifact-sandbox/dist/frame");
    let html = frame.join("index.html");
    let csp = frame.join("csp.json");
    println!("cargo::rerun-if-changed={}", html.display());
    println!("cargo::rerun-if-changed={}", csp.display());
    let (h, c) = match (
        std::fs::read_to_string(&html),
        std::fs::read_to_string(&csp),
    ) {
        (Ok(h), Ok(c)) => (h, c),
        _ => {
            if std::env::var_os("CARGO_FEATURE_CUSTOM_PROTOCOL").is_some() {
                panic!(
                    "the artifact sandbox is not built ({} missing): run `npm run sandbox` in apps/desktop",
                    html.display()
                );
            }
            println!(
                "cargo::warning=artifact sandbox not built; artifacts show a placeholder (npm run sandbox in apps/desktop)"
            );
            (
                "<!-- placeholder --><!doctype html><meta charset=\"utf-8\"><p>The artifact renderer is not built.</p>\n".to_string(),
                r#"{"strict":"default-src 'none'; style-src 'unsafe-inline'","network":"default-src 'none'; style-src 'unsafe-inline'"}"#.to_string(),
            )
        }
    };
    std::fs::write(out.join("artifact-frame.html"), h).expect("write frame shell");
    std::fs::write(out.join("artifact-frame-csp.json"), c).expect("write frame csp");
}

fn main() {
    artifact_frame();
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

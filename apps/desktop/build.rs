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
    "watch_add_links",
    "add_link_done",
    "hub_resolve_link",
    "hf_token_status",
    "set_hf_token",
    "delete_hf_token",
    "list_conversations",
    "create_conversation",
    "rename_conversation",
    "delete_conversation",
    "restore_conversation",
    "list_projects",
    "create_project",
    "update_project",
    "delete_project",
    "restore_project",
    "list_project_files",
    "add_project_files",
    "remove_project_file",
    "restore_project_file",
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
    "check_for_update",
    "install_update",
    // Knowledge search (knowledge.rs, #116): what an answer was given.
    "message_sources",
    // Windows artifact webview (artifact_child.rs, ADR-0012); main webview only.
    "artifact_child_open",
    "artifact_child_render",
    "artifact_child_bounds",
    "artifact_child_focus",
    "artifact_child_close",
    // Dev fake-model builds only (they refuse otherwise): the e2e isolation checks.
    "artifact_child_probe",
    "artifact_child_probe_report",
    "artifact_child_state",
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

    // Host page of the Windows artifact webview (`dist/host/`, artifact_child.rs).
    let host = manifest.join("../../ui/artifact-sandbox/dist/host");
    let (host_html, host_js) = (host.join("index.html"), host.join("host.js"));
    println!("cargo::rerun-if-changed={}", host_html.display());
    println!("cargo::rerun-if-changed={}", host_js.display());
    let (hh, hj) = match (
        std::fs::read_to_string(&host_html),
        std::fs::read_to_string(&host_js),
    ) {
        (Ok(h), Ok(j)) => (h, j),
        _ => {
            if std::env::var_os("CARGO_FEATURE_CUSTOM_PROTOCOL").is_some() {
                panic!(
                    "the artifact sandbox is not built ({} missing): run `npm run sandbox` in apps/desktop",
                    host_html.display()
                );
            }
            (
                "<!-- placeholder --><!doctype html><meta charset=\"utf-8\"><p>The artifact renderer is not built.</p>\n".to_string(),
                String::new(),
            )
        }
    };
    std::fs::write(out.join("artifact-host.html"), hh).expect("write host page");
    std::fs::write(out.join("artifact-host.js"), hj).expect("write host script");
}

// The distribution guard (T-STORE-08), shared with the app as `crate::distribution`.
include!("src/distribution.rs");

fn main() {
    artifact_frame();
    // Store (MSIX) and Flathub builds must not contain the self-updater.
    println!("cargo::rerun-if-env-changed=DEMIOS_DISTRIBUTION");
    println!("cargo::rerun-if-env-changed=DEMIOS_UPDATER_PUBKEY");
    let distribution = std::env::var("DEMIOS_DISTRIBUTION").ok();
    if let Err(why) = check_build(
        std::env::var_os("CARGO_FEATURE_UPDATER").is_some(),
        distribution.as_deref(),
    ) {
        panic!("{why}");
    }
    // Shown on the About screen (`app_info`); set by the preview workflow.
    println!("cargo::rerun-if-env-changed=DEMIOS_BUILD_COMMIT");
    println!("cargo::rerun-if-env-changed=DEMIOS_BUILD_CHANNEL");
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

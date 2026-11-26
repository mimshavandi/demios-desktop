//! Demios Desktop shell (ADR-0002, T-SHELL-01..03). The Rust side owns all state and
//! logic; the web UI in `ui/` only renders and calls the typed commands in [`commands`].

// build.rs only sets `fake_model` for debug builds; make that impossible to undo by accident.
#[cfg(all(fake_model, not(debug_assertions)))]
compile_error!("the dev fake model must never be in a release build");

pub mod agent;
pub mod artifact_child;
pub mod artifacts;
pub mod chat;
pub mod commands;
pub mod deeplink;
pub mod distribution;
pub mod error;
#[cfg(any(fake_model, test))]
pub mod fake;
pub mod hub;
pub mod knowledge;
pub mod links;
pub mod local;
pub mod mcp;
pub mod models;
pub mod projects;
pub mod remote;
pub mod secrets;
pub mod settings;
pub mod skills;
pub mod state;
pub mod support;
pub mod tool_probe;
pub mod tool_review;
pub mod updates;
pub mod window_fit;
pub mod window_state;

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
        // The artifact frame shell (T-ART-06): WebKitGTK and WebView2 also ask about sub-frame
        // navigations. Only the shell's own document; should the main window ever land there,
        // the shell removes the IPC globals and refuses to run (it renders only for a parent).
        "artifact" => host == "localhost" && artifacts::is_frame_path(url.path()),
        "http" if host == "artifact.localhost" => artifacts::is_frame_path(url.path()),
        "http" if cfg!(debug_assertions) => {
            (host == "localhost" || host == "127.0.0.1") && url.port() == Some(1420)
        }
        _ => false,
    }
}

fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            let ok = if webview.label() == artifact_child::LABEL {
                artifact_child::allowed_navigation(url)
            } else {
                allowed_navigation(url)
            };
            if !ok {
                eprintln!("blocked navigation to {url}");
            }
            ok
        })
        .build()
}

/// Routes `demios://` links from the OS to the add-model confirmation screen, as data
/// (`deeplink::AddLinks::receive` parses them; nothing downloads from a link).
fn install_add_links(app: &tauri::AppHandle, state: &std::sync::Arc<state::AppState>) {
    use tauri_plugin_deep_link::DeepLinkExt;
    // Installers (.app, MSI/NSIS, .deb/.rpm, Flatpak) register the scheme. Only dev builds and
    // an AppImage (no installer) register it at run time, so a packaged release never
    // rewrites a .desktop file or the registry on each launch.
    #[cfg(any(target_os = "linux", windows))]
    if register_scheme_at_runtime(
        cfg!(debug_assertions),
        std::env::var_os("APPIMAGE").is_some(),
    ) && let Err(e) = app.deep_link().register_all()
    {
        eprintln!("could not register demios:// links: {e}");
    }
    let receive = {
        let state = std::sync::Arc::clone(state);
        move |url: &str| {
            if let Err(why) = state.add_links.receive(url) {
                eprintln!("ignored a demios:// link: {why:?}");
            }
        }
    };
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for u in urls {
            receive(u.as_str());
        }
    }
    app.deep_link().on_open_url(move |ev| {
        for u in ev.urls() {
            receive(u.as_str());
        }
    });
}

/// Whether the app registers `demios://` itself: dev builds, and an AppImage on Linux.
pub fn register_scheme_at_runtime(debug: bool, appimage: bool) -> bool {
    debug || (cfg!(target_os = "linux") && appimage)
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
        use tauri::Manager;
        let mut dialog = self.0.dialog().message(request.dialog_text());
        if let Some(w) = self.0.get_webview_window("main") {
            dialog = dialog.parent(&w);
        }
        dialog
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
    let builder = tauri::Builder::default()
        // First: a second launch (e.g. from a demios:// link) hands over to this one. With
        // the `deep-link` feature the link is forwarded to the deep-link plugin below; the
        // second launch's command line is otherwise ignored.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        // demios://models/add links, from Rust only (no JS commands granted).
        .plugin(tauri_plugin_deep_link::init())
        .plugin(navigation_guard())
        // The artifact frame origin (T-ART-06): serves only the sandbox frame shell.
        // On Windows also the artifact webview's host page (artifact_child.rs, ADR-0012).
        .register_uri_scheme_protocol(artifacts::SCHEME, |ctx, request| {
            let label = ctx.webview_label();
            artifact_child::host_response(ctx.app_handle(), label, &request).unwrap_or_else(|| {
                artifacts::frame_response(&request, label == artifact_child::LABEL)
            })
        })
        // Used from Rust only (`commands::report_problem`): no link-click handler is injected
        // into the webview, and none of its JS commands are granted (see capabilities).
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        // Native "start this program?" confirm for local MCP servers, shown from Rust only;
        // none of the dialog plugin's JS commands are granted (see capabilities).
        .plugin(tauri_plugin_dialog::init());
    // Signed self-update, direct-download builds only (T-STORE-08). Used from Rust only
    // (`updates::check_for_update` / `install_update`); none of its JS commands are granted.
    #[cfg(feature = "updater")]
    let builder = builder.plugin(updates::plugin());
    builder
        .manage(updates::Updates::default())
        .manage(window_state::Tracker::default())
        .on_window_event(window_state::on_event)
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
            install_add_links(app.handle(), &state);
            knowledge::start(&state);
            // The page's first commands need these, so they come before the window.
            app.manage(state.clone());
            app.manage(artifact_child::ChildState::default());
            // Create the window hidden, fit it to the screen's work area (or put it back
            // where it was), then show it (#172, #195).
            window_state::open(app.handle(), &state)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::delete_all_local_data,
            commands::local_data_summary,
            commands::report_problem,
            commands::open_link,
            commands::list_mcp_servers,
            commands::save_mcp_server,
            commands::remove_mcp_server,
            commands::reconnect_mcp_server,
            commands::sign_in_mcp_server,
            commands::list_mcp_tools,
            commands::set_mcp_tool_choice,
            commands::mark_mcp_tools_reviewed,
            commands::sign_out_mcp_server,
            commands::hub_search,
            commands::hub_repo,
            commands::hub_pull,
            commands::hub_cancel,
            commands::hub_pull_status,
            commands::watch_add_links,
            commands::add_link_done,
            commands::hub_resolve_link,
            commands::hf_token_status,
            commands::set_hf_token,
            commands::delete_hf_token,
            commands::list_conversations,
            commands::create_conversation,
            commands::rename_conversation,
            commands::delete_conversation,
            commands::list_skills,
            commands::list_answer_skills,
            commands::skills_overview,
            commands::reload_skills,
            commands::open_skills_folder,
            commands::get_chat_skill,
            commands::set_chat_skill,
            commands::restore_conversation,
            projects::list_projects,
            projects::create_project,
            projects::update_project,
            projects::delete_project,
            projects::restore_project,
            projects::move_conversation,
            projects::list_project_files,
            projects::add_project_files,
            projects::remove_project_file,
            projects::restore_project_file,
            commands::list_messages,
            commands::send_message,
            commands::regenerate,
            commands::stop_generation,
            commands::get_settings,
            commands::update_settings,
            commands::list_models,
            commands::check_keys,
            commands::set_provider_key,
            remote::add_remote_server,
            remote::refresh_remote_server,
            remote::remove_remote_server,
            remote::restore_remote_server,
            remote::check_remote_server,
            remote::set_remote_model_tools,
            commands::delete_provider_key,
            commands::restore_provider_key,
            commands::watch_tools,
            commands::respond_approval,
            commands::list_unknown_outcomes,
            commands::resolve_unknown_outcome,
            commands::list_artifacts,
            commands::get_artifact,
            commands::rollback_artifact,
            commands::watch_local_models,
            commands::download_model,
            knowledge::message_sources,
            knowledge::knowledge_status,
            knowledge::knowledge_offer,
            artifact_child::artifact_child_open,
            artifact_child::artifact_child_render,
            artifact_child::artifact_child_bounds,
            artifact_child::artifact_child_focus,
            artifact_child::artifact_child_close,
            artifact_child::artifact_child_probe,
            artifact_child::artifact_child_probe_report,
            artifact_child::artifact_child_state,
            commands::pause_download,
            commands::remove_local_model,
            commands::cancel_download,
            commands::check_model_tools,
            updates::check_for_update,
            updates::install_update,
            updates::launch_update_check,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Demios Desktop")
        .run(|app, event| {
            // Tauri exits without dropping managed state: stop MCP servers (and their
            // process trees) explicitly, with a bound so a stuck server can't hang quit.
            // Save the window while it still exists (⌘Q on macOS closes it before Exit).
            if let tauri::RunEvent::ExitRequested { .. } = event {
                window_state::persist(app);
            }
            if let tauri::RunEvent::Exit = event {
                use tauri::Manager;
                window_state::persist(app);
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
            "artifact://localhost/frame/",
            "artifact://localhost/frame/?net=1",
            "http://artifact.localhost/frame/",
        ] {
            assert!(allowed_navigation(&ok.parse().unwrap()), "{ok}");
        }
        for bad in [
            "https://example.com/",
            "http://localhost:8080/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "tauri://evil/",
            "artifact://localhost/",
            "artifact://localhost/other",
            "artifact://evil/frame/",
            "https://artifact.localhost/frame/",
            "http://artifact.localhost/x",
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

    #[test]
    fn only_dev_builds_and_appimages_register_the_scheme_themselves() {
        assert!(super::register_scheme_at_runtime(true, false));
        assert!(
            !super::register_scheme_at_runtime(false, false),
            "installed releases don't"
        );
        assert_eq!(
            super::register_scheme_at_runtime(false, true),
            cfg!(target_os = "linux"),
            "an AppImage on Linux does"
        );
    }
}

#[cfg(test)]
mod capability_tests {
    /// The capability is granted to the main webview by its label, not to the `main` window:
    /// a child webview added to that window later (such as an artifact webview, #75) must not
    /// inherit the app's commands.
    #[test]
    fn capability_is_scoped_to_the_main_webview_only() {
        let cap: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert_eq!(cap["webviews"], serde_json::json!(["main"]));
        assert!(
            cap.get("windows").is_none(),
            "a `windows` entry would grant every webview in that window"
        );
        assert!(
            cap.get("remote").is_none(),
            "no remote origin may call app commands"
        );
        assert!(cap.get("local").is_none_or(|l| l == true));
    }

    /// Tauri's capability patterns: `*` matches any run of characters, `?` one character.
    fn glob_matches(pattern: &str, name: &str) -> bool {
        fn go(p: &[u8], n: &[u8]) -> bool {
            match (p.first(), n.first()) {
                (None, None) => true,
                (Some(b'*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
                (Some(b'?'), Some(_)) => go(&p[1..], &n[1..]),
                (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
                _ => false,
            }
        }
        go(pattern.as_bytes(), name.as_bytes())
    }

    #[test]
    fn glob_matching_follows_tauri() {
        assert!(glob_matches("*", "artifact"));
        assert!(glob_matches("art*", "artifact"));
        assert!(glob_matches("m?in", "main"));
        assert!(glob_matches("*a*", "main"));
        assert!(!glob_matches("main", "artifact"));
        assert!(!glob_matches("ma", "main"));
    }

    /// The Windows artifact webview's isolation is that no capability can reach it (ADR-0012):
    /// in **every** capability file, no `webviews` entry may match `artifact` (by name or
    /// pattern), and no `windows` entry may match the `main` window, which holds both webviews.
    #[test]
    fn no_capability_reaches_the_artifact_webview() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/capabilities");
        let mut files = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            files += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            let cap: serde_json::Value = serde_json::from_str(&text).unwrap();
            let names = |key: &str| -> Vec<String> {
                cap[key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|v| v.as_str().expect("capability names are strings").to_owned())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for w in names("webviews") {
                assert!(
                    !glob_matches(&w, crate::artifact_child::LABEL),
                    "{}: webviews entry {w:?} matches the artifact webview",
                    path.display()
                );
            }
            for w in names("windows") {
                assert!(
                    !glob_matches(&w, "main") && !glob_matches(&w, crate::artifact_child::LABEL),
                    "{}: windows entry {w:?} matches the main window, which holds the artifact webview",
                    path.display()
                );
            }
        }
        assert!(files > 0, "no capability files found in {dir}");
    }

    /// The main window is created by `window_state::open`, after setup, hidden until it is
    /// placed (#195). If Tauri built it from the config, the page would load before it is
    /// placed and the placement would undo a resize made meanwhile. The platform files
    /// replace the whole `windows` list, so each one has to say it.
    #[test]
    fn main_window_is_created_hidden_by_setup() {
        let files = [
            ("tauri.conf.json", include_str!("../tauri.conf.json")),
            (
                "tauri.windows.conf.json",
                include_str!("../tauri.windows.conf.json"),
            ),
            (
                "tauri.macos.conf.json",
                include_str!("../tauri.macos.conf.json"),
            ),
        ];
        for (name, text) in files {
            let conf: serde_json::Value = serde_json::from_str(text).unwrap();
            let windows = conf["app"]["windows"].as_array();
            let Some(windows) = windows else {
                continue;
            };
            let main = windows
                .iter()
                .find(|w| w["label"] == crate::window_state::MAIN)
                .unwrap_or_else(|| panic!("{name}: no main window"));
            assert_eq!(
                main["create"], false,
                "{name}: Tauri would create the window before setup"
            );
            assert_eq!(
                main["visible"], false,
                "{name}: the window would show before it is placed"
            );
        }
        // No other config file replaces the windows list unseen.
        for e in std::fs::read_dir(env!("CARGO_MANIFEST_DIR")).unwrap() {
            let name = e.unwrap().file_name().into_string().unwrap();
            if name.starts_with("tauri.")
                && name.ends_with(".json")
                && !files.iter().any(|f| f.0 == name)
            {
                let text = std::fs::read_to_string(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(&name),
                )
                .unwrap();
                let conf: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert!(
                    conf["app"]["windows"].is_null(),
                    "{name} sets windows: add it to this test"
                );
            }
        }
    }
}

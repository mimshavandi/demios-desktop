//! SPIKE for #75 (not for main): can a child webview of the main window hold artifacts on
//! Windows, isolated by Tauri's ACL? WebView2 injects Tauri's IPC scripts (and invoke key)
//! into every frame, so an iframe in the main webview can call every app command. A child
//! webview labelled `artifact` matches no capability (the app capability names the `main`
//! webview only), so every call from it should be refused.
//!
//! Dev fake-model builds only. `spike_open_child` adds the webview; its page runs the probes
//! and POSTs the results to `/spike/report` on the artifact scheme; `spike_child_report`
//! hands them to the e2e suite (which reads them from the main webview).

use std::sync::Mutex;
use tauri::http::{Request, Response, StatusCode, header};

static REPORT: Mutex<Option<String>> = Mutex::new(None);

pub const CHILD_LABEL: &str = "artifact";

fn origin() -> &'static str {
    if cfg!(windows) {
        "http://artifact.localhost"
    } else {
        "artifact://localhost"
    }
}

const PROBE_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>spike</title></head>
<body style="margin:0;font:14px system-ui;background:#fde68a;color:#111">
<p id="status" style="padding:8px">child webview: probing…</p>
<script>
(async () => {
  const out = {};
  const T = window.__TAURI_INTERNALS__;
  out.hasInternals = typeof T;
  out.hasIpc = typeof window.ipc;
  out.origin = location.origin;
  out.isTop = window.top === window;
  out.hasOpener = window.opener !== null && window.opener !== undefined;
  const call = async (cmd, args, opts) => {
    if (!T || typeof T.invoke !== "function") return "no-invoke";
    try { return "ok:" + JSON.stringify(await T.invoke(cmd, args, opts)).slice(0, 300); }
    catch (e) { return "err:" + String(e && e.message ? e.message : e).slice(0, 300); }
  };
  // (a) an app command
  out.listConversations = await call("list_conversations", {});
  out.createConversation = await call("create_conversation", {});
  // (b) channel data: the one command Tauri doesn't ACL-check; scoped to the owner's label
  out.channelHits = [];
  for (let id = 0; id < 64; id++) {
    const r = await call("plugin:__TAURI_CHANNEL__|fetch", null, { headers: { "Tauri-Channel-Id": String(id) } });
    if (r.startsWith("ok:")) out.channelHits.push(id + "=" + r.slice(0, 80));
  }
  // (c) events
  out.eventEmit = await call("plugin:event|emit", { event: "spike-from-child", payload: { x: 1 } });
  out.eventListen = await call("plugin:event|listen", { event: "tauri://focus", target: { kind: "Any" }, handler: 1 });
  // (e) the main webview's DOM and storage
  out.storageSentinel = (() => { try { return localStorage.getItem("dn-spike-sentinel"); } catch (e) { return "err:" + e.name; } })();
  out.sessionSentinel = (() => { try { return sessionStorage.getItem("dn-spike-sentinel"); } catch (e) { return "err:" + e.name; } })();
  out.cookie = document.cookie;
  out.parentDom = (() => { try { return window.parent.document.getElementById("dn-spike-sentinel") ? "visible" : "not-found"; } catch (e) { return "err:" + e.name; } })();
  out.topDom = (() => { try { return window.top.document.title; } catch (e) { return "err:" + e.name; } })();
  document.getElementById("status").textContent = "child webview: done";
  await fetch("/spike/report", { method: "POST", body: JSON.stringify(out) });
})();
</script></body></html>"#;

/// The spike's routes on the artifact scheme; `None` for every other path.
pub fn response(req: &Request<Vec<u8>>) -> Option<Response<Vec<u8>>> {
    match (req.method().as_str(), req.uri().path()) {
        ("GET", "/spike/") => Some(
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .header(
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' ipc: http://ipc.localhost",
                )
                .body(PROBE_PAGE.as_bytes().to_vec())
                .expect("static response"),
        ),
        ("POST", "/spike/report") => {
            let body = String::from_utf8_lossy(req.body()).into_owned();
            eprintln!("spike report: {body}");
            *REPORT.lock().unwrap_or_else(|p| p.into_inner()) = Some(body);
            Some(
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(vec![])
                    .expect("static response"),
            )
        }
        _ => None,
    }
}

/// Adds the child webview over the right part of the main window.
#[tauri::command]
pub async fn spike_open_child(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if !cfg!(fake_model) {
        return Err("spike only in dev fake-model builds".into());
    }
    *REPORT.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let window = app.get_window("main").ok_or("no main window")?;
    if let Some(old) = app.get_webview(CHILD_LABEL) {
        old.close().map_err(|e| e.to_string())?;
    }
    let url: tauri::Url = format!("{}/spike/", origin())
        .parse()
        .map_err(|e| format!("{e}"))?;
    let builder =
        tauri::webview::WebviewBuilder::new(CHILD_LABEL, tauri::WebviewUrl::CustomProtocol(url));
    window
        .add_child(
            builder,
            tauri::LogicalPosition::new(860.0, 120.0),
            tauri::LogicalSize::new(380.0, 260.0),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// What the child's probes found (JSON), once it has reported.
#[tauri::command]
pub async fn spike_child_report() -> Option<String> {
    REPORT.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// `DEMIOS_SPIKE_AUTORUN=1`: open the child a few seconds after start (manual runs on macOS,
/// where there is no WebDriver); the report is printed to stderr.
pub fn autorun(app: &tauri::AppHandle) {
    if !cfg!(fake_model) || std::env::var_os("DEMIOS_SPIKE_AUTORUN").is_none() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        if let Err(e) = spike_open_child(app.clone()).await {
            eprintln!("spike: could not open the child webview: {e}");
        }
        // WindowChild check: the main webview must follow the window when it is resized.
        use tauri::Manager;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let (Some(w), Some(main)) = (app.get_window("main"), app.get_webview("main")) {
            let before = (w.inner_size().ok(), main.size().ok());
            let _ = w.set_size(tauri::LogicalSize::new(1100.0, 760.0));
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let after = (w.inner_size().ok(), main.size().ok(), main.position().ok());
            eprintln!("spike resize: before (window, main webview) {before:?}; after (window, main webview, position) {after:?}");
        }
    });
}

/// The spike page on the artifact origin (the navigation guard lets it through in dev builds).
pub fn is_spike_url(url: &tauri::Url) -> bool {
    url.path() == "/spike/"
        && match url.scheme() {
            "artifact" => url.host_str() == Some("localhost"),
            "http" => url.host_str() == Some("artifact.localhost"),
            _ => false,
        }
}

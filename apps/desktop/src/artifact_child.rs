//! The artifact webview on Windows (#75, threat model R8, ADR-0012).
//!
//! WebView2 runs Tauri's IPC scripts (with the invoke key) in every frame, and they can't be
//! removed, so an artifact iframe inside the main webview could call every command the main
//! webview may call. On Windows artifacts therefore render in a **separate child webview** of
//! the main window, labelled [`LABEL`]. No capability names that webview (the app capability is
//! scoped to `webviews: ["main"]`), so Tauri's ACL refuses every call from it: from its host page
//! and from the artifact frame inside it. The e2e-windows isolation spec checks this on every
//! run, and every Tauri upgrade must pass it (multiwebview is Tauri's `unstable` feature).
//!
//! Inside the child, the host page (`ui/artifact-sandbox/src/host/host.ts`) holds the same
//! sandboxed iframe as on macOS and Linux: opaque origin, artifact CSP, one render per frame,
//! nonce-checked messages. Everything between the child and the app goes through here:
//! - main UI → [`artifact_child_render`] stores the document; Rust tells the host page to fetch
//!   it (`GET /host/doc`), which it may do once;
//! - host page → `POST /host/event` → validated → the main UI's channel (frame messages are
//!   validated again there, with the nonce);
//! - bounds, visibility and focus come from the main UI ([`artifact_child_bounds`],
//!   [`artifact_child_focus`]); focus leaving the child by Tab comes back as a `focus-out` event.
//!
//! Both `/host/` routes need the per-session token (a header the host page gets through webview
//! eval, never in a URL) and are served only to the child webview. macOS and Linux keep the
//! iframe in the main webview: their init scripts really are main-frame only.

use crate::error::{IpcError, IpcResult};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::ipc::Channel;

/// Label of the artifact webview. No capability may name it (`capability_tests` in lib.rs).
pub const LABEL: &str = "artifact";

/// Origin the child loads (Windows' form of the `artifact` scheme).
const ORIGIN: &str = "http://artifact.localhost";

const HOST_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/artifact-host.html"));
const HOST_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/artifact-host.js"));

/// CSP of the host page: its own script, the artifact frame, and the two `/host/` routes.
const HOST_CSP: &str = "default-src 'none'; script-src http://artifact.localhost/host/host.js; \
     frame-src http://artifact.localhost/frame/; connect-src http://artifact.localhost/host/; \
     style-src 'none'; img-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Largest document the main UI may hand over (the Mermaid runtime alone is ~4 MiB).
const MAX_DOC: usize = 64 * 1024 * 1024;
/// Largest event body from the host page (a frame message is capped far below this).
const MAX_EVENT: usize = 64 * 1024;
const TOKEN_HEADER: &str = "x-dn-token";

/// What the main UI hears from the child.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ChildEvent {
    /// A message from the artifact frame of render `seq`, as the host page validated it. The
    /// main UI checks it again (nonce, fields) before using it.
    Frame { seq: u64, data: serde_json::Value },
    /// The frame shell refused to run (it found host objects it shouldn't have).
    Refused { seq: u64 },
    /// Tab (or Shift+Tab) left the child: the main UI focuses what comes next (or before).
    FocusOut { dir: Dir },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    Prev,
    Next,
}

/// An event body from the host page. Anything else is dropped.
#[derive(Deserialize, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum HostEvent {
    Frame { seq: u64, data: serde_json::Value },
    Refused { seq: u64 },
    FocusOut { dir: Dir },
}

#[derive(Serialize, Clone)]
struct Doc {
    html: String,
    nonce: String,
    net: bool,
    title: String,
}

#[derive(Default)]
struct Inner {
    token: Option<String>,
    events: Option<Channel<ChildEvent>>,
    /// Documents not yet fetched, newest last (at most two: the one loading and the next).
    docs: VecDeque<(u64, Doc)>,
    /// The host page has loaded (it can take `render` calls).
    loaded: bool,
    /// A render asked for before the host page loaded.
    pending: Option<u64>,
    shown: Option<Bounds>,
    /// What happened to the child lately (page loads, host requests, renders), newest last;
    /// shown by the e2e-only `artifact_child_state`. No content, tokens or URLs with secrets.
    log: VecDeque<String>,
}

/// State of the artifact webview (managed by the app).
#[derive(Default)]
pub struct ChildState(Mutex<Inner>);

impl ChildState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn note(&self, what: impl Into<String>) {
        let mut s = self.lock();
        if s.log.len() >= 60 {
            s.log.pop_front();
        }
        s.log.push_back(what.into());
    }
}

/// Where the child goes, in the main webview's CSS pixels (= logical pixels: the main webview
/// fills the window's client area).
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() < 100_000.0)
            && self.width >= 1.0
            && self.height >= 1.0
    }
}

/// The main window's WebView2 browser arguments, from the one merged app config. The child
/// must use exactly these: WebView2 webviews that share a data folder must have identical
/// arguments, or the later one silently never loads (found in the #86 spike).
pub fn main_browser_args(config: &tauri::Config) -> Option<String> {
    config
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .and_then(|w| w.additional_browser_args.clone())
}

/// Navigations the child may make: its host page only.
pub fn allowed_navigation(url: &tauri::Url) -> bool {
    url.scheme() == "http"
        && url.host_str() == Some("artifact.localhost")
        && url.port().is_none()
        && matches!(url.path(), "/host/" | "/frame/")
}

/// Whether a `/host/` request may be answered: on Windows, from the artifact webview, on the
/// artifact origin (in either of the forms wry may pass: `artifact://localhost` or
/// `http://artifact.localhost`).
fn host_request_allowed(windows: bool, webview_label: &str, uri: &tauri::http::Uri) -> bool {
    windows && webview_label == LABEL && crate::artifacts::is_frame_origin(uri)
}

fn valid_token(t: &str) -> bool {
    (32..=128).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_background(bg: &str) -> bool {
    bg.len() == 7 && bg.starts_with('#') && bg[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn not_windows() -> IpcError {
    IpcError::invalid("the artifact webview is only used on Windows")
}

fn child<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<tauri::Webview<R>> {
    use tauri::Manager;
    app.get_webview(LABEL)
}

/// Opens the artifact webview (hidden until the first [`artifact_child_bounds`]), or takes over
/// the one that is open. `token` authenticates the host page's requests; `events` gets
/// [`ChildEvent`]s.
#[tauri::command]
pub async fn artifact_child_open(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChildState>,
    token: String,
    background: Option<String>,
    events: Channel<ChildEvent>,
) -> IpcResult<()> {
    if !cfg!(windows) {
        return Err(not_windows());
    }
    if !valid_token(&token) || !background.as_deref().is_none_or(valid_background) {
        return Err(IpcError::invalid("bad artifact webview session"));
    }
    {
        let mut s = state.lock();
        s.token = Some(token);
        s.events = Some(events);
        s.docs.clear();
        s.pending = None;
    }
    if child(&app).is_some() {
        return Ok(());
    }
    state.lock().loaded = false;
    use tauri::Manager;
    let window = app
        .get_window("main")
        .ok_or_else(|| IpcError::invalid("no main window"))?;
    let query = background
        .map(|bg| format!("?bg=%23{}", &bg[1..]))
        .unwrap_or_default();
    let url: tauri::Url = format!("{ORIGIN}/host/{query}")
        .parse()
        .map_err(|e| IpcError::invalid(format!("{e}")))?;
    let mut builder =
        tauri::webview::WebviewBuilder::new(LABEL, tauri::WebviewUrl::CustomProtocol(url))
            .focused(false)
            .on_navigation(|url| {
                let ok = allowed_navigation(url);
                if !ok {
                    eprintln!("artifact webview: blocked navigation to {url}");
                }
                ok
            })
            .on_page_load(|wv, payload| {
                use tauri::Manager;
                let app = wv.app_handle();
                let state = app.state::<ChildState>();
                state.note(format!(
                    "page-load {:?} {}",
                    payload.event(),
                    payload.url().path()
                ));
                if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                    return;
                }
                let mut s = state.lock();
                s.loaded = true;
                if let (Some(seq), Some(token)) = (s.pending.take(), s.token.clone()) {
                    drop(s);
                    let _ = wv.eval(render_call(seq, &token));
                }
            });
    if let Some(args) = main_browser_args(app.config()) {
        builder = builder.additional_browser_args(&args);
    }
    window
        .add_child(
            builder,
            tauri::LogicalPosition::new(0.0, 0.0),
            tauri::LogicalSize::new(1.0, 1.0),
        )
        .map_err(|e| IpcError::invalid(format!("could not open the artifact webview: {e}")))?;
    state.lock().shown = None;
    state.note("created");
    Ok(())
}

fn render_call(seq: u64, token: &str) -> String {
    // Both values are checked (a number, a hex token): nothing else goes into the script.
    format!("window.__dnArtifactHost&&window.__dnArtifactHost.render({seq},\"{token}\")")
}

/// Hands the child one document to render (built and nonce'd by the main UI's sandbox code).
#[tauri::command]
pub async fn artifact_child_render(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChildState>,
    seq: u64,
    html: String,
    nonce: String,
    net: bool,
    title: String,
) -> IpcResult<()> {
    if !cfg!(windows) {
        return Err(not_windows());
    }
    if html.len() > MAX_DOC || nonce.len() > 64 || title.len() > 1024 {
        return Err(IpcError::invalid("artifact document too large"));
    }
    let mut s = state.lock();
    let token = s
        .token
        .clone()
        .ok_or_else(|| IpcError::invalid("the artifact webview is not open"))?;
    s.docs.retain(|(n, _)| *n < seq);
    while s.docs.len() >= 2 {
        s.docs.pop_front();
    }
    s.docs.push_back((
        seq,
        Doc {
            html,
            nonce,
            net,
            title,
        },
    ));
    if !s.loaded {
        s.pending = Some(seq);
        drop(s);
        state.note(format!("render {seq} queued (host page not loaded)"));
        return Ok(());
    }
    drop(s);
    if let Some(wv) = child(&app) {
        state.note(format!("render {seq}"));
        wv.eval(render_call(seq, &token))
            .map_err(|e| IpcError::invalid(e.to_string()))?;
    }
    Ok(())
}

/// Moves and shows the child over the pane's frame area, or hides it (`bounds: None`: the pane
/// is closed, scrolled away, or an app dialog, menu, toast or tooltip is over it).
#[tauri::command]
pub async fn artifact_child_bounds(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChildState>,
    bounds: Option<Bounds>,
) -> IpcResult<()> {
    if !cfg!(windows) {
        return Err(not_windows());
    }
    let Some(wv) = child(&app) else {
        return Ok(());
    };
    let err = |e: tauri::Error| IpcError::invalid(e.to_string());
    match bounds.filter(Bounds::valid) {
        Some(b) => {
            wv.set_bounds(tauri::Rect {
                position: tauri::LogicalPosition::new(b.x, b.y).into(),
                size: tauri::LogicalSize::new(b.width, b.height).into(),
            })
            .map_err(err)?;
            wv.show().map_err(err)?;
            state.lock().shown = Some(b);
            state.note("shown");
        }
        None => {
            wv.hide().map_err(err)?;
            state.lock().shown = None;
            state.note("hidden");
        }
    }
    Ok(())
}

/// Moves keyboard focus into the artifact (the pane's frame placeholder got focus).
#[tauri::command]
pub async fn artifact_child_focus(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChildState>,
) -> IpcResult<()> {
    if !cfg!(windows) {
        return Err(not_windows());
    }
    let Some(token) = state.lock().token.clone() else {
        return Ok(());
    };
    if let Some(wv) = child(&app) {
        let _ = wv.set_focus();
        let _ = wv.eval(format!(
            "window.__dnArtifactHost&&window.__dnArtifactHost.focus(\"{token}\")"
        ));
    }
    Ok(())
}

/// Closes the artifact webview (the pane closed or the app is leaving artifacts).
#[tauri::command]
pub async fn artifact_child_close(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChildState>,
) -> IpcResult<()> {
    *state.lock() = Inner::default();
    if let Some(wv) = child(&app) {
        wv.close().map_err(|e| IpcError::invalid(e.to_string()))?;
    }
    Ok(())
}

fn empty(status: StatusCode) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'")
        .header("X-Content-Type-Options", "nosniff")
        .body(Vec::new())
        .expect("static response")
}

fn token_ok(req: &Request<Vec<u8>>, state: &ChildState) -> bool {
    let sent = req
        .headers()
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok());
    match (sent, state.lock().token.as_deref()) {
        (Some(a), Some(b)) => a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x == y),
        _ => false,
    }
}

/// What the host page reported, if it is well formed.
fn parse_event(body: &[u8]) -> Option<ChildEvent> {
    if body.len() > MAX_EVENT {
        return None;
    }
    match serde_json::from_slice::<HostEvent>(body).ok()? {
        HostEvent::Frame { seq, data } if data.is_object() => Some(ChildEvent::Frame { seq, data }),
        HostEvent::Frame { .. } => None,
        HostEvent::Refused { seq } => Some(ChildEvent::Refused { seq }),
        HostEvent::FocusOut { dir } => Some(ChildEvent::FocusOut { dir }),
    }
}

/// Requests on the artifact origin for the host page and its routes. `None`: not a `/host/`
/// request (the caller serves the frame shell or a 404). Only the child webview, on Windows,
/// gets anything other than a 404 here.
pub fn host_response<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    webview_label: &str,
    req: &Request<Vec<u8>>,
) -> Option<Response<Vec<u8>>> {
    use tauri::Manager;
    let path = req.uri().path();
    if !path.starts_with("/host/") {
        return None;
    }
    let state = app.state::<ChildState>();
    if !host_request_allowed(cfg!(windows), webview_label, req.uri()) {
        if webview_label == LABEL {
            state.note(format!("refused {} {path}", req.method()));
        }
        return Some(empty(StatusCode::NOT_FOUND));
    }
    let get = req.method() == tauri::http::Method::GET;
    let post = req.method() == tauri::http::Method::POST;
    let response = match path {
        "/host/" | "/host/index.html" if get => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CONTENT_SECURITY_POLICY, HOST_CSP)
            .header("X-Content-Type-Options", "nosniff")
            .header(header::REFERRER_POLICY, "no-referrer")
            .header(header::CACHE_CONTROL, "no-store")
            .body(HOST_HTML.as_bytes().to_vec())
            .expect("static response"),
        "/host/host.js" if get => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
            .header("X-Content-Type-Options", "nosniff")
            .header(header::CACHE_CONTROL, "no-store")
            .body(HOST_JS.as_bytes().to_vec())
            .expect("static response"),
        "/host/doc" if get && token_ok(req, &state) => {
            let seq = req
                .uri()
                .query()
                .and_then(|q| q.strip_prefix("seq="))
                .and_then(|v| v.parse::<u64>().ok());
            let doc = seq.and_then(|seq| {
                let mut s = state.lock();
                let i = s.docs.iter().position(|(n, _)| *n == seq)?;
                s.docs.remove(i).map(|(_, d)| d)
            });
            match doc.and_then(|d| serde_json::to_vec(&d).ok()) {
                Some(body) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-Content-Type-Options", "nosniff")
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(body)
                    .expect("static response"),
                None => empty(StatusCode::NOT_FOUND),
            }
        }
        "/host/event" if post && token_ok(req, &state) => match parse_event(req.body()) {
            Some(ev) => {
                if matches!(ev, ChildEvent::FocusOut { .. })
                    && let Some(main) = app.get_webview("main")
                {
                    let _ = main.set_focus();
                }
                if let Some(ch) = state.lock().events.clone() {
                    let _ = ch.send(ev);
                }
                empty(StatusCode::NO_CONTENT)
            }
            None => empty(StatusCode::BAD_REQUEST),
        },
        #[cfg(fake_model)]
        "/host/probe-report" if post => probe::record(req),
        _ => empty(StatusCode::NOT_FOUND),
    };
    state.note(format!(
        "{} {path} {}",
        req.method(),
        response.status().as_u16()
    ));
    Some(response)
}

/// e2e only (dev fake-model builds): runs the isolation probes inside the real artifact
/// webview's host page and hands the results to the e2e suite. The probes are the #86 spike's
/// (a)–(e): see apps/desktop/e2e/specs/artifact-child.e2e.mjs.
#[tauri::command]
pub async fn artifact_child_probe(app: tauri::AppHandle) -> IpcResult<()> {
    #[cfg(fake_model)]
    {
        if !cfg!(windows) {
            return Err(not_windows());
        }
        let wv =
            child(&app).ok_or_else(|| IpcError::invalid("the artifact webview is not open"))?;
        probe::reset();
        wv.eval(probe::SCRIPT)
            .map_err(|e| IpcError::invalid(e.to_string()))
    }
    #[cfg(not(fake_model))]
    {
        let _ = app;
        Err(IpcError::invalid("only in dev fake-model builds"))
    }
}

/// e2e only: the probe results (JSON text), once the probe has reported.
#[tauri::command]
pub async fn artifact_child_probe_report() -> IpcResult<Option<String>> {
    #[cfg(fake_model)]
    {
        Ok(probe::report())
    }
    #[cfg(not(fake_model))]
    {
        Err(IpcError::invalid("only in dev fake-model builds"))
    }
}

/// e2e only: whether the child exists, where it is shown (`None`: hidden) and its URL.
#[tauri::command]
pub async fn artifact_child_state(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChildState>,
) -> IpcResult<serde_json::Value> {
    if !cfg!(fake_model) {
        return Err(IpcError::invalid("only in dev fake-model builds"));
    }
    let wv = child(&app);
    Ok(serde_json::json!({
        "exists": wv.is_some(),
        "url": wv.and_then(|w| w.url().ok()).map(|u| u.to_string()),
        "shown": state.lock().shown,
        "loaded": state.lock().loaded,
        "log": state.lock().log.iter().cloned().collect::<Vec<_>>(),
    }))
}

#[cfg(fake_model)]
mod probe {
    use super::{StatusCode, empty};
    use std::sync::Mutex;
    use tauri::http::{Request, Response};

    static REPORT: Mutex<Option<String>> = Mutex::new(None);

    pub fn reset() {
        *REPORT.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub fn report() -> Option<String> {
        REPORT.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn record(req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
        if req.method() == tauri::http::Method::POST && req.body().len() < 256 * 1024 {
            let mut r = REPORT.lock().unwrap_or_else(|p| p.into_inner());
            if r.is_none() {
                *r = Some(String::from_utf8_lossy(req.body()).into_owned());
            }
        }
        empty(StatusCode::NO_CONTENT)
    }

    /// Runs in the host page of the artifact webview, which has Tauri's IPC object (the point).
    pub const SCRIPT: &str = r##"(async () => {
  const out = {};
  const T = window.__TAURI_INTERNALS__;
  out.hasInternals = typeof T;
  out.label = T && T.metadata && T.metadata.currentWebview ? T.metadata.currentWebview.label : null;
  out.origin = location.origin;
  out.isTop = window.top === window;
  out.hasOpener = window.opener !== null && window.opener !== undefined;
  const call = async (cmd, args, opts) => {
    if (!T || typeof T.invoke !== "function") return "no-invoke";
    const timeout = new Promise((r) => setTimeout(() => r("timeout"), 5000));
    try {
      return await Promise.race([T.invoke(cmd, args, opts).then((v) => "ok:" + JSON.stringify(v).slice(0, 300)), timeout]);
    } catch (e) { return "err:" + String(e && e.message ? e.message : e).slice(0, 300); }
  };
  out.listConversations = await call("list_conversations", {});
  out.createConversation = await call("create_conversation", {});
  out.childOpen = await call("artifact_child_open", { token: "0".repeat(32), background: null, events: null });
  out.channelHits = [];
  for (let id = 0; id < 64; id++) {
    const r = await call("plugin:__TAURI_CHANNEL__|fetch", null, { headers: { "Tauri-Channel-Id": String(id) } });
    if (r.startsWith("ok:")) out.channelHits.push(id + "=" + r.slice(0, 80));
  }
  out.eventEmit = await call("plugin:event|emit", { event: "probe-from-child", payload: { x: 1 } });
  out.eventListen = await call("plugin:event|listen", { event: "tauri://focus", target: { kind: "Any" }, handler: 1 });
  out.storageSentinel = (() => { try { return localStorage.getItem("dn-child-sentinel"); } catch (e) { return "err:" + e.name; } })();
  out.sessionSentinel = (() => { try { return sessionStorage.getItem("dn-child-sentinel"); } catch (e) { return "err:" + e.name; } })();
  out.cookie = document.cookie;
  out.parentDom = (() => { try { return window.parent.document.getElementById("dn-child-sentinel") ? "visible" : "not-found"; } catch (e) { return "err:" + e.name; } })();
  out.hostDoc = await fetch("/host/doc?seq=0").then((r) => r.status).catch((e) => "err:" + e);
  await fetch("/host/probe-report", { method: "POST", body: JSON.stringify(out) }).catch(() => {});
})();"##;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The merged Windows config (tauri.conf.json + tauri.windows.conf.json, as Tauri merges it).
    fn windows_config() -> tauri::Config {
        let mut base: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let win: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.windows.conf.json")).unwrap();
        merge(&mut base, &win);
        serde_json::from_value(base).unwrap()
    }

    /// JSON merge patch (RFC 7396), which Tauri uses for platform config files.
    fn merge(target: &mut serde_json::Value, patch: &serde_json::Value) {
        match (target, patch) {
            (serde_json::Value::Object(t), serde_json::Value::Object(p)) => {
                for (k, v) in p {
                    if v.is_null() {
                        t.remove(k);
                    } else {
                        merge(t.entry(k.clone()).or_insert(serde_json::Value::Null), v);
                    }
                }
            }
            (t, p) => *t = p.clone(),
        }
    }

    #[test]
    fn child_uses_exactly_the_main_windows_browser_args() {
        let config = windows_config();
        let main = config
            .app
            .windows
            .iter()
            .find(|w| w.label == "main")
            .expect("main window");
        let args = main_browser_args(&config).expect("the Windows config sets browser args");
        assert_eq!(Some(&args), main.additional_browser_args.as_ref());
        // The WebRTC policy (T-ART-06) is part of them, so the child gets it too.
        assert!(args.contains("--force-webrtc-ip-handling-policy=disable_non_proxied_udp"));
        // The only window is main: no second source of browser args to drift from.
        assert_eq!(config.app.windows.len(), 1);
    }

    #[test]
    fn child_navigates_only_to_its_host_page() {
        let ok = |u: &str| allowed_navigation(&u.parse().unwrap());
        assert!(ok("http://artifact.localhost/host/"));
        assert!(ok("http://artifact.localhost/host/?bg=%23ffffff"));
        assert!(!ok("http://artifact.localhost/host/doc"));
        assert!(!ok("http://tauri.localhost/"));
        assert!(!ok("https://artifact.localhost/host/"));
        assert!(!ok("http://artifact.localhost:8080/host/"));
        assert!(!ok("https://example.com/host/"));
        assert!(!ok("artifact://localhost/host/"));
    }

    #[test]
    fn host_routes_answer_only_the_artifact_webview_on_its_origin() {
        let uri = |u: &str| u.parse::<tauri::http::Uri>().unwrap();
        // wry on WebView2 passes the reverted `artifact://localhost` form (the e2e-windows bug).
        assert!(host_request_allowed(
            true,
            LABEL,
            &uri("artifact://localhost/host/")
        ));
        assert!(host_request_allowed(
            true,
            LABEL,
            &uri("http://artifact.localhost/host/doc?seq=1")
        ));
        assert!(!host_request_allowed(
            true,
            "main",
            &uri("artifact://localhost/host/")
        ));
        assert!(!host_request_allowed(
            false,
            LABEL,
            &uri("artifact://localhost/host/")
        ));
        assert!(!host_request_allowed(
            true,
            LABEL,
            &uri("artifact://evil/host/")
        ));
        assert!(!host_request_allowed(
            true,
            LABEL,
            &uri("http://tauri.localhost/host/")
        ));
    }

    #[test]
    fn host_events_are_parsed_strictly() {
        assert_eq!(
            parse_event(br#"{"kind":"frame","seq":3,"data":{"type":"ready"}}"#),
            Some(ChildEvent::Frame {
                seq: 3,
                data: serde_json::json!({"type": "ready"})
            })
        );
        assert_eq!(
            parse_event(br#"{"kind":"focus-out","dir":"next"}"#),
            Some(ChildEvent::FocusOut { dir: Dir::Next })
        );
        assert_eq!(
            parse_event(br#"{"kind":"refused","seq":1}"#),
            Some(ChildEvent::Refused { seq: 1 })
        );
        for bad in [
            &br#"{"kind":"frame","seq":3,"data":"x"}"#[..],
            br#"{"kind":"frame","seq":-1,"data":{}}"#,
            br#"{"kind":"focus-out","dir":"up"}"#,
            br#"{"kind":"invoke","cmd":"delete_all_local_data"}"#,
            br#"{"kind":"refused","seq":1,"extra":true}"#,
            b"not json",
        ] {
            assert_eq!(parse_event(bad), None, "{}", String::from_utf8_lossy(bad));
        }
        let mut big = br#"{"kind":"frame","seq":1,"data":{"x":""#.to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_EVENT));
        big.extend(br#""}}"#);
        assert_eq!(parse_event(&big), None);
    }

    #[test]
    fn session_values_are_checked() {
        assert!(valid_token(&"a1".repeat(16)));
        assert!(!valid_token("short"));
        assert!(!valid_token(&format!("{}\");alert(1);//", "a".repeat(32))));
        assert!(valid_background("#1a2B3c"));
        assert!(!valid_background("red"));
        assert!(!valid_background("#12345g"));
        assert!(render_call(7, &"ab".repeat(16)).contains("render(7,\"abab"));
    }

    #[test]
    fn bounds_must_be_finite_and_non_empty() {
        let b = |x, y, w, h| Bounds {
            x,
            y,
            width: w,
            height: h,
        };
        assert!(b(10.0, 20.0, 300.0, 200.0).valid());
        assert!(!b(f64::NAN, 0.0, 300.0, 200.0).valid());
        assert!(!b(0.0, 0.0, 0.0, 200.0).valid());
        assert!(!b(0.0, 0.0, 1e9, 200.0).valid());
    }
}

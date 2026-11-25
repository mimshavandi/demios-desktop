//! Artifacts in the app (T-ART-06, ADR-0004).
//!
//! - **Frame origin.** The `artifact` URI scheme serves exactly one document: the sandbox
//!   frame shell built by `ui/artifact-sandbox` (`dist/frame/index.html`), with the artifact
//!   CSP as a response header (`network` policy only for `?net=1`). Nothing else is served
//!   there. The origin is `artifact://localhost` on macOS/Linux and `http://artifact.localhost`
//!   on Windows; the main window's CSP allows it in `frame-src` and nothing else.
//! - **Model protocol.** A model that can make tool calls gets the `artifact_*` tools
//!   ([`RunTools`]); a model that can't gets [`dn_artifacts::FENCED_INSTRUCTIONS`] and its
//!   text goes through the fenced parser ([`FencedRun`]). Never both: [`ArtifactMode`] is
//!   chosen once per run from the model registry, so `<artifact>` text from a tool-capable
//!   model (or echoed from a tool result) never creates anything.
//! - **Store.** `dn-artifacts` tables live in the app database (`migration_sets`).

use crate::chat::{ChatEvent, EventSink};
use crate::error::{IpcError, IpcResult};
use crate::state::AppState;
use dn_agent::{BoxFuture, ToolContext, ToolExecutor, ToolOutput};
use dn_artifacts::{
    Applied, ArtifactError, ArtifactStore, ArtifactSummary, FenceEvent, FencedParser, OpContext,
};
use dn_core::artifact::{Artifact, ArtifactType, ArtifactVersion};
use dn_core::chat::{ToolCall, ToolSpec};
use dn_core::{CoreError, Id};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tauri::http::{Request, Response, StatusCode, header};

/// URI scheme of the artifact frame origin.
pub const SCHEME: &str = "artifact";

/// Frame shell and its CSPs, copied from `ui/artifact-sandbox/dist/frame/` by build.rs (a
/// placeholder page when the sandbox isn't built; builds with `custom-protocol` require it).
const FRAME_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/artifact-frame.html"));
const FRAME_CSP_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/artifact-frame-csp.json"));

/// Whether the real frame shell is in this build (false: the placeholder page).
pub fn frame_built() -> bool {
    !FRAME_HTML.starts_with("<!-- placeholder")
}

#[derive(Deserialize)]
struct FrameCsp {
    strict: String,
    network: String,
}

fn frame_csp() -> &'static FrameCsp {
    static CSP: OnceLock<FrameCsp> = OnceLock::new();
    CSP.get_or_init(|| {
        serde_json::from_str(FRAME_CSP_JSON).unwrap_or_else(|e| {
            eprintln!("artifact frame CSP is unreadable ({e}); serving a deny-all policy");
            FrameCsp {
                strict: "default-src 'none'".into(),
                network: "default-src 'none'".into(),
            }
        })
    })
}

/// The only path served on the artifact origin.
pub fn is_frame_path(path: &str) -> bool {
    matches!(path, "/frame/" | "/frame/index.html")
}

/// Whether the request is for the artifact origin itself: `artifact://localhost` (macOS, Linux)
/// or `http://artifact.localhost` (Windows). On Windows wry hands the scheme handler the
/// `artifact://localhost/…` form (it undoes its `http://artifact.localhost` workaround first), so
/// both forms can reach a handler.
pub fn is_frame_origin(uri: &tauri::http::Uri) -> bool {
    matches!(
        (uri.scheme_str(), uri.host()),
        (Some(SCHEME), Some("localhost")) | (Some("http"), Some("artifact.localhost"))
    )
}

/// Who may embed the frame: the app's own window only (on Windows: the artifact webview's host
/// page, which is on the artifact origin itself).
fn frame_ancestors(in_child: bool) -> &'static str {
    if in_child {
        "frame-ancestors http://artifact.localhost"
    } else if cfg!(debug_assertions) {
        // `tauri dev` serves the UI from the Vite dev server.
        "frame-ancestors tauri://localhost http://tauri.localhost https://tauri.localhost http://localhost:1420 http://127.0.0.1:1420"
    } else {
        "frame-ancestors tauri://localhost http://tauri.localhost https://tauri.localhost"
    }
}

/// Response for a request on the `artifact` scheme: the frame shell at `/frame/` (GET only),
/// 404 for anything else. `in_child`: the request comes from the artifact webview
/// (`artifact_child::LABEL`, Windows only).
pub fn frame_response(req: &Request<Vec<u8>>, in_child: bool) -> Response<Vec<u8>> {
    let path = req.uri().path();
    if req.method() != tauri::http::Method::GET
        || !is_frame_origin(req.uri())
        || !is_frame_path(path)
    {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain")
            .header("X-Content-Type-Options", "nosniff")
            .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'")
            .body(b"not found".to_vec())
            .expect("static response");
    }
    // Windows: WebView2 injects the app's init scripts (Tauri's IPC object) into sub-frames and
    // they can't be removed, so artifacts render only in the artifact webview, which no
    // capability names (artifact_child.rs, threat model R8). Anything else that loads the frame
    // (an iframe in the main window) gets an inert page with no script at all.
    if cfg!(windows) && !in_child {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox",
            )
            .header("X-Content-Type-Options", "nosniff")
            .body(b"<!doctype html><p>Artifacts open in the Demios artifact pane.</p>".to_vec())
            .expect("static response");
    }
    let network = req
        .uri()
        .query()
        .is_some_and(|q| q.split('&').any(|kv| kv == "net=1"));
    let csp = frame_csp();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(
            header::CONTENT_SECURITY_POLICY,
            // `sandbox allow-scripts`: the document is sandboxed even if the iframe attribute
            // were ever dropped; `frame-ancestors`: only the app's window may embed it.
            format!(
                "{}; sandbox allow-scripts; {}",
                if network { &csp.network } else { &csp.strict },
                frame_ancestors(in_child)
            ),
        )
        .header("X-Content-Type-Options", "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CACHE_CONTROL, "no-store")
        .body(FRAME_HTML.as_bytes().to_vec())
        .expect("static response")
}

// ---- views for the UI

/// One version plus the model that wrote it (from its message), for "Made by <model>".
#[derive(Clone, Debug, Serialize)]
pub struct VersionView {
    #[serde(flatten)]
    pub version: ArtifactVersion,
    /// Model ref of the message that made this version; `None` for the user's own versions.
    pub model: Option<String>,
}

/// [`Artifact`] as the pane gets it.
#[derive(Clone, Debug, Serialize)]
pub struct ArtifactView {
    pub id: Id,
    pub conversation_id: Id,
    pub artifact_type: ArtifactType,
    pub title: String,
    pub versions: Vec<VersionView>,
}

fn store_error(e: ArtifactError) -> IpcError {
    match e {
        ArtifactError::NotFound { .. } | ArtifactError::VersionNotFound { .. } => {
            IpcError::new("not_found", e.to_string())
        }
        ArtifactError::Db(_) | ArtifactError::Corrupt(_) => {
            eprintln!("artifact store error: {e}");
            IpcError::new("storage", "Could not read or write local data.")
        }
        // The store's own text is written for the model; the user gets plain words.
        ArtifactError::TooMany { what, max } => IpcError::invalid(if what == "artifacts" {
            format!("This chat already has {max} artifacts, the most it can hold.")
        } else {
            format!(
                "This artifact already has {max} versions, the most it can have, so this version can't be restored. Ask for a new artifact instead."
            )
        }),
        other => IpcError::invalid(other.to_string()),
    }
}

/// Artifacts of a conversation, oldest first. Blocking.
pub fn list(state: &AppState, conversation_id: Id) -> IpcResult<Vec<ArtifactSummary>> {
    let db = state.db()?;
    ArtifactStore::new(&db)
        .list(conversation_id)
        .map_err(store_error)
}

/// One artifact with every version, if it belongs to `conversation_id`. Blocking.
pub fn get(state: &AppState, conversation_id: Id, artifact_id: Id) -> IpcResult<ArtifactView> {
    let db = state.db()?;
    let store = ArtifactStore::new(&db);
    if !store
        .exists_in(artifact_id, conversation_id)
        .map_err(store_error)?
    {
        return Err(IpcError::new(
            "not_found",
            "That artifact isn't in this chat.",
        ));
    }
    let a = store.get(artifact_id).map_err(store_error)?;
    let models: std::collections::HashMap<Id, Option<String>> =
        dn_store::chat::list_messages(&db, conversation_id)?
            .into_iter()
            .map(|m| (m.id, m.model))
            .collect();
    Ok(view(a, &models))
}

fn view(a: Artifact, models: &std::collections::HashMap<Id, Option<String>>) -> ArtifactView {
    ArtifactView {
        id: a.id,
        conversation_id: a.conversation_id,
        artifact_type: a.artifact_type,
        title: a.title,
        versions: a
            .versions
            .into_iter()
            .map(|v| VersionView {
                model: match v.author {
                    dn_core::artifact::Author::User => None,
                    dn_core::artifact::Author::Model => {
                        v.message_id.and_then(|m| models.get(&m).cloned().flatten())
                    }
                },
                version: v,
            })
            .collect(),
    }
}

/// "Restore this version": a new version by the user with `to_n`'s content. Blocking.
pub fn rollback(
    state: &AppState,
    conversation_id: Id,
    artifact_id: Id,
    to_n: u32,
) -> IpcResult<ArtifactView> {
    {
        let db = state.db()?;
        ArtifactStore::new(&db)
            .rollback(conversation_id, artifact_id, to_n, &OpContext::user())
            .map_err(store_error)?;
    }
    get(state, conversation_id, artifact_id)
}

// ---- the model side

/// How the model in this run gets artifacts. Exactly one, chosen from the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactMode {
    /// `artifact_create` / `artifact_update` / `artifact_rewrite` tool calls; text is never
    /// parsed for `<artifact>` blocks.
    Tools,
    /// The fenced `<artifact>` format in the model's text; no artifact tools are offered.
    Fenced,
}

impl ArtifactMode {
    pub fn for_model(state: &AppState, model: &dn_core::model::ModelRef) -> Self {
        if state.models.supports_tools(model) {
            Self::Tools
        } else {
            Self::Fenced
        }
    }
}

/// Capability id in the agent manifest for an artifact tool name (`artifact_create` →
/// `artifact.create`).
fn capability_id(tool: &str) -> Option<String> {
    tool.strip_prefix("artifact_")
        .map(|op| format!("artifact.{op}"))
}

/// Event for the UI after an artifact changed.
fn changed(message_id: Id, conversation_id: Id, a: &Applied) -> ChatEvent {
    ChatEvent::ArtifactChanged {
        message_id,
        conversation_id,
        artifact_id: a.artifact_id,
        title: a.title.clone(),
        version: a.version.n,
    }
}

/// The run's tools: the manifest-filtered dn-tools executor plus, in [`ArtifactMode::Tools`],
/// the artifact tools the manifest allows (`artifact.create` / `.update` / `.rewrite`).
pub struct RunTools {
    pub base: Arc<dyn ToolExecutor>,
    /// `None` in [`ArtifactMode::Fenced`]: no artifact tools at all.
    pub artifacts: Option<ArtifactTools>,
}

pub struct ArtifactTools {
    pub state: Arc<AppState>,
    pub conversation_id: Id,
    /// The turn's assistant message; versions point at it (provenance, "Made by").
    pub message_id: Id,
    pub sink: Arc<dyn EventSink>,
}

impl ArtifactTools {
    fn specs(&self) -> Vec<ToolSpec> {
        dn_artifacts::tool_specs()
            .into_iter()
            .filter(|s| {
                capability_id(&s.name).is_some_and(|c| self.state.agent.manifest.allows_tool(&c))
            })
            .collect()
    }
}

impl ToolExecutor for RunTools {
    fn tools(&self) -> Vec<ToolSpec> {
        let mut v = self.base.tools();
        if let Some(a) = &self.artifacts {
            v.retain(|s| !dn_artifacts::protocol::is_artifact_tool(&s.name));
            v.extend(a.specs());
        }
        v
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, CoreError>> {
        Box::pin(async move {
            if !dn_artifacts::protocol::is_artifact_tool(&call.name) {
                return self.base.execute(call, ctx).await;
            }
            let Some(a) = self
                .artifacts
                .as_ref()
                .filter(|a| a.specs().iter().any(|s| s.name == call.name))
            else {
                return Ok(ToolOutput::error(format!(
                    "The tool `{}` isn't available to this assistant.",
                    call.name
                )));
            };
            let state = Arc::clone(&a.state);
            let (conversation_id, message_id) = (a.conversation_id, a.message_id);
            let call = call.clone();
            let applied = tauri::async_runtime::spawn_blocking(move || {
                let db = state.db().map_err(|e| e.message)?;
                Ok::<_, String>(dn_artifacts::execute_tool_call(
                    &ArtifactStore::new(&db),
                    conversation_id,
                    &call,
                    &OpContext::model(Some(message_id)),
                ))
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            let result = match applied {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("artifact tool failed: {e}");
                    return Ok(ToolOutput::error(
                        "The artifact couldn't be saved. Try again.",
                    ));
                }
            };
            if let Ok(ap) = &result {
                a.sink.send(changed(message_id, conversation_id, ap));
            }
            let text = dn_artifacts::tool_result(&result).to_string();
            Ok(if result.is_ok() {
                ToolOutput::text(text)
            } else {
                ToolOutput::error(text)
            })
        })
    }
}

/// [`ArtifactMode::Fenced`]: parses the streamed text and saves each closed `<artifact>`
/// block in order, on one blocking worker (SQLite stays off the async threads).
pub struct FencedRun {
    parser: FencedParser,
    tx: std::sync::mpsc::Sender<dn_artifacts::FencedArtifact>,
    worker: tauri::async_runtime::JoinHandle<()>,
    sink: Arc<dyn EventSink>,
    conversation_id: Id,
    message_id: Id,
}

/// The user-facing event for a fenced block that wasn't saved.
fn not_saved(message_id: Id, conversation_id: Id, title: &str, reason: String) -> ChatEvent {
    ChatEvent::ArtifactNotSaved {
        message_id,
        conversation_id,
        title: title.to_owned(),
        reason,
    }
}

impl FencedRun {
    pub fn start(
        state: Arc<AppState>,
        conversation_id: Id,
        message_id: Id,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<dn_artifacts::FencedArtifact>();
        let worker_sink = Arc::clone(&sink);
        let worker = tauri::async_runtime::spawn_blocking(move || {
            let sink = worker_sink;
            for block in rx {
                let result = match state.db() {
                    Ok(db) => dn_artifacts::apply_fenced(
                        &ArtifactStore::new(&db),
                        conversation_id,
                        &block,
                        &OpContext::model(Some(message_id)),
                    ),
                    Err(e) => {
                        eprintln!("artifact block not saved: {}", e.message);
                        sink.send(not_saved(
                            message_id,
                            conversation_id,
                            &block.header.title,
                            "Local data couldn't be written.".into(),
                        ));
                        continue;
                    }
                };
                match result {
                    Ok(a) => sink.send(changed(message_id, conversation_id, &a)),
                    // The model can't get a tool result back: tell the user instead.
                    Err(e) => sink.send(not_saved(
                        message_id,
                        conversation_id,
                        &block.header.title,
                        e.to_string(),
                    )),
                }
            }
        });
        Self {
            parser: FencedParser::new(),
            tx,
            worker,
            sink,
            conversation_id,
            message_id,
        }
    }

    pub fn push(&mut self, text: &str) {
        let events = self.parser.push(text);
        self.handle(events);
    }

    fn handle(&mut self, events: Vec<FenceEvent>) {
        for ev in events {
            match ev {
                FenceEvent::Close(block) => {
                    let _ = self.tx.send(block);
                }
                FenceEvent::Rejected { header, reason } => self.sink.send(not_saved(
                    self.message_id,
                    self.conversation_id,
                    &header.title,
                    reason,
                )),
                FenceEvent::Unterminated(block) => self.sink.send(not_saved(
                    self.message_id,
                    self.conversation_id,
                    &block.header.title,
                    ArtifactError::Unterminated.to_string(),
                )),
                _ => {}
            }
        }
    }

    /// Ends the stream (unterminated blocks are dropped, never saved) and waits until every
    /// closed block is stored.
    pub async fn finish(mut self) {
        let parser = std::mem::take(&mut self.parser);
        let events = parser.finish();
        self.handle(events);
        drop(self.tx);
        let _ = self.worker.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(uri: &str) -> Request<Vec<u8>> {
        Request::builder().uri(uri).body(vec![]).unwrap()
    }

    #[cfg(not(windows))]
    #[test]
    fn serves_only_the_frame_shell() {
        let ok = frame_response(&req("artifact://localhost/frame/"), false);
        assert_eq!(ok.status(), StatusCode::OK);
        let h = ok.headers();
        assert_eq!(h["x-content-type-options"], "nosniff");
        let csp = h["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("default-src 'none'"), "{csp}");
        let policy = csp.split("; sandbox").next().unwrap();
        assert!(
            !policy.contains("https:"),
            "strict policy has no network: {csp}"
        );
        assert_eq!(ok.body(), FRAME_HTML.as_bytes());
        assert!(csp.contains("sandbox allow-scripts"), "{csp}");
        assert!(csp.contains("frame-ancestors tauri://localhost"), "{csp}");

        // Windows form of the same origin.
        let win = frame_response(&req("http://artifact.localhost/frame/index.html"), false);
        assert_eq!(win.status(), StatusCode::OK);

        for bad in [
            "artifact://localhost/",
            "artifact://localhost/frame/../secret",
            "artifact://localhost/frame/x.js",
            "artifact://localhost/frame",
            "artifact://localhost/index.html",
            "artifact://evil/frame/",
            "http://evil.localhost/frame/",
            "https://artifact.localhost/frame/",
        ] {
            let r = frame_response(&req(bad), false);
            assert_eq!(r.status(), StatusCode::NOT_FOUND, "{bad}");
        }
        let post = Request::builder()
            .method("POST")
            .uri("artifact://localhost/frame/")
            .body(vec![])
            .unwrap();
        assert_eq!(frame_response(&post, false).status(), StatusCode::NOT_FOUND);
    }

    #[cfg(windows)]
    #[test]
    fn windows_serves_the_shell_only_to_the_artifact_webview() {
        // The main window (or anything but the artifact webview): an inert page, no script.
        let r = frame_response(&req("http://artifact.localhost/frame/"), false);
        assert_eq!(r.status(), StatusCode::OK);
        let body = String::from_utf8(r.body().clone()).unwrap();
        assert!(!body.contains("<script"), "{body}");
        // The artifact webview: the real shell, embeddable only by its host page.
        let r = frame_response(&req("http://artifact.localhost/frame/"), true);
        assert_eq!(r.body(), FRAME_HTML.as_bytes());
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("sandbox allow-scripts"), "{csp}");
        assert!(
            csp.ends_with("frame-ancestors http://artifact.localhost"),
            "{csp}"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn network_policy_only_with_net_1() {
        let csp = |uri: &str| {
            // The content policy, without the `sandbox` / `frame-ancestors` tail.
            frame_response(&req(uri), false).headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .split("; sandbox")
                .next()
                .unwrap()
                .to_owned()
        };
        let strict = csp("artifact://localhost/frame/");
        assert_eq!(csp("artifact://localhost/frame/?net=0"), strict);
        assert_eq!(csp("artifact://localhost/frame/?network=1"), strict);
        if frame_built() {
            let net = csp("artifact://localhost/frame/?net=1");
            assert_ne!(net, strict);
            assert!(net.contains("https:"), "{net}");
            assert!(!net.contains("http:"), "plain http stays blocked: {net}");
        }
    }

    #[test]
    fn limit_errors_are_worded_for_people() {
        let e = store_error(ArtifactError::TooMany {
            what: "versions of one artifact",
            max: 200,
        });
        assert!(e.message.contains("can't be restored"), "{}", e.message);
        assert!(!e.message.contains("artifact_"), "{}", e.message);
    }

    #[test]
    fn capability_ids_match_the_manifest_names() {
        assert_eq!(
            capability_id("artifact_create").as_deref(),
            Some("artifact.create")
        );
        assert_eq!(
            capability_id("artifact_rewrite").as_deref(),
            Some("artifact.rewrite")
        );
        assert_eq!(capability_id("calc"), None);
    }
}

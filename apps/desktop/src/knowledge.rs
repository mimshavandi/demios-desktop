//! Knowledge search over a project's files (#116, dn-knowledge, ADR-0007).
//!
//! - **Index.** A background worker keeps dn-knowledge in step with Projects: a full reconcile
//!   at start, then the projects [`files_changed`] names. It reads only the app's local copies
//!   (`<data dir>/project-files`, through [`dn_store::project::file_path`]); files are
//!   immutable, so a file id identifies its bytes. Files over dn-knowledge's size cap are not
//!   read at all.
//! - **Embeddings.** With an embedding model downloaded (the Project search slot, or the catalog's
//!   nomic-embed-text v2 MoE, #201), chunks are embedded in batches, outside the database lock,
//!   with nomic's task prefixes. Vectors of different models are never compared: a new model
//!   re-embeds the stored chunks (no file is read again). Without one, search is **keyword only**, and the sources under
//!   each answer say so and how to get better results (no silent degradation).
//! - **Retrieval.** For a chat in a project, the question's best excerpts go into the system
//!   prompt as quoted, cited, untrusted data ([`dn_knowledge::context_block`]), and what the
//!   answer was given is recorded ([`dn_knowledge::record_sources`]) for the UI.
//! - **Delete all** removes the index with the rest (`state.rs`).

use crate::error::{IpcError, IpcResult};
use crate::state::AppState;
use dn_core::Id;
use dn_knowledge::{EmbedKind, Embedder, Index, QueryVector, SearchMode, SourceFile, Sources};
use dn_store::Connection;
use std::sync::{Arc, OnceLock, mpsc};

type AppStateRef<'a> = tauri::State<'a, Arc<AppState>>;

/// The catalog's embedding model, used when the Project search slot is empty: nomic-embed-text v2
/// MoE at Q5_K_M (#201; chosen by the evaluation in #128 and #194).
pub const DEFAULT_EMBED_MODEL: &str = "nomic-embed-text-v2-moe-q5_k_m";
/// The default before #201. Never used unless the user picks it for the Project search slot; if
/// it is still downloaded, Settings says it is no longer used, so it can be removed.
pub const PREVIOUS_EMBED_MODEL: &str = "nomic-embed-text-v1.5-q4_k_m";
/// Excerpts given to the model per question.
const TOP_K: usize = 6;
/// Chunks embedded per call to the model.
const EMBED_BATCH: usize = 16;

enum Job {
    /// Everything: projects that are gone, conversations that are gone, every live project.
    Reconcile,
    Project(Id),
    /// Embed whatever isn't embedded by the current model yet (it was just downloaded or
    /// changed), from the stored chunks: no file is read again.
    EmbedAll,
}

static WORKER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

/// Starts the index worker (once) and queues a full reconcile. Called after the app state is
/// open (Projects has purged what was deleted).
pub fn start(state: &Arc<AppState>) {
    let (tx, rx) = mpsc::channel();
    if WORKER.set(tx.clone()).is_err() {
        return;
    }
    let _ = tx.send(Job::Reconcile);
    let st = Arc::clone(state);
    let spawned = std::thread::Builder::new()
        .name("knowledge-index".into())
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                // Coalesce what queued up meanwhile: one reconcile covers every project.
                let mut reconcile = false;
                let mut projects = Vec::new();
                let mut take = |job: Job| match job {
                    Job::Reconcile => reconcile = true,
                    Job::Project(p) if !projects.contains(&p) => projects.push(p),
                    Job::Project(_) | Job::EmbedAll => {}
                };
                take(first);
                while let Ok(more) = rx.try_recv() {
                    take(more);
                }
                let result = if reconcile {
                    reconcile_all(&st)
                } else {
                    projects
                        .iter()
                        .try_for_each(|p| index_project(&st, *p))
                        .and_then(|()| embed_pending(&st, None))
                };
                if let Err(e) = result {
                    eprintln!("knowledge index: {e}");
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("knowledge index: could not start ({e}); project files won't be searched");
    }
}

/// A project's files changed (added, removed, restored; the project deleted or restored):
/// re-index it in the background.
pub fn files_changed(project_id: Id) {
    if let Some(tx) = WORKER.get() {
        let _ = tx.send(Job::Project(project_id));
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Brings one project's index in step with its live files. A deleted project has no live
/// files, so its index goes.
pub fn index_project(state: &AppState, project_id: Id) -> Result<(), String> {
    let files = {
        let db = state.db().map_err(|e| e.message)?;
        dn_store::project::list_files(&db, project_id).map_err(err)?
    };
    let sources = read_files(state, &files);
    let db = state.db().map_err(|e| e.message)?;
    let report = Index::new(&db)
        .sync_source(&project_id.to_string(), &sources, None)
        .map_err(err)?;
    if !report.skipped.is_empty() {
        eprintln!(
            "knowledge index: {} file(s) of a project are not searchable (not text, or empty)",
            report.skipped.len()
        );
    }
    Ok(())
}

/// The local copies of `files` as dn-knowledge inputs. Too-large files are not read; a copy
/// that can't be read is left out (and so dropped from the index).
fn read_files(state: &AppState, files: &[dn_store::project::ProjectFile]) -> Vec<SourceFile> {
    let Some(root) = state.project_files_root() else {
        return Vec::new();
    };
    files
        .iter()
        .filter(|f| f.size <= dn_knowledge::chunk::MAX_FILE_BYTES as u64)
        .filter_map(|f| {
            let path = dn_store::project::file_path(root, f);
            match std::fs::read(&path) {
                Ok(content) => Some(SourceFile {
                    file_id: f.id.to_string(),
                    display_path: f.name.clone(),
                    content,
                }),
                Err(e) => {
                    eprintln!("knowledge index: a project file's copy can't be read: {e}");
                    None
                }
            }
        })
        .collect()
}

/// Full reconcile: drops the index of projects and the sources of conversations that are
/// gone, re-indexes every live project, then embeds what isn't embedded yet.
fn reconcile_all(state: &AppState) -> Result<(), String> {
    let (live, stale_conversations) = {
        let db = state.db().map_err(|e| e.message)?;
        let mut live: Vec<Id> = dn_store::project::list_all_files(&db)
            .map_err(err)?
            .into_iter()
            .map(|f| f.project_id)
            .collect();
        live.sort();
        live.dedup();
        let gone = |c: &String| {
            serde_json::from_value::<Id>(serde_json::Value::String(c.clone())).map_or(true, |id| {
                matches!(
                    dn_store::chat::get_conversation(&db, id),
                    Err(dn_store::StoreError::NotFound(_))
                )
            })
        };
        let stale: Vec<String> = dn_knowledge::conversations_with_sources(&db)
            .map_err(err)?
            .into_iter()
            .filter(gone)
            .collect();
        (live, stale)
    };
    {
        let db = state.db().map_err(|e| e.message)?;
        let idx = Index::new(&db);
        for s in idx.sources().map_err(err)? {
            if !live.iter().any(|p| p.to_string() == s) {
                idx.remove_source(&s).map_err(err)?;
            }
        }
        for c in &stale_conversations {
            dn_knowledge::remove_conversation_sources(&db, c).map_err(err)?;
        }
    }
    for p in live {
        index_project(state, p)?;
    }
    embed_pending(state, None)
}

/// Embeds chunks that have no embedding from the current model yet, in batches, without
/// holding the database while the model runs. Nothing to do without an embedding model.
fn embed_pending(state: &AppState, source: Option<&str>) -> Result<(), String> {
    match embedder(state)? {
        Some(emb) => embed_pending_with(state, &emb, source),
        None => Ok(()),
    }
}

/// [`embed_pending`] with a given model (tests use a fake one).
fn embed_pending_with(
    state: &AppState,
    emb: &dyn Embedder,
    source: Option<&str>,
) -> Result<(), String> {
    loop {
        let batch = {
            let db = state.db().map_err(|e| e.message)?;
            Index::new(&db)
                .pending_embeddings(source, emb.model_id(), emb.dimension(), EMBED_BATCH)
                .map_err(err)?
        };
        if batch.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
        let vecs = emb.embed(&texts, EmbedKind::Document)?;
        if vecs.len() != batch.len() {
            return Err("the embedding model returned the wrong number of vectors".into());
        }
        let items: Vec<(i64, Vec<f32>)> = batch.iter().map(|(id, _)| *id).zip(vecs).collect();
        let db = state.db().map_err(|e| e.message)?;
        Index::new(&db)
            .store_embeddings(emb.model_id(), emb.dimension(), &items)
            .map_err(err)?;
    }
}

/// Task prefixes per embedding model family: (id fragment, document prefix, query prefix).
/// One table, so a model added for #128 (e5: `passage: ` / `query: `, Qwen3-Embedding: an
/// instruction on queries) is one row. Models not listed get none.
const PREFIXES: &[(&str, &str, &str)] =
    &[("nomic-embed-text", "search_document: ", "search_query: ")];

/// The task prefix `model_id` expects for `kind`.
pub fn prefix(model_id: &str, kind: EmbedKind) -> &'static str {
    PREFIXES
        .iter()
        .find(|(fragment, _, _)| model_id.contains(fragment))
        .map_or("", |(_, doc, query)| match kind {
            EmbedKind::Document => doc,
            EmbedKind::Query => query,
        })
}

/// The local embedding model, loaded: its id and vector size, and a blocking `embed`.
pub struct LocalEmbedder {
    model_id: String,
    dimension: usize,
    model: Arc<dn_inference::LoadedModel<dn_inference::LlamaCppBackend>>,
}

impl LocalEmbedder {
    /// Embeds `texts` (with the model's task prefix). Blocks; call it off the async workers.
    pub fn embed_now(&self, texts: &[String], kind: EmbedKind) -> Result<Vec<Vec<f32>>, String> {
        let p = prefix(&self.model_id, kind);
        let input: Vec<String> = texts.iter().map(|t| format!("{p}{t}")).collect();
        tauri::async_runtime::block_on(self.model.embed(&input)).map_err(err)
    }
}

impl dn_knowledge::Embedder for LocalEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn embed(&self, texts: &[String], kind: EmbedKind) -> Result<Vec<Vec<f32>>, String> {
        self.embed_now(texts, kind)
    }
}

/// Which embedding model to use: the Project search slot if set to a local model, else the
/// catalog's default once it is downloaded. `None`: keyword search only.
fn embed_model_id(state: &AppState) -> Result<Option<String>, String> {
    let Some(local) = state.models.local.as_ref() else {
        return Ok(None);
    };
    let settings = state.settings().map_err(|e| e.message)?;
    let id = match settings.slot("embed") {
        Some(m) => match crate::local::catalogue_id(&m) {
            Some(id) => id.to_owned(),
            // Only local embedding models are supported here.
            None => return Ok(None),
        },
        None => DEFAULT_EMBED_MODEL.to_owned(),
    };
    Ok(local.is_ready(&id).then_some(id))
}

/// The embedding model, loaded on first use. `Ok(None)`: none is downloaded (keyword only).
/// A model that is downloaded but fails to load is an error, not keyword search.
fn embedder(state: &AppState) -> Result<Option<LocalEmbedder>, String> {
    let Some(id) = embed_model_id(state)? else {
        return Ok(None);
    };
    let local = state.models.local.as_ref().ok_or("no local models")?;
    let model = tauri::async_runtime::block_on(local.embedding_model(&id))
        .map_err(|e| format!("the embedding model couldn't be loaded: {e}"))?;
    // The vector size, from the model itself (it fixes the index's dimension guard).
    let probe = tauri::async_runtime::block_on(model.embed(&["dimension probe".to_owned()]))
        .map_err(err)?;
    let dimension = probe.first().map_or(0, Vec::len);
    if dimension == 0 {
        return Err("the embedding model returned no vector".into());
    }
    Ok(Some(LocalEmbedder {
        model_id: format!("local:{id}"),
        dimension,
        model,
    }))
}

/// For a chat in a project: the question's excerpts as a system-prompt section, recording
/// what the answer `message_id` was given. `None` for chats outside a project. Blocking.
pub fn context_for(
    state: &AppState,
    db: &Connection,
    conversation_id: Id,
    message_id: Id,
    question: &str,
) -> IpcResult<Option<String>> {
    let Some(project_id) = dn_store::chat::get_conversation(db, conversation_id)?.project_id else {
        return Ok(None);
    };
    let fail = |e: String| {
        eprintln!("knowledge search: {e}");
        IpcError::new(
            "knowledge",
            "Searching this project's files failed. Try again, or check the embedding model in Settings › Models.",
        )
    };
    let emb = embedder(state).map_err(fail)?;
    let query_vector = match &emb {
        Some(e) if !question.trim().is_empty() => Some(QueryVector {
            model_id: e.model_id.clone(),
            values: e
                .embed_now(&[question.to_owned()], EmbedKind::Query)
                .map_err(fail)?
                .into_iter()
                .next()
                .ok_or_else(|| fail("no vector for the question".into()))?,
        }),
        _ => None,
    };
    let hits = Index::new(db)
        .search_with(
            &[&project_id.to_string()],
            question,
            TOP_K,
            query_vector.as_ref(),
        )
        .map_err(|e| fail(e.to_string()))?;
    let (block, citations) = dn_knowledge::context_block(&hits);
    let mode = if emb.is_some() {
        SearchMode::Hybrid
    } else {
        SearchMode::Keyword
    };
    dn_knowledge::record_sources(
        db,
        &message_id.to_string(),
        &conversation_id.to_string(),
        &Sources { mode, citations },
    )
    .map_err(|e| fail(e.to_string()))?;
    Ok((!block.is_empty()).then_some(block))
}

// ---- the embedding-model offer (#171): never bundled, offered once when it helps.

/// Key of the remembered answer in dn-knowledge's prefs (cleared by Delete all).
const OFFER_KEY: &str = "embedding_model_offer";

/// The user's answer to "Turn on better search?".
#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Offer {
    /// Not answered yet: the project view may ask (once).
    Ask,
    /// "Not now": never asked again (Settings › Models still offers it).
    Declined,
    Accepted,
    /// Accepted for an earlier model (nomic v1.5 before #201), and the current one isn't
    /// downloaded: asked once more, because the new model is a new download. Until then
    /// search is keyword only, and the answers say so.
    Upgrade,
}

/// The stored answer for `model_id`. Stored values: `declined`, `accepted:<model id>`, and
/// plain `accepted` (from before #201, when the only offered model was nomic v1.5).
fn offer(db: &Connection, model_id: &str, ready: bool) -> Result<Offer, String> {
    let stored = dn_knowledge::get_pref(db, OFFER_KEY).map_err(err)?;
    Ok(match stored.as_deref() {
        None => Offer::Ask,
        Some("declined") => Offer::Declined,
        Some(v) if v.strip_prefix("accepted:") == Some(model_id) || ready => Offer::Accepted,
        Some(_) => Offer::Upgrade,
    })
}

/// The embedding model the offer is about: the Project search slot's local model, or the
/// catalog default.
fn offered_model_id(state: &AppState) -> Result<String, String> {
    let settings = state.settings().map_err(|e| e.message)?;
    Ok(settings
        .slot("embed")
        .and_then(|m| crate::local::catalogue_id(&m).map(str::to_owned))
        .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_owned()))
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct OfferedModel {
    pub id: String,
    pub name: String,
    /// Download size in bytes (shown as "371 MB").
    pub size: u64,
    /// Licence id from the model card (shown with the offer; accepted by downloading).
    pub license: String,
    pub state: crate::local::DownloadState,
}

impl OfferedModel {
    fn of(r: crate::local::LocalModelRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            size: r.size,
            license: r.license,
            state: r.state,
        }
    }
}

#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbedProgress {
    pub done: u64,
    pub total: u64,
}

/// What the UI needs for the offer and the progress of "improving search".
#[derive(serde::Serialize, Clone, Debug)]
pub struct KnowledgeStatus {
    pub offer: Offer,
    /// `None`: no local models in this build (nothing to offer).
    pub model: Option<OfferedModel>,
    /// While the model is installed and the index isn't fully embedded by it yet.
    pub embedding: Option<EmbedProgress>,
    /// The earlier default (nomic v1.5), while it is still downloaded but no longer used for
    /// search: Settings says so, and it can be removed there.
    pub unused: Option<OfferedModel>,
}

/// The offer and progress. If the model is installed and chunks still wait for it (e.g. it
/// was just downloaded), embedding them is queued here.
pub fn status(state: &AppState) -> Result<KnowledgeStatus, String> {
    let id = offered_model_id(state)?;
    let rows = state
        .models
        .local
        .as_ref()
        .map(|local| local.rows())
        .unwrap_or_default();
    let model = rows
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .map(OfferedModel::of);
    let ready = matches!(
        model.as_ref().map(|m| &m.state),
        Some(crate::local::DownloadState::Ready)
    );
    let unused = rows
        .into_iter()
        .find(|r| {
            r.id == PREVIOUS_EMBED_MODEL
                && r.id != id
                && r.state == crate::local::DownloadState::Ready
        })
        .map(OfferedModel::of);
    let offer = offer(&*state.db().map_err(|e| e.message)?, &id, ready)?;
    let embedding = if ready {
        let (done, total) = dn_knowledge::embedding_counts(
            &*state.db().map_err(|e| e.message)?,
            &format!("local:{id}"),
        )
        .map_err(err)?;
        (done < total).then(|| {
            if let Some(tx) = WORKER.get() {
                let _ = tx.send(Job::EmbedAll);
            }
            EmbedProgress { done, total }
        })
    } else {
        None
    };
    Ok(KnowledgeStatus {
        offer,
        model,
        embedding,
        unused,
    })
}

/// "Download" (`accept`) or "Not now". Download starts the normal verified download of the
/// model; once it is ready, [`status`] queues embedding the existing chunks (re-embedding
/// those an earlier model embedded, from the stored text).
pub fn answer_offer(state: &AppState, accept: bool) -> Result<(), String> {
    let id = offered_model_id(state)?;
    dn_knowledge::set_pref(
        &*state.db().map_err(|e| e.message)?,
        OFFER_KEY,
        &if accept {
            format!("accepted:{id}")
        } else {
            "declined".to_owned()
        },
    )
    .map_err(err)?;
    if !accept {
        return Ok(());
    }
    let local = state
        .models
        .local
        .as_ref()
        .ok_or("local models aren't available in this build")?;
    if !local.is_ready(&id) {
        local.download(&id)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn knowledge_status(state: AppStateRef<'_>) -> IpcResult<KnowledgeStatus> {
    crate::commands::blocking(&state, |s| {
        status(s).map_err(|e| IpcError::new("knowledge", e))
    })
    .await
}

#[tauri::command]
pub async fn knowledge_offer(state: AppStateRef<'_>, accept: bool) -> IpcResult<()> {
    crate::commands::blocking(&state, move |s| {
        answer_offer(s, accept).map_err(|e| IpcError::new("knowledge", e))
    })
    .await
}

/// What the answer `message_id` was given (for the sources under it), or `None` when it
/// wasn't a project chat.
#[tauri::command]
pub async fn message_sources(state: AppStateRef<'_>, message_id: Id) -> IpcResult<Option<Sources>> {
    crate::commands::blocking(&state, move |s| {
        dn_knowledge::sources_for(&*s.db()?, &message_id.to_string())
            .map_err(|e| IpcError::new("knowledge", e.to_string()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic stand-in for the embedding model.
    struct Fake(&'static str);
    impl Embedder for Fake {
        fn model_id(&self) -> &str {
            self.0
        }
        fn dimension(&self) -> usize {
            4
        }
        fn embed(&self, texts: &[String], _: EmbedKind) -> Result<Vec<Vec<f32>>, String> {
            Ok(texts.iter().map(|_| vec![0.5; 4]).collect())
        }
    }

    #[test]
    fn a_model_added_later_embeds_the_stored_chunks_without_reading_files_and_a_new_model_re_embeds()
     {
        // No project-files folder at all: embedding can't read a file even if it tried.
        let st = AppState::in_memory().unwrap();
        assert!(st.project_files_root().is_none());
        {
            let db = st.db().unwrap();
            Index::new(&db)
                .sync_source(
                    "p",
                    &[SourceFile {
                        file_id: "f".into(),
                        display_path: "plan.md".into(),
                        content: b"The launch is in May.".to_vec(),
                    }],
                    None,
                )
                .unwrap();
        }
        let counts = |m: &str| dn_knowledge::embedding_counts(&st.db().unwrap(), m).unwrap();
        assert_eq!(counts("local:a"), (0, 1));
        embed_pending_with(&st, &Fake("local:a"), None).unwrap();
        assert_eq!(counts("local:a"), (1, 1));
        embed_pending_with(&st, &Fake("local:b"), None).unwrap();
        assert_eq!(
            counts("local:b"),
            (1, 1),
            "another model re-embeds by model id"
        );
        assert_eq!(counts("local:a"), (0, 1));
    }

    #[test]
    fn not_now_is_remembered_and_never_asked_again() {
        let st = AppState::in_memory().unwrap();
        assert_eq!(status(&st).unwrap().offer, Offer::Ask);
        answer_offer(&st, false).unwrap();
        assert_eq!(status(&st).unwrap().offer, Offer::Declined);
        assert_eq!(
            status(&st).unwrap().offer,
            Offer::Declined,
            "it stays declined"
        );
    }

    #[test]
    fn nomic_gets_its_task_prefixes_and_other_models_none() {
        for id in [DEFAULT_EMBED_MODEL, PREVIOUS_EMBED_MODEL] {
            let id = format!("local:{id}");
            assert_eq!(prefix(&id, EmbedKind::Document), "search_document: ");
            assert_eq!(prefix(&id, EmbedKind::Query), "search_query: ");
        }
        assert_eq!(prefix("local:bge-small", EmbedKind::Query), "");
    }

    #[test]
    fn the_default_is_nomic_v2_q5_k_m_pinned_in_the_catalogue() {
        let c = dn_models::Catalog::builtin();
        let m = c.get(DEFAULT_EMBED_MODEL).expect("in the catalogue");
        assert_eq!(m.capabilities, [dn_core::model::Capability::Embedding]);
        assert_eq!(m.quant, "Q5_K_M");
        assert_eq!(m.size, 370_828_256);
        assert_eq!(
            m.sha256,
            "a6546bdf4e46288c93d79e6292bbdd8efa7ca87c242a8e60afa9cbacdc641471"
        );
        assert_eq!(m.license, "apache-2.0");
        assert_eq!(
            m.url(),
            "https://huggingface.co/nomic-ai/nomic-embed-text-v2-moe-GGUF/resolve/ffbcf4c99e5d617dda10ec8c0e9f75754b0cbb80/nomic-embed-text-v2-moe.Q5_K_M.gguf"
        );
        // The old default stays listed only so a downloaded copy can be seen and removed.
        assert!(c.get(PREVIOUS_EMBED_MODEL).is_some());
    }

    #[test]
    fn the_stored_answer_maps_to_the_offer() {
        let st = AppState::in_memory().unwrap();
        let db = st.db().unwrap();
        let v2 = DEFAULT_EMBED_MODEL;
        assert_eq!(offer(&db, v2, false).unwrap(), Offer::Ask);
        for (stored, ready, want) in [
            ("declined", false, Offer::Declined),
            ("declined", true, Offer::Declined),
            // Before #201 "accepted" meant nomic v1.5: the new model is a new download.
            ("accepted", false, Offer::Upgrade),
            ("accepted", true, Offer::Accepted),
            (
                "accepted:nomic-embed-text-v1.5-q4_k_m",
                false,
                Offer::Upgrade,
            ),
            (
                "accepted:nomic-embed-text-v2-moe-q5_k_m",
                false,
                Offer::Accepted,
            ),
        ] {
            dn_knowledge::set_pref(&db, OFFER_KEY, stored).unwrap();
            assert_eq!(
                offer(&db, v2, ready).unwrap(),
                want,
                "{stored} ready={ready}"
            );
        }
    }

    /// A models folder where the given catalogue models are downloaded and verified (sparse
    /// files of the catalogue size, with the checksum marker).
    fn with_downloaded(st: &mut AppState, dir: &std::path::Path, ids: &[&str]) {
        let catalog = dn_models::Catalog::builtin();
        std::fs::create_dir_all(dir).unwrap();
        for id in ids {
            let e = catalog.get(id).unwrap();
            let f = std::fs::File::create(dir.join(&e.file)).unwrap();
            f.set_len(e.size).unwrap();
            std::fs::write(dir.join(format!("{}.sha256", e.file)), &e.sha256).unwrap();
        }
        st.models.local = Some(Arc::new(crate::local::LocalModels::new(
            dir.to_path_buf(),
            catalog,
            dn_models::Machine::cpu_only(16 << 30),
        )));
    }

    #[test]
    fn a_v15_index_is_never_searched_with_v15_and_re_embeds_with_v2_after_the_download() {
        let dir = std::env::temp_dir().join(format!("dn-knowledge-v2-{}", Id::new()));
        let mut st = AppState::in_memory().unwrap();
        // An existing user: accepted the offer before #201, nomic v1.5 downloaded, and the
        // project's passages embedded with it.
        with_downloaded(&mut st, &dir, &[PREVIOUS_EMBED_MODEL]);
        let v15 = format!("local:{PREVIOUS_EMBED_MODEL}");
        let v2 = format!("local:{DEFAULT_EMBED_MODEL}");
        {
            let db = st.db().unwrap();
            dn_knowledge::set_pref(&db, OFFER_KEY, "accepted").unwrap();
            Index::new(&db)
                .sync_source(
                    "p",
                    &[SourceFile {
                        file_id: "f".into(),
                        display_path: "plan.md".into(),
                        content: b"Der Start ist im Mai.".to_vec(),
                    }],
                    None,
                )
                .unwrap();
        }
        embed_pending_with(&st, &Fake("local:nomic-embed-text-v1.5-q4_k_m"), None).unwrap();
        let counts =
            |st: &AppState, m: &str| dn_knowledge::embedding_counts(&st.db().unwrap(), m).unwrap();
        assert_eq!(counts(&st, &v15), (1, 1));

        // v1.5 is no fallback: until v2 is downloaded, search is keyword only (and says so).
        assert_eq!(embed_model_id(&st).unwrap(), None);
        let s = status(&st).unwrap();
        assert_eq!(
            s.offer,
            Offer::Upgrade,
            "asked once more, for the new download"
        );
        let m = s.model.unwrap();
        assert_eq!(m.id, DEFAULT_EMBED_MODEL);
        assert_eq!((m.size, m.license.as_str()), (370_828_256, "apache-2.0"));
        assert_eq!(m.state, crate::local::DownloadState::NotDownloaded);
        assert_eq!(s.unused.unwrap().id, PREVIOUS_EMBED_MODEL);
        assert!(s.embedding.is_none());

        // After the user's download: v2 is used, and the stored chunks are re-embedded with
        // it (no project-files folder exists, so no file can be read).
        with_downloaded(&mut st, &dir, &[PREVIOUS_EMBED_MODEL, DEFAULT_EMBED_MODEL]);
        assert!(st.project_files_root().is_none());
        assert_eq!(
            embed_model_id(&st).unwrap().as_deref(),
            Some(DEFAULT_EMBED_MODEL)
        );
        let s = status(&st).unwrap();
        assert_eq!(s.offer, Offer::Accepted);
        assert_eq!(s.embedding, Some(EmbedProgress { done: 0, total: 1 }));
        embed_pending_with(&st, &Fake("local:nomic-embed-text-v2-moe-q5_k_m"), None).unwrap();
        assert_eq!(counts(&st, &v2), (1, 1));
        assert_eq!(
            counts(&st, &v15),
            (0, 1),
            "no v1.5 vector is left to mix in"
        );
        let s = status(&st).unwrap();
        assert!(s.embedding.is_none());
        assert_eq!(
            s.unused.map(|m| m.id).as_deref(),
            Some(PREVIOUS_EMBED_MODEL),
            "the old file is pointed out, so it can be removed"
        );
        // "Download" records the model it was for.
        answer_offer(&st, true).unwrap();
        assert_eq!(
            dn_knowledge::get_pref(&st.db().unwrap(), OFFER_KEY)
                .unwrap()
                .as_deref(),
            Some("accepted:nomic-embed-text-v2-moe-q5_k_m")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_an_embedding_model_search_is_keyword_only_and_recorded_as_such() {
        let st = AppState::in_memory().unwrap();
        let db = st.db().unwrap();
        let p = dn_store::project::create_project(&db, "Thesis").unwrap();
        let conv = dn_store::chat::create_conversation_in(&db, "New chat", Some(p.id)).unwrap();
        Index::new(&db)
            .sync_source(
                &p.id.to_string(),
                &[SourceFile {
                    file_id: "f".into(),
                    display_path: "plan.md".into(),
                    content: b"The launch is on the 14th of May.".to_vec(),
                }],
                None,
            )
            .unwrap();
        let msg = Id::new();
        let block = context_for(&st, &db, conv.id, msg, "when is the launch?")
            .unwrap()
            .expect("a project chat gets excerpts");
        assert!(block.contains("file=\"plan.md\""));
        assert!(block.contains("untrusted data"));
        let s = dn_knowledge::sources_for(&db, &msg.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(s.mode, SearchMode::Keyword);
        assert_eq!(s.citations[0].display_path, "plan.md");
        // A chat outside a project gets nothing, and nothing is recorded.
        let other = dn_store::chat::create_conversation(&db, "New chat").unwrap();
        let m2 = Id::new();
        assert!(
            context_for(&st, &db, other.id, m2, "when is the launch?")
                .unwrap()
                .is_none()
        );
        assert!(
            dn_knowledge::sources_for(&db, &m2.to_string())
                .unwrap()
                .is_none()
        );
    }
}

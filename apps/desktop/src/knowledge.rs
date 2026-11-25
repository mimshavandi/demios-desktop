//! Knowledge search over a project's files (#116, dn-knowledge, ADR-0007).
//!
//! - **Index.** A background worker keeps dn-knowledge in step with Projects: a full reconcile
//!   at start, then the projects [`files_changed`] names. It reads only the app's local copies
//!   (`<data dir>/project-files`, through [`dn_store::project::file_path`]); files are
//!   immutable, so a file id identifies its bytes. Files over dn-knowledge's size cap are not
//!   read at all.
//! - **Embeddings.** With an embedding model downloaded (the Embeddings slot, or the catalog's
//!   nomic-embed-text v1.5), chunks are embedded in batches, outside the database lock, with
//!   nomic's task prefixes. Without one, search is **keyword only**, and the sources under
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

/// The catalog's embedding model (#131), used when the Embeddings slot is empty.
pub const DEFAULT_EMBED_MODEL: &str = "nomic-embed-text-v1.5-q4_k_m";
/// Excerpts given to the model per question.
const TOP_K: usize = 6;
/// Chunks embedded per call to the model.
const EMBED_BATCH: usize = 16;

enum Job {
    /// Everything: projects that are gone, conversations that are gone, every live project.
    Reconcile,
    Project(Id),
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
                let mut reconcile = matches!(first, Job::Reconcile);
                let mut projects = Vec::new();
                if let Job::Project(p) = first {
                    projects.push(p);
                }
                while let Ok(more) = rx.try_recv() {
                    match more {
                        Job::Reconcile => reconcile = true,
                        Job::Project(p) if !projects.contains(&p) => projects.push(p),
                        Job::Project(_) => {}
                    }
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
    let Some(emb) = embedder(state)? else {
        return Ok(());
    };
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
        let vecs = emb.embed_now(&texts, EmbedKind::Document)?;
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

/// Task prefixes a retrieval model expects (nomic-embed-text: `search_document: ` /
/// `search_query: `); none for other models.
pub fn prefix(model_id: &str, kind: EmbedKind) -> &'static str {
    if !model_id.contains("nomic-embed-text") {
        return "";
    }
    match kind {
        EmbedKind::Document => "search_document: ",
        EmbedKind::Query => "search_query: ",
    }
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

/// Which embedding model to use: the Embeddings slot if set to a local model, else the
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

    #[test]
    fn nomic_gets_its_task_prefixes_and_other_models_none() {
        assert_eq!(
            prefix("local:nomic-embed-text-v1.5-q4_k_m", EmbedKind::Document),
            "search_document: "
        );
        assert_eq!(
            prefix("local:nomic-embed-text-v1.5-q4_k_m", EmbedKind::Query),
            "search_query: "
        );
        assert_eq!(prefix("local:bge-small", EmbedKind::Query), "");
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

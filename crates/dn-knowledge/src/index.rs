//! The index: `knowledge_*` tables in the app database, kept in step with a source's files, and
//! hybrid search over them (BM25 through FTS5, plus embeddings, merged by reciprocal rank).

use crate::chunk::{self, Skip};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

/// This crate's tables (run by dn-store with the other crates' migrations).
pub fn migrations() -> &'static [&'static str] {
    &[
        r#"
CREATE TABLE knowledge_files (
    source_id    TEXT NOT NULL,
    file_id      TEXT NOT NULL,
    display_path TEXT NOT NULL,
    sha256       TEXT NOT NULL,
    PRIMARY KEY (source_id, file_id)
);
CREATE TABLE knowledge_chunks (
    id          INTEGER PRIMARY KEY,
    source_id   TEXT NOT NULL,
    file_id     TEXT NOT NULL,
    start_line  INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    text        TEXT NOT NULL,
    -- little-endian f32 vector, L2-normalised; NULL until embedded
    embedding   BLOB,
    -- the Embedder::model_id and dimension that made `embedding`: a vector is only ever
    -- compared with vectors of the same model and size; another model re-embeds
    embed_model TEXT,
    embed_dim   INTEGER
);
CREATE INDEX knowledge_chunks_by_file ON knowledge_chunks (source_id, file_id);
CREATE VIRTUAL TABLE knowledge_fts USING fts5(
    text, content='knowledge_chunks', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER knowledge_chunks_ai AFTER INSERT ON knowledge_chunks BEGIN
    INSERT INTO knowledge_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER knowledge_chunks_ad AFTER DELETE ON knowledge_chunks BEGIN
    INSERT INTO knowledge_fts(knowledge_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
"#,
        r#"
-- The excerpts an answer was given, so the app can show them with it (file and lines only, no
-- text). One row per assistant message; removed with the conversation and by delete_all.
CREATE TABLE knowledge_sources (
    message_id      TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL,
    -- 'hybrid' (keywords + embeddings) or 'keyword' (no embedding model)
    mode            TEXT NOT NULL,
    -- JSON array of Citation
    citations       TEXT NOT NULL
);
CREATE INDEX knowledge_sources_by_conversation ON knowledge_sources (conversation_id);
"#,
        r#"
-- Small preferences of knowledge search (e.g. the answer to the embedding-model offer).
-- Removed by delete_all.
CREATE TABLE knowledge_prefs (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
"#,
    ]
}

/// How a search ranked: both ways, or keywords only (no embedding model installed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Hybrid,
    Keyword,
}

impl SearchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Keyword => "keyword",
        }
    }
}

/// What an answer was given: how it was searched and which excerpts (see [`crate::context_block`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sources {
    pub mode: SearchMode,
    pub citations: Vec<crate::Citation>,
}

/// A query's embedding, made by the caller (so it needn't hold the database while the model runs).
#[derive(Debug, Clone)]
pub struct QueryVector {
    pub model_id: String,
    pub values: Vec<f32>,
}

/// Errors from the index.
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    #[error("knowledge index: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("embedding model: {0}")]
    Embed(String),
}

pub type Result<T> = std::result::Result<T, KnowledgeError>;

/// What a text is embedded as. Retrieval models like nomic-embed-text need a different task
/// prefix for each ("search_document: " / "search_query: "); the [`Embedder`] applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedKind {
    /// A chunk being indexed.
    Document,
    /// A search query.
    Query,
}

/// Makes embeddings with a local model. The app adapts `dn_inference`'s `embed` to this. Vectors
/// must be L2-normalised (dot product = cosine similarity) and [`Embedder::dimension`] long.
pub trait Embedder {
    /// Identifies the model; chunks embedded by another model are re-embedded.
    fn model_id(&self) -> &str;
    /// Length of every vector this model makes.
    fn dimension(&self) -> usize;
    fn embed(
        &self,
        texts: &[String],
        kind: EmbedKind,
    ) -> std::result::Result<Vec<Vec<f32>>, String>;
}

/// A file of a source, as the app hands it over. dn-knowledge never opens paths itself.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Stable id from the owner (e.g. Projects); survives renames.
    pub file_id: String,
    /// What citations show, e.g. `docs/plan.md`.
    pub display_path: String,
    pub content: Vec<u8>,
}

/// What [`Index::sync_source`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SyncReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub skipped: Vec<(String, Skip)>,
    pub chunks: usize,
    /// Chunks embedded in this sync (0 without an embedder).
    pub embedded: usize,
}

/// A search result: the chunk, where it is, and its fused score (higher is better).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Hit {
    pub chunk_id: i64,
    pub source_id: String,
    pub file_id: String,
    pub display_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
    pub score: f64,
}

/// Counts for "Delete all local data" and the UI.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    pub files: u64,
    pub chunks: u64,
}

/// Chunks embedded per call to the model.
const EMBED_BATCH: usize = 16;
/// Reciprocal-rank-fusion constant (the usual 60).
const RRF_K: f64 = 60.0;
/// Most results a search returns.
pub const MAX_RESULTS: usize = 50;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// An FTS5 query from user text: each word quoted (no FTS syntax from the user), OR-ed, so
/// BM25 ranks chunks with more and rarer matching words first.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .take(32)
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect();
    (!words.is_empty()).then(|| words.join(" OR "))
}

/// The index over a database connection the app passes in.
pub struct Index<'c> {
    conn: &'c Connection,
}

impl<'c> Index<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Brings `source_id` in step with `files`: new or changed files are (re)chunked, unchanged
    /// ones (same SHA-256) kept, files no longer listed removed. With an `embedder`, chunks
    /// without an embedding from that model are embedded.
    pub fn sync_source(
        &self,
        source_id: &str,
        files: &[SourceFile],
        embedder: Option<&dyn Embedder>,
    ) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        // Chunks deleted (files removed or re-indexed): their text must leave the FTS index too.
        let mut dropped = 0;
        let tx = self.conn.unchecked_transaction()?;
        let listed: std::collections::HashSet<&str> =
            files.iter().map(|f| f.file_id.as_str()).collect();
        let known: Vec<(String, String)> = {
            let mut st =
                tx.prepare("SELECT file_id, sha256 FROM knowledge_files WHERE source_id = ?1")?;
            st.query_map([source_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        for (file_id, _) in &known {
            if !listed.contains(file_id.as_str()) {
                dropped += remove_file(&tx, source_id, file_id)?;
                report.removed += 1;
            }
        }
        for f in files {
            let text = match chunk::text_of(&f.content) {
                Ok(t) => t,
                Err(skip) => {
                    if known.iter().any(|(id, _)| id == &f.file_id) {
                        dropped += remove_file(&tx, source_id, &f.file_id)?;
                    }
                    report.skipped.push((f.display_path.clone(), skip));
                    continue;
                }
            };
            let sha = sha256_hex(&f.content);
            if known.iter().any(|(id, s)| id == &f.file_id && s == &sha) {
                tx.execute(
                    "UPDATE knowledge_files SET display_path = ?3 WHERE source_id = ?1 AND file_id = ?2",
                    params![source_id, f.file_id, f.display_path],
                )?;
                report.unchanged += 1;
                continue;
            }
            dropped += remove_file(&tx, source_id, &f.file_id)?;
            tx.execute(
                "INSERT INTO knowledge_files (source_id, file_id, display_path, sha256) VALUES (?1, ?2, ?3, ?4)",
                params![source_id, f.file_id, f.display_path, sha],
            )?;
            for c in chunk::chunk(text) {
                tx.execute(
                    "INSERT INTO knowledge_chunks (source_id, file_id, start_line, end_line, text) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![source_id, f.file_id, c.start_line, c.end_line, c.text],
                )?;
                report.chunks += 1;
            }
            report.indexed += 1;
        }
        if dropped > 0 {
            optimize(&tx)?;
        }
        tx.commit()?;
        if let Some(e) = embedder {
            report.embedded = self.embed_missing(source_id, e)?;
        }
        Ok(report)
    }

    /// Embeds the source's chunks that have no embedding from `embedder`'s model and dimension.
    fn embed_missing(&self, source_id: &str, embedder: &dyn Embedder) -> Result<usize> {
        let mut done = 0;
        loop {
            let batch = self.pending_embeddings(
                Some(source_id),
                embedder.model_id(),
                embedder.dimension(),
                EMBED_BATCH,
            )?;
            if batch.is_empty() {
                return Ok(done);
            }
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let vecs = embedder
                .embed(&texts, EmbedKind::Document)
                .map_err(KnowledgeError::Embed)?;
            if vecs.len() != batch.len() {
                return Err(KnowledgeError::Embed(format!(
                    "the model returned {} vectors for {} texts",
                    vecs.len(),
                    batch.len()
                )));
            }
            let items: Vec<(i64, Vec<f32>)> = batch.iter().map(|(id, _)| *id).zip(vecs).collect();
            done += self.store_embeddings(embedder.model_id(), embedder.dimension(), &items)?;
        }
    }

    /// Up to `limit` chunks (id, text) that have no embedding from this model and dimension, in
    /// one source or all. With [`Index::store_embeddings`], lets the caller run the model
    /// without holding the database.
    pub fn pending_embeddings(
        &self,
        source_id: Option<&str>,
        model_id: &str,
        dim: usize,
        limit: usize,
    ) -> Result<Vec<(i64, String)>> {
        let dim = i64::try_from(dim).unwrap_or(i64::MAX);
        let limit = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
        let mut st = self.conn.prepare(
            "SELECT id, text FROM knowledge_chunks WHERE (?1 IS NULL OR source_id = ?1) \
             AND (embed_model IS NULL OR embed_model != ?2 OR embed_dim IS NULL OR embed_dim != ?3) \
             ORDER BY id LIMIT ?4",
        )?;
        let rows = st
            .query_map(params![source_id, model_id, dim, limit], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// Stores embeddings made by `model_id` for chunk ids (from [`Index::pending_embeddings`]).
    /// Every vector must be `dim` long; chunks removed meanwhile are skipped. Returns how many
    /// were stored.
    pub fn store_embeddings(
        &self,
        model_id: &str,
        dim: usize,
        items: &[(i64, Vec<f32>)],
    ) -> Result<usize> {
        if let Some((_, v)) = items.iter().find(|(_, v)| v.len() != dim) {
            return Err(KnowledgeError::Embed(format!(
                "a vector of size {} from a model of size {dim}",
                v.len()
            )));
        }
        let d = i64::try_from(dim).unwrap_or(i64::MAX);
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        for (id, v) in items {
            n += tx.execute(
                "UPDATE knowledge_chunks SET embedding = ?2, embed_model = ?3, embed_dim = ?4 WHERE id = ?1",
                params![id, to_blob(v), model_id, d],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Removes everything indexed for a source (e.g. a deleted project). The FTS index is
    /// optimised afterwards: FTS5 only marks deleted rows, and their text would otherwise stay in
    /// its shadow tables until a later merge.
    pub fn remove_source(&self, source_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM knowledge_chunks WHERE source_id = ?1",
            [source_id],
        )?;
        tx.execute(
            "DELETE FROM knowledge_files WHERE source_id = ?1",
            [source_id],
        )?;
        optimize(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// The `k` best chunks for `query` in `sources`: BM25 and (with an embedder) vector
    /// similarity, merged by reciprocal rank fusion. Only chunks embedded by the same model take
    /// part in the vector ranking.
    pub fn search(
        &self,
        sources: &[&str],
        query: &str,
        k: usize,
        embedder: Option<&dyn Embedder>,
    ) -> Result<Vec<Hit>> {
        let qv = match embedder {
            Some(e) if !query.trim().is_empty() => Some(QueryVector {
                model_id: e.model_id().to_owned(),
                values: e
                    .embed(&[query.to_owned()], EmbedKind::Query)
                    .map_err(KnowledgeError::Embed)?
                    .into_iter()
                    .next()
                    .filter(|v| v.len() == e.dimension())
                    .ok_or_else(|| {
                        KnowledgeError::Embed("no vector of the right size for the query".into())
                    })?,
            }),
            _ => None,
        };
        self.search_with(sources, query, k, qv.as_ref())
    }

    /// [`Index::search`] with the query's embedding made by the caller (`None`: BM25 only).
    /// Only chunks embedded by the same model with the same dimension take part.
    pub fn search_with(
        &self,
        sources: &[&str],
        query: &str,
        k: usize,
        query_vector: Option<&QueryVector>,
    ) -> Result<Vec<Hit>> {
        let k = k.clamp(1, MAX_RESULTS);
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let depth = k * 4;
        let in_sources = format!(
            "c.source_id IN ({})",
            (0..sources.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",")
        );
        let mut fused: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();

        if let Some(q) = fts_query(query) {
            let sql = format!(
                "SELECT c.id FROM knowledge_fts f JOIN knowledge_chunks c ON c.id = f.rowid \
                 WHERE knowledge_fts MATCH ?1 AND {in_sources} ORDER BY bm25(knowledge_fts) LIMIT {depth}"
            );
            let mut st = self.conn.prepare(&sql)?;
            let mut args: Vec<&dyn rusqlite::ToSql> = vec![&q];
            for s in sources {
                args.push(s);
            }
            let ids: Vec<i64> = st
                .query_map(args.as_slice(), |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            for (rank, id) in ids.into_iter().enumerate() {
                *fused.entry(id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
            }
        }

        if let Some(qv) = query_vector {
            let model = format!("{}\u{0}{}", qv.model_id, qv.values.len());
            let sql = format!(
                "SELECT c.id, c.embedding FROM knowledge_chunks c \
                 WHERE c.embed_model || char(0) || c.embed_dim = ?1 AND c.embedding IS NOT NULL AND {in_sources}"
            );
            let mut st = self.conn.prepare(&sql)?;
            let mut args: Vec<&dyn rusqlite::ToSql> = vec![&model];
            for s in sources {
                args.push(s);
            }
            let mut scored: Vec<(i64, f32)> = st
                .query_map(args.as_slice(), |r| {
                    let id: i64 = r.get(0)?;
                    let blob: Vec<u8> = r.get(1)?;
                    Ok((id, blob))
                })?
                .filter_map(|r| r.ok())
                .filter_map(|(id, blob)| {
                    let v = from_blob(&blob);
                    (v.len() == qv.values.len()).then(|| (id, dot(&qv.values, &v)))
                })
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (rank, (id, _)) in scored.into_iter().take(depth).enumerate() {
                *fused.entry(id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
            }
        }

        let mut ranked: Vec<(i64, f64)> = fused.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(k);
        let mut out = Vec::with_capacity(ranked.len());
        for (id, score) in ranked {
            let hit = self
                .conn
                .query_row(
                    "SELECT c.source_id, c.file_id, f.display_path, c.start_line, c.end_line, c.text \
                     FROM knowledge_chunks c JOIN knowledge_files f ON f.source_id = c.source_id AND f.file_id = c.file_id \
                     WHERE c.id = ?1",
                    [id],
                    |r| {
                        Ok(Hit {
                            chunk_id: id,
                            source_id: r.get(0)?,
                            file_id: r.get(1)?,
                            display_path: r.get(2)?,
                            start_line: r.get(3)?,
                            end_line: r.get(4)?,
                            text: r.get(5)?,
                            score,
                        })
                    },
                )
                .optional()?;
            out.extend(hit);
        }
        Ok(out)
    }

    /// Every source that has something indexed (for a full reconcile).
    pub fn sources(&self) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT DISTINCT source_id FROM knowledge_files ORDER BY source_id")?;
        let out = st
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(out)
    }

    pub fn counts(&self) -> Result<Counts> {
        Ok(Counts {
            files: self
                .conn
                .query_row("SELECT COUNT(*) FROM knowledge_files", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map(|n| u64::try_from(n).unwrap_or(0))?,
            chunks: self
                .conn
                .query_row("SELECT COUNT(*) FROM knowledge_chunks", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map(|n| u64::try_from(n).unwrap_or(0))?,
        })
    }
}

/// Deletes a file's rows; returns how many chunks went.
fn remove_file(conn: &Connection, source_id: &str, file_id: &str) -> Result<usize> {
    let chunks = conn.execute(
        "DELETE FROM knowledge_chunks WHERE source_id = ?1 AND file_id = ?2",
        params![source_id, file_id],
    )?;
    conn.execute(
        "DELETE FROM knowledge_files WHERE source_id = ?1 AND file_id = ?2",
        params![source_id, file_id],
    )?;
    Ok(chunks)
}

/// Merges the FTS index so deleted rows' text leaves its shadow tables now (FTS5 only marks
/// deletions until segments merge). With `secure_delete` (dn-store sets it on every app
/// connection) the freed pages are zeroed as well.
fn optimize(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO knowledge_fts(knowledge_fts) VALUES ('optimize')",
        [],
    )?;
    Ok(())
}

/// Records what the answer `message_id` was given (replaces an earlier record, e.g. regenerate).
pub fn record_sources(
    conn: &Connection,
    message_id: &str,
    conversation_id: &str,
    sources: &Sources,
) -> Result<()> {
    let json = serde_json::to_string(&sources.citations)
        .map_err(|e| KnowledgeError::Embed(format!("citations: {e}")))?;
    conn.execute(
        "INSERT INTO knowledge_sources (message_id, conversation_id, mode, citations) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(message_id) DO UPDATE SET conversation_id = ?2, mode = ?3, citations = ?4",
        params![message_id, conversation_id, sources.mode.as_str(), json],
    )?;
    Ok(())
}

/// What the answer `message_id` was given, if it was given project excerpts.
pub fn sources_for(conn: &Connection, message_id: &str) -> Result<Option<Sources>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT mode, citations FROM knowledge_sources WHERE message_id = ?1",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((mode, json)) = row else {
        return Ok(None);
    };
    let mode = if mode == "hybrid" {
        SearchMode::Hybrid
    } else {
        SearchMode::Keyword
    };
    let citations = serde_json::from_str(&json)
        .map_err(|e| KnowledgeError::Embed(format!("stored citations: {e}")))?;
    Ok(Some(Sources { mode, citations }))
}

/// How many chunks are embedded by `model_id`, out of all chunks (progress of embedding the
/// index after a model is added or changed).
pub fn embedding_counts(conn: &Connection, model_id: &str) -> Result<(u64, u64)> {
    let (done, total): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(embed_model = ?1 AND embedding IS NOT NULL), 0), COUNT(*) FROM knowledge_chunks",
        [model_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((
        u64::try_from(done).unwrap_or(0),
        u64::try_from(total).unwrap_or(0),
    ))
}

/// A stored preference, if set.
pub fn get_pref(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM knowledge_prefs WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}

/// Stores a preference (replacing an earlier value).
pub fn set_pref(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO knowledge_prefs (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}

/// Conversations that have recorded sources (for removing those of deleted conversations).
pub fn conversations_with_sources(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT DISTINCT conversation_id FROM knowledge_sources")?;
    let out = st
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(out)
}

/// Removes the recorded sources of a conversation (deleted or purged).
pub fn remove_conversation_sources(conn: &Connection, conversation_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM knowledge_sources WHERE conversation_id = ?1",
        [conversation_id],
    )?;
    Ok(())
}

/// "Delete all local data": removes every indexed file and chunk, and rebuilds the FTS index so
/// no text is left in its shadow tables.
pub fn delete_all(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM knowledge_chunks", [])?;
    tx.execute("DELETE FROM knowledge_files", [])?;
    tx.execute("DELETE FROM knowledge_sources", [])?;
    tx.execute("DELETE FROM knowledge_prefs", [])?;
    tx.execute(
        "INSERT INTO knowledge_fts(knowledge_fts) VALUES ('rebuild')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

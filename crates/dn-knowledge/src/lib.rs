//! Local knowledge index over project files (ADR-0007, issue #116).
//!
//! - **Chunks** of about 40 lines with their file line numbers ([`chunk`]), text files only.
//! - **Hybrid search**: BM25 through SQLite FTS5, plus vector similarity over embeddings from
//!   a local model ([`Embedder`], which the app backs with `dn-inference`), merged by reciprocal
//!   rank fusion. Without an embedder, search is BM25 only.
//! - **Storage**: `knowledge_*` tables in the app database ([`migrations`]), so the index lives
//!   in the app data folder; [`delete_all`] is part of "Delete all local data".
//! - **No network**: nothing here talks to anything; the app hands over file bytes.
//! - **Untrusted content**: retrieved text reaches the model only through [`context_block`], as
//!   quoted, cited data.

pub mod chunk;
mod context;
mod index;

pub use context::{Citation, MAX_CONTEXT_CHARS, context_block};
pub use index::{
    Counts, EmbedKind, Embedder, Hit, Index, KnowledgeError, MAX_RESULTS, Result, SourceFile,
    SyncReport, delete_all, fts_query, migrations,
};

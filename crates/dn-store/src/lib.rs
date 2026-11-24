//! dn-store — the local SQLite store (ADR-0007, T-CORE-02).
//!
//! - [`paths`]: the per-OS app-data directory and the default database path.
//! - [`migrate`]: a runner that composes every crate's `migrations()` (convention in
//!   `CLAUDE.md`) and applies the ones a database has not seen yet.
//! - [`chat`]: dn-store's own tables, conversations and messages, with UUIDv7 ids and
//!   `updated_at` so they can sync later (ADR-0008).
//!
//! Other crates never open the database themselves: the app opens it with [`open`] and
//! passes `&rusqlite::Connection` around.

pub mod chat;
pub mod migrate;
pub mod paths;

pub use migrate::{MigrationSet, run_migrations};
pub use rusqlite::Connection;

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("migration {set}#{version} failed: {source}")]
    Migration {
        set: &'static str,
        version: usize,
        source: rusqlite::Error,
    },
    #[error("database has migration {set}#{version}, but this build only knows {known}")]
    NewerSchema {
        set: String,
        version: i64,
        known: usize,
    },
    #[error("invalid stored value: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// dn-store's own migration set.
pub fn migrations() -> &'static [&'static str] {
    chat::MIGRATIONS
}

/// Opens (creating if needed) the database at `path`, sets pragmas and runs `sets`
/// plus dn-store's own migrations.
pub fn open(path: &std::path::Path, sets: &[MigrationSet]) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut conn = Connection::open(path)?;
    prepare(&mut conn, sets)?;
    Ok(conn)
}

/// Creates `dir` (and parents); on Unix the leaf is `0700` so other users cannot read
/// the history.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// In-memory database with every migration applied (tests, dev mode).
pub fn open_in_memory(sets: &[MigrationSet]) -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    prepare(&mut conn, sets)?;
    Ok(conn)
}

fn prepare(conn: &mut Connection, sets: &[MigrationSet]) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    // WAL is ignored by in-memory databases, which is fine.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let mut all = vec![MigrationSet::new("store", migrations())];
    all.extend_from_slice(sets);
    run_migrations(conn, &all)
}

/// Milliseconds since the Unix epoch (what `created_at` / `updated_at` columns hold).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

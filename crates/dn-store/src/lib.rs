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

/// Opens another connection to a database that [`open`] already created and migrated,
/// e.g. a dedicated connection for the dn-tools ledger. Same pragmas (WAL, foreign keys,
/// busy timeout); migrations are **not** run, so call [`open`] first. Fails if the file
/// does not exist rather than creating an empty database.
pub fn open_connection(path: &std::path::Path) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    apply_pragmas(&conn)?;
    Ok(conn)
}

/// How long a connection waits for another connection's write lock before `SQLITE_BUSY`.
pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    // WAL lets readers and one writer on separate connections proceed together.
    // In-memory databases ignore it, which is fine.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // Deleted content is overwritten with zeros instead of left in free pages
    // (T-STORE-11: a purged chat must not be recoverable from the file).
    conn.pragma_update(None, "secure_delete", true)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(())
}

/// Copies the WAL into the main file and truncates it, so content deleted by the last
/// transactions doesn't linger in old WAL frames. Best effort: another connection reading
/// at the same time can keep frames; the next checkpoint gets them.
pub fn checkpoint(conn: &Connection) {
    if let Err(e) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())) {
        eprintln!("database checkpoint failed: {e}");
    }
}

fn prepare(conn: &mut Connection, sets: &[MigrationSet]) -> Result<()> {
    apply_pragmas(conn)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp() -> TempDir {
        TempDir(std::env::temp_dir().join(format!("dn-store-conn-{}", dn_core::Id::new())))
    }

    #[test]
    fn extra_connection_shares_the_file_in_wal_mode() {
        const LEDGER: &[&str] = &["CREATE TABLE tool_ledger_entries (id TEXT PRIMARY KEY)"];
        let dir = temp();
        let path = dir.0.join("db.sqlite3");
        let main = open(&path, &[MigrationSet::new("tool_ledger", LEDGER)]).unwrap();
        let extra = open_connection(&path).unwrap();

        for c in [&main, &extra] {
            let mode: String = c
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            let fk: i64 = c
                .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                .unwrap();
            assert_eq!(fk, 1);
        }
        // Writes on one connection are visible on the other; migrations ran once.
        extra
            .execute("INSERT INTO tool_ledger_entries (id) VALUES ('a')", [])
            .unwrap();
        let n: i64 = main
            .query_row("SELECT COUNT(*) FROM tool_ledger_entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let runs: i64 = main
            .query_row("SELECT COUNT(*) FROM store_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs, crate::migrations().len() as i64 + 1);
        // A reader on one connection does not block a writer on the other.
        let tx = main.unchecked_transaction().unwrap();
        let _: i64 = tx
            .query_row("SELECT COUNT(*) FROM tool_ledger_entries", [], |r| r.get(0))
            .unwrap();
        extra
            .execute("INSERT INTO tool_ledger_entries (id) VALUES ('b')", [])
            .unwrap();
        drop(tx);
    }

    #[test]
    fn extra_connection_does_not_create_a_database() {
        let dir = temp();
        assert!(open_connection(&dir.0.join("missing.sqlite3")).is_err());
        assert!(!dir.0.join("missing.sqlite3").exists());
    }
}

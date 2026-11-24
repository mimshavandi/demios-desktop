//! Migration runner. Each crate that stores data exports
//! `pub fn migrations() -> &'static [&'static str]`; the app wraps each in a
//! [`MigrationSet`] and passes them to [`crate::open`].
//!
//! Rules:
//! - A migration is identified by `(set name, 1-based index)`. Append only: never edit
//!   or reorder a released migration.
//! - Each migration runs in its own transaction together with its bookkeeping row, so a
//!   failing migration leaves the database at the previous version.
//! - A database that has more migrations for a set than this build knows is refused
//!   (an older app must not write to a newer schema).

use crate::{Result, StoreError};
use rusqlite::{Connection, params};

/// One crate's ordered migrations.
#[derive(Clone, Copy, Debug)]
pub struct MigrationSet {
    /// Crate prefix, e.g. `artifact`, `tool_ledger`. Unique per database.
    pub name: &'static str,
    pub migrations: &'static [&'static str],
}

impl MigrationSet {
    pub const fn new(name: &'static str, migrations: &'static [&'static str]) -> Self {
        Self { name, migrations }
    }
}

const BOOKKEEPING: &str = "CREATE TABLE IF NOT EXISTS store_migrations (
    set_name   TEXT    NOT NULL,
    version    INTEGER NOT NULL,
    applied_at INTEGER NOT NULL,
    PRIMARY KEY (set_name, version)
)";

/// Applies every migration in `sets` that the database has not recorded yet.
pub fn run_migrations(conn: &mut Connection, sets: &[MigrationSet]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for set in sets {
        if !seen.insert(set.name) {
            return Err(StoreError::Invalid(format!(
                "migration set `{}` passed twice",
                set.name
            )));
        }
    }
    conn.execute(BOOKKEEPING, [])?;
    for set in sets {
        let applied: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM store_migrations WHERE set_name = ?1",
            [set.name],
            |r| r.get(0),
        )?;
        let known = set.migrations.len();
        if applied > known as i64 {
            return Err(StoreError::NewerSchema {
                set: set.name.to_string(),
                version: applied,
                known,
            });
        }
        for (i, sql) in set.migrations.iter().enumerate().skip(applied as usize) {
            let version = i + 1;
            let tx = conn.transaction()?;
            let run = tx.execute_batch(sql).and_then(|()| {
                tx.execute(
                    "INSERT INTO store_migrations (set_name, version, applied_at) VALUES (?1, ?2, ?3)",
                    params![set.name, version as i64, crate::now_ms()],
                )
            });
            run.map_err(|source| StoreError::Migration {
                set: set.name,
                version,
                source,
            })?;
            tx.commit()?;
        }
    }
    Ok(())
}

/// Highest applied migration for `set_name` (0 if none).
pub fn applied_version(conn: &Connection, set_name: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM store_migrations WHERE set_name = ?1",
        [set_name],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A1: &[&str] = &["CREATE TABLE a_items (id TEXT PRIMARY KEY)"];
    const A2: &[&str] = &[
        "CREATE TABLE a_items (id TEXT PRIMARY KEY)",
        "ALTER TABLE a_items ADD COLUMN name TEXT",
    ];
    const BROKEN: &[&str] = &[
        "CREATE TABLE b_items (id TEXT PRIMARY KEY)",
        "CREATE TABLE b_more (x); THIS IS NOT SQL",
    ];

    #[test]
    fn applies_incrementally_and_is_idempotent() {
        let mut c = Connection::open_in_memory().unwrap();
        run_migrations(&mut c, &[MigrationSet::new("a", A1)]).unwrap();
        assert_eq!(applied_version(&c, "a").unwrap(), 1);
        run_migrations(&mut c, &[MigrationSet::new("a", A2)]).unwrap();
        run_migrations(&mut c, &[MigrationSet::new("a", A2)]).unwrap();
        assert_eq!(applied_version(&c, "a").unwrap(), 2);
        c.execute("INSERT INTO a_items (id, name) VALUES ('1', 'x')", [])
            .unwrap();
    }

    #[test]
    fn failed_migration_rolls_back_to_previous_version() {
        let mut c = Connection::open_in_memory().unwrap();
        let err = run_migrations(&mut c, &[MigrationSet::new("b", BROKEN)]).unwrap_err();
        assert!(
            matches!(err, StoreError::Migration { version: 2, .. }),
            "{err}"
        );
        assert_eq!(applied_version(&c, "b").unwrap(), 1);
        let more: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'b_more'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(more, 0);
    }

    #[test]
    fn refuses_newer_schema() {
        let mut c = Connection::open_in_memory().unwrap();
        run_migrations(&mut c, &[MigrationSet::new("a", A2)]).unwrap();
        let err = run_migrations(&mut c, &[MigrationSet::new("a", A1)]).unwrap_err();
        assert!(matches!(err, StoreError::NewerSchema { .. }), "{err}");
    }

    #[test]
    fn rejects_duplicate_set_names() {
        let mut c = Connection::open_in_memory().unwrap();
        let sets = [MigrationSet::new("a", A1), MigrationSet::new("a", A2)];
        assert!(run_migrations(&mut c, &sets).is_err());
    }
}

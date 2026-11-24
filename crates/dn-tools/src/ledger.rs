//! Side-effect ledger (T-CON-03), ported semantics from the Demios `run_side_effects` ledger:
//! `claim → running → succeeded | failed | unknown`.
//!
//! - Replaying a **succeeded** key returns the cached output (no second side effect).
//! - A **running** key blocks (another attempt is in flight).
//! - A **failed** key may be claimed again (a new attempt): the side effect did not happen.
//! - An **unknown** key (interrupted, cancelled, dropped or timed out mid-call: the side
//!   effect may or may not have happened) refuses new claims until the user decides with
//!   [`resolve`].
//! - Reads are not recorded (unkeyed).
//!
//! Functions take a `&rusqlite::Connection` the app passes in; tables are prefixed
//! `tool_ledger_` and created by [`migrations`], which `dn-store` composes.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Schema migrations for this crate, applied in order by `dn-store`.
pub fn migrations() -> &'static [&'static str] {
    &[r#"
CREATE TABLE IF NOT EXISTS tool_ledger_runs (
    idempotency_key TEXT PRIMARY KEY NOT NULL,
    capability_id   TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    arguments_json  TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'unknown')),
    output_json     TEXT,
    error           TEXT,
    attempts        INTEGER NOT NULL DEFAULT 1,
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS tool_ledger_runs_conversation
    ON tool_ledger_runs (conversation_id, created_at_ms);
CREATE INDEX IF NOT EXISTS tool_ledger_runs_unknown
    ON tool_ledger_runs (status) WHERE status = 'unknown';
"#]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    /// Outcome not known; blocks re-runs until [`resolve`]d.
    Unknown,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
    fn parse(s: &str) -> Result<Self, LedgerError> {
        match s {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(LedgerError::Corrupt(format!("unknown status `{other}`"))),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ledger row is corrupt: {0}")]
    Corrupt(String),
    #[error("idempotency key `{0}` was reused with different arguments")]
    ArgumentsMismatch(String),
    #[error("idempotency key `{0}` is not running")]
    NotRunning(String),
    #[error("idempotency key `{0}` has no unknown outcome to resolve")]
    NotUnknown(String),
}

/// Outcome of [`claim`].
#[derive(Clone, Debug, PartialEq)]
pub enum Claim {
    /// This caller owns the attempt; it must call [`succeed`] or [`fail`].
    Acquired { attempt: u32 },
    /// Already succeeded; here is the cached output. Do not run again.
    Replay { output: serde_json::Value },
    /// Another attempt is running.
    InFlight,
    /// An earlier attempt has an unknown outcome; the user must [`resolve`] it first.
    OutcomeUnknown { error: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub idempotency_key: String,
    pub capability_id: String,
    pub conversation_id: String,
    pub arguments: serde_json::Value,
    pub status: RunStatus,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub attempts: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Look up a key without changing it.
pub fn get(conn: &Connection, key: &str) -> Result<Option<LedgerEntry>, LedgerError> {
    let row = conn
        .query_row(
            "SELECT idempotency_key, capability_id, conversation_id, arguments_json, status,
                    output_json, error, attempts, created_at_ms, updated_at_ms
             FROM tool_ledger_runs WHERE idempotency_key = ?1",
            params![key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, u32>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()?;
    row.map(|(k, cap, conv, args, status, out, err, attempts, c, u)| {
        Ok(LedgerEntry {
            idempotency_key: k,
            capability_id: cap,
            conversation_id: conv,
            arguments: serde_json::from_str(&args)?,
            status: RunStatus::parse(&status)?,
            output: out.map(|o| serde_json::from_str(&o)).transpose()?,
            error: err,
            attempts,
            created_at_ms: c,
            updated_at_ms: u,
        })
    })
    .transpose()
}

/// Atomically claim `key` for a new attempt (see module docs for the state machine).
pub fn claim(
    conn: &Connection,
    key: &str,
    capability_id: &str,
    conversation_id: &str,
    arguments: &serde_json::Value,
) -> Result<Claim, LedgerError> {
    // IMMEDIATE takes the write lock up front, so two connections can't both read "absent"
    // and race to insert.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let existing = get(&tx, key)?;
    let now = now_ms();
    let claim = match existing {
        None => {
            tx.execute(
                "INSERT INTO tool_ledger_runs
                   (idempotency_key, capability_id, conversation_id, arguments_json, status,
                    attempts, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, 'running', 1, ?5, ?5)",
                params![
                    key,
                    capability_id,
                    conversation_id,
                    serde_json::to_string(arguments)?,
                    now
                ],
            )?;
            Claim::Acquired { attempt: 1 }
        }
        Some(e) => {
            if &e.arguments != arguments {
                return Err(LedgerError::ArgumentsMismatch(key.to_owned()));
            }
            match e.status {
                RunStatus::Succeeded => Claim::Replay {
                    output: e.output.unwrap_or(serde_json::Value::Null),
                },
                RunStatus::Running => Claim::InFlight,
                RunStatus::Unknown => Claim::OutcomeUnknown { error: e.error },
                RunStatus::Failed => {
                    tx.execute(
                        "UPDATE tool_ledger_runs
                         SET status = 'running', error = NULL, attempts = attempts + 1,
                             updated_at_ms = ?2
                         WHERE idempotency_key = ?1",
                        params![key, now],
                    )?;
                    Claim::Acquired {
                        attempt: e.attempts + 1,
                    }
                }
            }
        }
    };
    tx.commit()?;
    Ok(claim)
}

fn finish(
    conn: &Connection,
    key: &str,
    status: RunStatus,
    output: Option<&serde_json::Value>,
    error: Option<&str>,
) -> Result<(), LedgerError> {
    let output = output.map(serde_json::to_string).transpose()?;
    let n = conn.execute(
        "UPDATE tool_ledger_runs SET status = ?2, output_json = ?3, error = ?4, updated_at_ms = ?5
         WHERE idempotency_key = ?1 AND status = 'running'",
        params![key, status.as_str(), output, error, now_ms()],
    )?;
    if n == 0 {
        return Err(LedgerError::NotRunning(key.to_owned()));
    }
    Ok(())
}

/// Record success and cache `output` for replay.
pub fn succeed(
    conn: &Connection,
    key: &str,
    output: &serde_json::Value,
) -> Result<(), LedgerError> {
    finish(conn, key, RunStatus::Succeeded, Some(output), None)
}

/// Record a failure where the side effect did **not** happen; the key may be claimed again.
pub fn fail(conn: &Connection, key: &str, error: &str) -> Result<(), LedgerError> {
    finish(conn, key, RunStatus::Failed, None, Some(error))
}

/// Record that the attempt ended without a known outcome (see module docs).
pub fn mark_unknown(conn: &Connection, key: &str, reason: &str) -> Result<(), LedgerError> {
    finish(conn, key, RunStatus::Unknown, None, Some(reason))
}

/// At startup, mark rows left `running` by a previous process as `unknown` (`interrupted`).
/// They block re-runs until the user resolves them. Returns how many rows changed.
pub fn mark_interrupted(conn: &Connection) -> Result<usize, LedgerError> {
    Ok(conn.execute(
        "UPDATE tool_ledger_runs SET status = 'unknown', error = 'interrupted', updated_at_ms = ?1
         WHERE status = 'running'",
        params![now_ms()],
    )?)
}

/// The user's answer for an `unknown` row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "resolution", rename_all = "snake_case")]
pub enum Resolution {
    /// It happened. Future replays of this key return `output` (no re-run).
    Happened { output: serde_json::Value },
    /// It did not happen; the key may run again (with a new approval as policy requires).
    DidNotHappen,
}

/// Resolve an `unknown` row (UI: "did this go through?").
pub fn resolve(conn: &Connection, key: &str, resolution: &Resolution) -> Result<(), LedgerError> {
    let (status, output, error) = match resolution {
        Resolution::Happened { output } => (
            RunStatus::Succeeded,
            Some(serde_json::to_string(output)?),
            None,
        ),
        Resolution::DidNotHappen => (RunStatus::Failed, None, Some("resolved: did not happen")),
    };
    let n = conn.execute(
        "UPDATE tool_ledger_runs SET status = ?2, output_json = ?3, error = ?4, updated_at_ms = ?5
         WHERE idempotency_key = ?1 AND status = 'unknown'",
        params![key, status.as_str(), output, error, now_ms()],
    )?;
    if n == 0 {
        return Err(LedgerError::NotUnknown(key.to_owned()));
    }
    Ok(())
}

/// Key of an `unknown` row for `capability_id` in `scope` (the `conversation_id` column), if
/// any. While one exists, new calls to that capability in that scope are refused: the model
/// would otherwise retry with a fresh call id (= fresh key) and repeat the side effect.
pub fn unknown_in_scope(
    conn: &Connection,
    capability_id: &str,
    scope: &str,
) -> Result<Option<String>, LedgerError> {
    Ok(conn
        .query_row(
            "SELECT idempotency_key FROM tool_ledger_runs
             WHERE status = 'unknown' AND capability_id = ?1 AND conversation_id = ?2
             ORDER BY created_at_ms LIMIT 1",
            params![capability_id, scope],
            |r| r.get(0),
        )
        .optional()?)
}

/// Rows waiting for the user to resolve, oldest first.
pub fn list_unknown(conn: &Connection) -> Result<Vec<LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT idempotency_key FROM tool_ledger_runs WHERE status = 'unknown'
         ORDER BY created_at_ms, idempotency_key",
    )?;
    let keys: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    keys.iter()
        .filter_map(|k| get(conn, k).transpose())
        .collect()
}

/// Entries for one conversation, oldest first (audit view).
pub fn list_for_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Vec<LedgerEntry>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT idempotency_key FROM tool_ledger_runs WHERE conversation_id = ?1
         ORDER BY created_at_ms, idempotency_key",
    )?;
    let keys: Vec<String> = stmt
        .query_map(params![conversation_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    keys.iter()
        .filter_map(|k| get(conn, k).transpose())
        .collect()
}

/// Deletes the finished (`succeeded` / `failed`) rows of `scope` (the `conversation_id`
/// column, e.g. `conv:<id>`), for when that conversation is purged (T-STORE-11). `running`
/// and `unknown` rows stay: an unknown write must be resolved by the user first, then
/// [`delete_finished`] removes it. Returns how many rows were deleted.
pub fn delete_scope(conn: &Connection, scope: &str) -> Result<usize, LedgerError> {
    Ok(conn.execute(
        "DELETE FROM tool_ledger_runs
         WHERE conversation_id = ?1 AND status IN ('succeeded', 'failed')",
        params![scope],
    )?)
}

/// Deletes one row if it is finished (`succeeded` / `failed`). Returns whether it did.
pub fn delete_finished(conn: &Connection, key: &str) -> Result<bool, LedgerError> {
    Ok(conn.execute(
        "DELETE FROM tool_ledger_runs
         WHERE idempotency_key = ?1 AND status IN ('succeeded', 'failed')",
        params![key],
    )? > 0)
}

/// Deletes every row ("Delete all local data"). Callers stop tool runs first.
pub fn delete_all(conn: &Connection) -> Result<usize, LedgerError> {
    Ok(conn.execute("DELETE FROM tool_ledger_runs", [])?)
}

#[cfg(test)]
pub(crate) fn test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    for m in migrations() {
        conn.execute_batch(m).unwrap();
    }
    conn
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn state_machine() {
        let db = test_db();
        let args = json!({"path": "a.txt"});
        assert_eq!(
            claim(&db, "k1", "mcp.fs.write", "c1", &args).unwrap(),
            Claim::Acquired { attempt: 1 }
        );
        assert_eq!(
            claim(&db, "k1", "mcp.fs.write", "c1", &args).unwrap(),
            Claim::InFlight
        );
        fail(&db, "k1", "boom").unwrap();
        assert_eq!(
            claim(&db, "k1", "mcp.fs.write", "c1", &args).unwrap(),
            Claim::Acquired { attempt: 2 }
        );
        succeed(&db, "k1", &json!({"ok": true})).unwrap();
        assert_eq!(
            claim(&db, "k1", "mcp.fs.write", "c1", &args).unwrap(),
            Claim::Replay {
                output: json!({"ok": true})
            }
        );
        assert!(succeed(&db, "k1", &json!(1)).is_err());
        assert!(matches!(
            claim(&db, "k1", "mcp.fs.write", "c1", &json!({"path": "b"})),
            Err(LedgerError::ArgumentsMismatch(_))
        ));
        let entries = list_for_conversation(&db, "c1").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attempts, 2);
    }

    #[test]
    fn interrupted_runs_are_unknown_until_resolved() {
        let db = test_db();
        claim(&db, "k", "cap", "c", &json!({})).unwrap();
        assert_eq!(mark_interrupted(&db).unwrap(), 1);
        let e = get(&db, "k").unwrap().unwrap();
        assert_eq!(e.status, RunStatus::Unknown);
        assert_eq!(e.error.as_deref(), Some("interrupted"));
        assert_eq!(list_unknown(&db).unwrap().len(), 1);
        assert!(matches!(
            claim(&db, "k", "cap", "c", &json!({})).unwrap(),
            Claim::OutcomeUnknown { .. }
        ));
        resolve(&db, "k", &Resolution::DidNotHappen).unwrap();
        assert_eq!(
            claim(&db, "k", "cap", "c", &json!({})).unwrap(),
            Claim::Acquired { attempt: 2 }
        );
        mark_unknown(&db, "k", "cancelled").unwrap();
        resolve(
            &db,
            "k",
            &Resolution::Happened {
                output: json!("done"),
            },
        )
        .unwrap();
        assert_eq!(
            claim(&db, "k", "cap", "c", &json!({})).unwrap(),
            Claim::Replay {
                output: json!("done")
            }
        );
        assert!(resolve(&db, "k", &Resolution::DidNotHappen).is_err());
    }

    #[test]
    fn delete_scope_keeps_unknown_until_resolved() {
        let db = test_db();
        let a = json!({});
        claim(&db, "done", "cap", "conv:1", &a).unwrap();
        succeed(&db, "done", &json!("ok")).unwrap();
        claim(&db, "bad", "cap", "conv:1", &a).unwrap();
        fail(&db, "bad", "boom").unwrap();
        claim(&db, "unk", "cap", "conv:1", &a).unwrap();
        mark_unknown(&db, "unk", "cancelled").unwrap();
        claim(&db, "other", "cap", "conv:2", &a).unwrap();
        succeed(&db, "other", &json!("ok")).unwrap();

        assert_eq!(delete_scope(&db, "conv:1").unwrap(), 2);
        assert!(get(&db, "done").unwrap().is_none());
        assert!(get(&db, "bad").unwrap().is_none());
        assert!(get(&db, "other").unwrap().is_some());
        assert!(!delete_finished(&db, "unk").unwrap(), "unknown rows stay");

        resolve(&db, "unk", &Resolution::DidNotHappen).unwrap();
        assert!(delete_finished(&db, "unk").unwrap());
        assert!(get(&db, "unk").unwrap().is_none());

        assert_eq!(delete_all(&db).unwrap(), 1);
        assert!(get(&db, "other").unwrap().is_none());
    }
}

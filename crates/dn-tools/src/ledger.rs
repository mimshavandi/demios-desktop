//! Side-effect ledger (T-CON-03), ported semantics from the Demios `run_side_effects` ledger:
//! `claim → running → succeeded | failed`.
//!
//! - Replaying a **succeeded** key returns the cached output (no second side effect).
//! - A **running** key blocks (another attempt is in flight, or the app died mid-call; see
//!   [`mark_interrupted`]).
//! - A **failed** key may be claimed again (a new attempt).
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
    status          TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed')),
    output_json     TEXT,
    error           TEXT,
    attempts        INTEGER NOT NULL DEFAULT 1,
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS tool_ledger_runs_conversation
    ON tool_ledger_runs (conversation_id, created_at_ms);
"#]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Result<Self, LedgerError> {
        match s {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
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
}

/// Outcome of [`claim`].
#[derive(Clone, Debug, PartialEq)]
pub enum Claim {
    /// This caller owns the attempt; it must call [`succeed`] or [`fail`].
    Acquired { attempt: u32 },
    /// Already succeeded; here is the cached output. Do not run again.
    Replay { output: serde_json::Value },
    /// Another attempt is running (or was interrupted and not yet resolved).
    InFlight,
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
    let tx = conn.unchecked_transaction()?;
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

/// Record failure; the key may be claimed again.
pub fn fail(conn: &Connection, key: &str, error: &str) -> Result<(), LedgerError> {
    finish(conn, key, RunStatus::Failed, None, Some(error))
}

/// At startup, mark rows left `running` by a previous process as failed with
/// `interrupted`. The side effect may or may not have happened; the UI should show these to the
/// user before re-running. Returns how many rows changed.
pub fn mark_interrupted(conn: &Connection) -> Result<usize, LedgerError> {
    Ok(conn.execute(
        "UPDATE tool_ledger_runs SET status = 'failed', error = 'interrupted', updated_at_ms = ?1
         WHERE status = 'running'",
        params![now_ms()],
    )?)
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
    fn interrupted_runs_become_failed() {
        let db = test_db();
        claim(&db, "k", "cap", "c", &json!({})).unwrap();
        assert_eq!(mark_interrupted(&db).unwrap(), 1);
        let e = get(&db, "k").unwrap().unwrap();
        assert_eq!(e.status, RunStatus::Failed);
        assert_eq!(e.error.as_deref(), Some("interrupted"));
    }
}

//! Per-job settings and state, run history, "Run it now?" questions, pause-all and quiet
//! hours, in `jobs_*` tables. Every call takes the host's connection. Times are Unix
//! seconds.

use crate::{JobKey, JobsError, Result, parse_id, parse_plugin};
use dn_core::Id;
use dn_plugin_api::MissedRunRule;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

pub(crate) const MIGRATION: &str = "
CREATE TABLE jobs_jobs (
    plugin_id          TEXT    NOT NULL,
    job_id             TEXT    NOT NULL,
    enabled            INTEGER NOT NULL DEFAULT 0,
    enabled_at         INTEGER,
    -- The user's missed-run rule as JSON; NULL means the add-on's proposal.
    missed_rule        TEXT,
    -- The latest scheduled time that was run, attempted or settled (skipped, asked).
    last_scheduled_for INTEGER,
    -- The scheduled time of the last successful run.
    last_success       INTEGER,
    PRIMARY KEY (plugin_id, job_id)
) WITHOUT ROWID;

CREATE TABLE jobs_runs (
    id            TEXT    PRIMARY KEY NOT NULL,
    plugin_id     TEXT    NOT NULL,
    job_id        TEXT    NOT NULL,
    scheduled_for INTEGER NOT NULL,
    started_at    INTEGER NOT NULL,
    finished_at   INTEGER,
    late          INTEGER NOT NULL,
    covers_runs   INTEGER NOT NULL,
    outcome       TEXT    NOT NULL CHECK (outcome IN
        ('running', 'succeeded', 'failed', 'cancelled', 'timed_out', 'interrupted', 'skipped')),
    not_fresh     INTEGER NOT NULL DEFAULT 0,
    error         TEXT
);
CREATE INDEX jobs_runs_by_job ON jobs_runs (plugin_id, job_id, started_at DESC);

CREATE TABLE jobs_questions (
    plugin_id     TEXT    NOT NULL,
    job_id        TEXT    NOT NULL,
    scheduled_for INTEGER NOT NULL,
    missed        INTEGER NOT NULL,
    asked_at      INTEGER NOT NULL,
    PRIMARY KEY (plugin_id, job_id)
) WITHOUT ROWID;

CREATE TABLE jobs_runtime (
    key   TEXT    PRIMARY KEY NOT NULL,
    value INTEGER NOT NULL
) WITHOUT ROWID;
";

/// A job's stored settings and state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobState {
    pub enabled: bool,
    pub enabled_at: Option<i64>,
    /// The user's rule, if they changed the add-on's proposal.
    pub missed_rule: Option<MissedRunRule>,
    pub last_scheduled_for: Option<i64>,
    pub last_success: Option<i64>,
}

impl JobState {
    /// The point after which scheduled times count (see [`crate::decide`]).
    pub fn since(&self) -> Option<i64> {
        let enabled_at = self.enabled_at?;
        Some(
            [Some(enabled_at), self.last_scheduled_for, self.last_success]
                .into_iter()
                .flatten()
                .max()
                .unwrap_or(enabled_at),
        )
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    /// The runtime stopped (crash, power loss) while it ran.
    Interrupted,
    Skipped,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
            Self::Skipped => "skipped",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "timed_out" => Self::TimedOut,
            "interrupted" => Self::Interrupted,
            "skipped" => Self::Skipped,
            other => return Err(JobsError::Damaged(format!("outcome `{other}`"))),
        })
    }
}

/// One entry of a job's history: "missed at 07:00, ran late at 10:02", "skipped (not
/// fresh)".
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunRecord {
    pub id: Id,
    pub job: JobKey,
    pub scheduled_for: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub late: bool,
    pub covers_runs: u64,
    pub outcome: Outcome,
    /// For a skip: the latest missed run wasn't fresh any more.
    pub not_fresh: bool,
    pub error: Option<String>,
}

/// A "Run it now?" question waiting for the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Question {
    pub job: JobKey,
    pub scheduled_for: i64,
    pub missed: u64,
    pub asked_at: i64,
}

fn key_params(key: &JobKey) -> (String, String) {
    (key.plugin.as_str().to_owned(), key.job.clone())
}

fn ensure_row(conn: &Connection, key: &JobKey) -> Result<()> {
    let (p, j) = key_params(key);
    conn.execute(
        "INSERT OR IGNORE INTO jobs_jobs (plugin_id, job_id) VALUES (?1, ?2)",
        params![p, j],
    )?;
    Ok(())
}

/// Turns a job on or off. Turning it on starts its schedule from `now`, so nothing from
/// before counts as missed.
pub fn set_enabled(conn: &Connection, key: &JobKey, enabled: bool, now: i64) -> Result<()> {
    ensure_row(conn, key)?;
    let (p, j) = key_params(key);
    conn.execute(
        "UPDATE jobs_jobs SET enabled = ?3,
                enabled_at = CASE WHEN ?3 THEN ?4 ELSE NULL END
         WHERE plugin_id = ?1 AND job_id = ?2",
        params![p, j, enabled, now],
    )?;
    if !enabled {
        conn.execute(
            "DELETE FROM jobs_questions WHERE plugin_id = ?1 AND job_id = ?2",
            params![p, j],
        )?;
    }
    Ok(())
}

/// Sets the user's missed-run rule, or `None` for the add-on's proposal.
pub fn set_missed_rule(conn: &Connection, key: &JobKey, rule: Option<MissedRunRule>) -> Result<()> {
    if let Some(r) = &rule {
        r.validate()
            .map_err(|e| JobsError::Damaged(e.to_string()))?;
    }
    ensure_row(conn, key)?;
    let (p, j) = key_params(key);
    let json = rule
        .map(|r| serde_json::to_string(&r))
        .transpose()
        .map_err(|e| JobsError::Damaged(e.to_string()))?;
    conn.execute(
        "UPDATE jobs_jobs SET missed_rule = ?3 WHERE plugin_id = ?1 AND job_id = ?2",
        params![p, j, json],
    )?;
    Ok(())
}

/// A job's state; a job never turned on is off.
pub fn job_state(conn: &Connection, key: &JobKey) -> Result<JobState> {
    let (p, j) = key_params(key);
    let row = conn
        .query_row(
            "SELECT enabled, enabled_at, missed_rule, last_scheduled_for, last_success
             FROM jobs_jobs WHERE plugin_id = ?1 AND job_id = ?2",
            params![p, j],
            |r| {
                Ok((
                    r.get::<_, bool>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((enabled, enabled_at, rule, last_scheduled_for, last_success)) = row else {
        return Ok(JobState {
            enabled: false,
            enabled_at: None,
            missed_rule: None,
            last_scheduled_for: None,
            last_success: None,
        });
    };
    let missed_rule = rule
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|e| JobsError::Damaged(format!("missed-run rule: {e}")))?;
    Ok(JobState {
        enabled,
        enabled_at,
        missed_rule,
        last_scheduled_for,
        last_success,
    })
}

fn settle(conn: &Connection, key: &JobKey, scheduled_for: i64) -> Result<()> {
    ensure_row(conn, key)?;
    let (p, j) = key_params(key);
    conn.execute(
        "UPDATE jobs_jobs SET last_scheduled_for = MAX(COALESCE(last_scheduled_for, ?3), ?3)
         WHERE plugin_id = ?1 AND job_id = ?2",
        params![p, j, scheduled_for],
    )?;
    Ok(())
}

/// Records that a run started. The scheduled time counts as attempted from now on.
pub fn start_run(
    conn: &Connection,
    key: &JobKey,
    scheduled_for: i64,
    late: bool,
    covers_runs: u64,
    now: i64,
) -> Result<Id> {
    let id = Id::new();
    let (p, j) = key_params(key);
    let tx = conn.unchecked_transaction()?;
    // Checked in the same transaction as the insert: a job turned off or removed since
    // the scheduler planned this run doesn't start.
    if !job_state(&tx, key)?.enabled {
        return Err(JobsError::JobOff);
    }
    settle(&tx, key, scheduled_for)?;
    tx.execute(
        "INSERT INTO jobs_runs (id, plugin_id, job_id, scheduled_for, started_at, late,
                               covers_runs, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running')",
        params![
            id.to_string(),
            p,
            j,
            scheduled_for,
            now,
            late,
            i64::try_from(covers_runs).unwrap_or(i64::MAX)
        ],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Records how a run ended. A success moves the job's "last success" forward.
pub fn finish_run(
    conn: &Connection,
    run: Id,
    outcome: Outcome,
    error: Option<&str>,
    now: i64,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE jobs_runs SET outcome = ?2, error = ?3, finished_at = ?4
         WHERE id = ?1 AND outcome = 'running'",
        params![run.to_string(), outcome.as_str(), error, now],
    )?;
    if changed == 0 {
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM jobs_runs WHERE id = ?1)",
            [run.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            // "Delete all" or removing the add-on deleted it while it ran.
            return Ok(());
        }
        return Err(JobsError::Damaged(format!("run {run} isn't running")));
    }
    if outcome == Outcome::Succeeded {
        tx.execute(
            "UPDATE jobs_jobs SET last_success = MAX(COALESCE(last_success, r.scheduled_for), r.scheduled_for)
             FROM (SELECT plugin_id, job_id, scheduled_for FROM jobs_runs WHERE id = ?1) AS r
             WHERE jobs_jobs.plugin_id = r.plugin_id AND jobs_jobs.job_id = r.job_id",
            params![run.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Records skipped missed runs ("skipped (not fresh)"), so they aren't decided again.
pub fn record_skip(
    conn: &Connection,
    key: &JobKey,
    scheduled_for: i64,
    missed: u64,
    not_fresh: bool,
    now: i64,
) -> Result<()> {
    let (p, j) = key_params(key);
    let tx = conn.unchecked_transaction()?;
    settle(&tx, key, scheduled_for)?;
    tx.execute(
        "INSERT INTO jobs_runs (id, plugin_id, job_id, scheduled_for, started_at, finished_at,
                               late, covers_runs, outcome, not_fresh)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1, ?6, 'skipped', ?7)",
        params![
            Id::new().to_string(),
            p,
            j,
            scheduled_for,
            now,
            i64::try_from(missed).unwrap_or(i64::MAX),
            not_fresh
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// Stores one "Run it now?" question per job (the newest wins).
pub fn ask(
    conn: &Connection,
    key: &JobKey,
    scheduled_for: i64,
    missed: u64,
    now: i64,
) -> Result<()> {
    let (p, j) = key_params(key);
    let tx = conn.unchecked_transaction()?;
    settle(&tx, key, scheduled_for)?;
    tx.execute(
        "INSERT INTO jobs_questions (plugin_id, job_id, scheduled_for, missed, asked_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (plugin_id, job_id) DO UPDATE SET
            scheduled_for = excluded.scheduled_for,
            missed = jobs_questions.missed + excluded.missed,
            asked_at = excluded.asked_at",
        params![
            p,
            j,
            scheduled_for,
            i64::try_from(missed).unwrap_or(i64::MAX),
            now
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// The questions waiting for the user.
pub fn questions(conn: &Connection) -> Result<Vec<Question>> {
    let mut stmt = conn.prepare(
        "SELECT plugin_id, job_id, scheduled_for, missed, asked_at FROM jobs_questions
         ORDER BY asked_at, plugin_id, job_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (p, j, scheduled_for, missed, asked_at) = row?;
        out.push(Question {
            job: JobKey::new(parse_plugin(&p)?, j),
            scheduled_for,
            missed: u64::try_from(missed).unwrap_or(0),
            asked_at,
        });
    }
    Ok(out)
}

/// Removes a job's question. `Some(question)` when there was one: the caller runs it
/// late on "Run now", or records the skip on "Skip".
pub fn take_question(conn: &Connection, key: &JobKey) -> Result<Question> {
    let (p, j) = key_params(key);
    let q = questions(conn)?
        .into_iter()
        .find(|q| &q.job == key)
        .ok_or(JobsError::NoQuestion)?;
    conn.execute(
        "DELETE FROM jobs_questions WHERE plugin_id = ?1 AND job_id = ?2",
        params![p, j],
    )?;
    Ok(q)
}

fn run_from_row(r: &Row<'_>) -> rusqlite::Result<(String, String, String, RunRow)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        RunRow {
            scheduled_for: r.get(3)?,
            started_at: r.get(4)?,
            finished_at: r.get(5)?,
            late: r.get(6)?,
            covers_runs: r.get(7)?,
            outcome: r.get(8)?,
            not_fresh: r.get(9)?,
            error: r.get(10)?,
        },
    ))
}

struct RunRow {
    scheduled_for: i64,
    started_at: i64,
    finished_at: Option<i64>,
    late: bool,
    covers_runs: i64,
    outcome: String,
    not_fresh: bool,
    error: Option<String>,
}

/// A job's latest runs, newest first.
pub fn history(conn: &Connection, key: &JobKey, limit: u32) -> Result<Vec<RunRecord>> {
    let (p, j) = key_params(key);
    let mut stmt = conn.prepare(
        "SELECT id, plugin_id, job_id, scheduled_for, started_at, finished_at, late,
                covers_runs, outcome, not_fresh, error
         FROM jobs_runs WHERE plugin_id = ?1 AND job_id = ?2
         ORDER BY started_at DESC, id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![p, j, limit], run_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, p, j, r) = row?;
        out.push(RunRecord {
            id: parse_id(&id)?,
            job: JobKey::new(parse_plugin(&p)?, j),
            scheduled_for: r.scheduled_for,
            started_at: r.started_at,
            finished_at: r.finished_at,
            late: r.late,
            covers_runs: u64::try_from(r.covers_runs).unwrap_or(0),
            outcome: Outcome::parse(&r.outcome)?,
            not_fresh: r.not_fresh,
            error: r.error,
        });
    }
    Ok(out)
}

/// At start: runs left `running` by a crash are marked `interrupted` (they aren't retried;
/// the next scheduled time runs as usual).
pub fn recover(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs_runs SET outcome = 'interrupted', finished_at = ?1 WHERE outcome = 'running'",
        params![now],
    )?)
}

const PAUSED: &str = "paused";
const QUIET_START: &str = "quiet_start_min";
const QUIET_END: &str = "quiet_end_min";

fn runtime_get(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT value FROM jobs_runtime WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}

fn runtime_set(conn: &Connection, key: &str, value: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO jobs_runtime (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// The pause-all switch: when on, no job, catch-up or question runs.
pub fn paused(conn: &Connection) -> Result<bool> {
    Ok(runtime_get(conn, PAUSED)?.unwrap_or(0) != 0)
}

pub fn set_paused(conn: &Connection, paused: bool) -> Result<()> {
    runtime_set(conn, PAUSED, i64::from(paused))
}

/// Quiet hours as minutes after local midnight, `(start, end)`; equal means none.
pub fn quiet_hours(conn: &Connection) -> Result<(u16, u16)> {
    let get = |k| -> Result<u16> {
        Ok(runtime_get(conn, k)?
            .and_then(|v| u16::try_from(v).ok())
            .filter(|v| *v < 1440)
            .unwrap_or(0))
    };
    Ok((get(QUIET_START)?, get(QUIET_END)?))
}

pub fn set_quiet_hours(conn: &Connection, start_min: u16, end_min: u16) -> Result<()> {
    if start_min >= 1440 || end_min >= 1440 {
        return Err(JobsError::Damaged(
            "quiet hours must be within a day".into(),
        ));
    }
    runtime_set(conn, QUIET_START, i64::from(start_min))?;
    runtime_set(conn, QUIET_END, i64::from(end_min))
}

/// Deletes one add-on's job settings, history and questions (its held approvals go with
/// [`crate::held::delete_plugin`]).
pub fn delete_plugin(conn: &Connection, plugin: &dn_plugin_api::PluginId) -> Result<()> {
    for table in ["jobs_jobs", "jobs_runs", "jobs_questions"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE plugin_id = ?1"),
            [plugin.as_str()],
        )?;
    }
    Ok(())
}

/// "Delete all local data": every job table, emptied.
pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM jobs_jobs; DELETE FROM jobs_runs; DELETE FROM jobs_questions;
         DELETE FROM jobs_runtime;",
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use dn_plugin_api::PluginId;
    use dn_store::MigrationSet;

    pub(crate) fn db() -> Connection {
        dn_store::open_in_memory(&[MigrationSet::new("jobs", crate::migrations())]).unwrap()
    }

    pub(crate) fn key(job: &str) -> JobKey {
        JobKey::new(PluginId::new("brief").unwrap(), job)
    }

    #[test]
    fn turning_a_job_on_starts_its_schedule_now() {
        let conn = db();
        let k = key("daily");
        assert!(!job_state(&conn, &k).unwrap().enabled);
        assert_eq!(job_state(&conn, &k).unwrap().since(), None);
        set_enabled(&conn, &k, true, 100).unwrap();
        assert_eq!(job_state(&conn, &k).unwrap().since(), Some(100));
        set_enabled(&conn, &k, false, 200).unwrap();
        assert_eq!(job_state(&conn, &k).unwrap().since(), None);
    }

    #[test]
    fn runs_settle_their_time_and_success_moves_forward() {
        let conn = db();
        let k = key("daily");
        set_enabled(&conn, &k, true, 100).unwrap();
        let run = start_run(&conn, &k, 500, false, 1, 501).unwrap();
        assert_eq!(job_state(&conn, &k).unwrap().since(), Some(500));
        finish_run(&conn, run, Outcome::Failed, Some("network down"), 510).unwrap();
        let s = job_state(&conn, &k).unwrap();
        assert_eq!(s.last_success, None);
        assert_eq!(s.since(), Some(500), "a failure isn't retried in a loop");
        let run = start_run(&conn, &k, 900, true, 2, 950).unwrap();
        finish_run(&conn, run, Outcome::Succeeded, None, 960).unwrap();
        assert_eq!(job_state(&conn, &k).unwrap().last_success, Some(900));
        assert!(finish_run(&conn, run, Outcome::Failed, None, 970).is_err());
        let h = history(&conn, &k, 10).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].outcome, Outcome::Succeeded);
        assert!(h[0].late);
        assert_eq!(h[1].error.as_deref(), Some("network down"));
    }

    #[test]
    fn a_job_that_is_off_or_gone_does_not_start() {
        let conn = db();
        let k = key("daily");
        assert!(matches!(
            start_run(&conn, &k, 5, false, 1, 6),
            Err(JobsError::JobOff)
        ));
        set_enabled(&conn, &k, true, 1).unwrap();
        set_enabled(&conn, &k, false, 2).unwrap();
        assert!(matches!(
            start_run(&conn, &k, 5, false, 1, 6),
            Err(JobsError::JobOff)
        ));
        assert!(history(&conn, &k, 5).unwrap().is_empty());
    }

    #[test]
    fn a_run_deleted_while_it_ran_finishes_quietly() {
        let conn = db();
        let k = key("daily");
        set_enabled(&conn, &k, true, 1).unwrap();
        let run = start_run(&conn, &k, 5, false, 1, 6).unwrap();
        delete_all(&conn).unwrap();
        finish_run(&conn, run, Outcome::Cancelled, None, 7).unwrap();
        assert!(history(&conn, &k, 5).unwrap().is_empty());
    }

    #[test]
    fn a_crash_leaves_runs_interrupted_not_running() {
        let conn = db();
        let k = key("daily");
        set_enabled(&conn, &k, true, 1).unwrap();
        start_run(&conn, &k, 500, false, 1, 501).unwrap();
        assert_eq!(recover(&conn, 600).unwrap(), 1);
        assert_eq!(
            history(&conn, &k, 1).unwrap()[0].outcome,
            Outcome::Interrupted
        );
    }

    #[test]
    fn one_question_per_job_and_it_is_answered_once() {
        let conn = db();
        let k = key("weekly");
        ask(&conn, &k, 500, 2, 600).unwrap();
        ask(&conn, &k, 900, 1, 950).unwrap();
        let qs = questions(&conn).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!((qs[0].scheduled_for, qs[0].missed), (900, 3));
        assert_eq!(take_question(&conn, &k).unwrap().missed, 3);
        assert!(matches!(
            take_question(&conn, &k),
            Err(JobsError::NoQuestion)
        ));
        assert_eq!(job_state(&conn, &k).unwrap().last_scheduled_for, Some(900));
    }

    #[test]
    fn rules_pause_and_quiet_hours_are_stored() {
        let conn = db();
        let k = key("daily");
        set_missed_rule(&conn, &k, Some(MissedRunRule::Ask)).unwrap();
        assert_eq!(
            job_state(&conn, &k).unwrap().missed_rule,
            Some(MissedRunRule::Ask)
        );
        assert!(
            set_missed_rule(
                &conn,
                &k,
                Some(MissedRunRule::CatchUpOnce { fresh_for_secs: 0 })
            )
            .is_err()
        );
        assert!(!paused(&conn).unwrap());
        set_paused(&conn, true).unwrap();
        assert!(paused(&conn).unwrap());
        set_quiet_hours(&conn, 22 * 60, 7 * 60).unwrap();
        assert_eq!(quiet_hours(&conn).unwrap(), (1320, 420));
        assert!(set_quiet_hours(&conn, 1440, 0).is_err());
    }

    #[test]
    fn deleting_an_add_on_or_everything_leaves_no_rows() {
        let conn = db();
        let a = key("daily");
        let b = JobKey::new(PluginId::new("notes").unwrap(), "sync");
        for k in [&a, &b] {
            set_enabled(&conn, k, true, 1).unwrap();
            start_run(&conn, k, 5, false, 1, 6).unwrap();
            ask(&conn, k, 7, 1, 8).unwrap();
        }
        delete_plugin(&conn, &a.plugin).unwrap();
        assert!(history(&conn, &a, 10).unwrap().is_empty());
        assert_eq!(history(&conn, &b, 10).unwrap().len(), 1);
        delete_all(&conn).unwrap();
        for table in ["jobs_jobs", "jobs_runs", "jobs_questions", "jobs_runtime"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table}");
        }
    }
}

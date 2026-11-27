//! The daily token budget for background jobs, in `jobs_usage` and `jobs_runtime`.
//!
//! - **Usage is counted per run**, and so per job and per local day. The host calls
//!   [`record_usage`] after each model call a job makes, with the tokens the model reported.
//! - **The check comes before a run starts** ([`check`]). A run starts only if today's usage
//!   plus what the job is expected to need fits in the budget. The expected need is the most
//!   that one of its recent runs used. A run that doesn't fit is recorded as skipped with
//!   the reason ([`over_budget_message`]) and never starts, so a job never runs partway
//!   because of the budget.
//! - **Once the budget is used up, every job waits for the next local day,** including jobs
//!   that use no model. The scheduler can't tell which jobs call a model, and an add-on's
//!   jobs stopping together is easier to understand than some of them stopping.
//! - A run that has already started is never cut off. It can go over the budget, and what it
//!   used counts, so the next run waits.
//! - **No budget set means no limit.** A host that requires a budget before a job can be
//!   turned on checks [`daily_budget`] itself.
//!
//! The day is the local calendar day of the scheduler's clock, so it follows the time zone
//! and daylight saving like the schedules do.

use crate::{JobKey, Result, parse_plugin};
use dn_core::Id;
use dn_plugin_api::LocalTime;
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const MIGRATION: &str = "
-- Tokens each run used, on the local day it used them.
CREATE TABLE jobs_usage (
    run_id    TEXT    NOT NULL,
    plugin_id TEXT    NOT NULL,
    job_id    TEXT    NOT NULL,
    day       INTEGER NOT NULL,
    tokens    INTEGER NOT NULL CHECK (tokens >= 0),
    PRIMARY KEY (run_id, day)
) WITHOUT ROWID;
CREATE INDEX jobs_usage_by_day ON jobs_usage (day);
CREATE INDEX jobs_usage_by_job ON jobs_usage (plugin_id, job_id);
";

const BUDGET: &str = "daily_token_budget";

/// How many of a job's latest runs set its expected need.
pub const RECENT_RUNS: u32 = 5;

/// The local calendar day `now` (Unix seconds) falls on, as days since 1970-01-01.
pub fn local_day(now: i64, zone: &dyn LocalTime) -> i64 {
    zone.to_local(now).div_euclid(86_400)
}

/// The daily budget in tokens, or `None` when none is set.
pub fn daily_budget(conn: &Connection) -> Result<Option<u64>> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT value FROM jobs_runtime WHERE key = ?1",
            [BUDGET],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|v| u64::try_from(v).ok()))
}

/// Sets the daily budget, or removes it with `None`.
pub fn set_daily_budget(conn: &Connection, tokens: Option<u64>) -> Result<()> {
    match tokens {
        Some(t) => {
            conn.execute(
                "INSERT INTO jobs_runtime (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![BUDGET, i64::try_from(t).unwrap_or(i64::MAX)],
            )?;
        }
        None => {
            conn.execute("DELETE FROM jobs_runtime WHERE key = ?1", [BUDGET])?;
        }
    }
    Ok(())
}

/// Adds `tokens` to what run `run` used on `day`. A run that isn't recorded (deleted by
/// "Delete all" or by removing its add-on while it ran) is ignored.
pub fn record_usage(conn: &Connection, run: Id, day: i64, tokens: u64) -> Result<()> {
    let tokens = i64::try_from(tokens).unwrap_or(i64::MAX);
    conn.execute(
        "INSERT INTO jobs_usage (run_id, plugin_id, job_id, day, tokens)
         SELECT id, plugin_id, job_id, ?2, ?3 FROM jobs_runs WHERE id = ?1
         ON CONFLICT (run_id, day) DO UPDATE SET tokens = MIN(tokens + excluded.tokens, 9223372036854775807)",
        params![run.to_string(), day, tokens],
    )?;
    Ok(())
}

/// Tokens all jobs used on `day`.
pub fn used(conn: &Connection, day: i64) -> Result<u64> {
    let v: i64 = conn.query_row(
        "SELECT COALESCE(SUM(tokens), 0) FROM jobs_usage WHERE day = ?1",
        [day],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(v).unwrap_or(0))
}

/// Tokens each job used on `day`, most first.
pub fn usage_by_job(conn: &Connection, day: i64) -> Result<Vec<(JobKey, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT plugin_id, job_id, SUM(tokens) AS t FROM jobs_usage WHERE day = ?1
         GROUP BY plugin_id, job_id ORDER BY t DESC, plugin_id, job_id",
    )?;
    let rows = stmt.query_map([day], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (p, j, t) = row?;
        out.push((
            JobKey::new(parse_plugin(&p)?, j),
            u64::try_from(t).unwrap_or(0),
        ));
    }
    Ok(out)
}

/// The most one of the job's latest [`RECENT_RUNS`] runs used (0 with no history).
pub fn expected(conn: &Connection, key: &JobKey) -> Result<u64> {
    let v: i64 = conn.query_row(
        "SELECT COALESCE(MAX(t), 0) FROM (
             SELECT SUM(u.tokens) AS t FROM jobs_runs r JOIN jobs_usage u ON u.run_id = r.id
             WHERE r.plugin_id = ?1 AND r.job_id = ?2
             GROUP BY r.id ORDER BY MAX(r.started_at) DESC LIMIT ?3)",
        params![key.plugin.as_str(), key.job, RECENT_RUNS],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(v).unwrap_or(0))
}

/// Whether a run of `key` may start on `day`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// No budget, or the run fits.
    Fits,
    /// The run doesn't fit: `used` of `budget` tokens are gone today, and the job is expected
    /// to need `expected`.
    Over {
        used: u64,
        budget: u64,
        expected: u64,
    },
}

/// The check before a run starts.
pub fn check(conn: &Connection, key: &JobKey, day: i64) -> Result<Check> {
    let Some(budget) = daily_budget(conn)? else {
        return Ok(Check::Fits);
    };
    let used = used(conn, day)?;
    let expected = expected(conn, key)?;
    if used >= budget || used.saturating_add(expected) > budget {
        return Ok(Check::Over {
            used,
            budget,
            expected,
        });
    }
    Ok(Check::Fits)
}

/// The reason a skipped run shows, in the user's words.
pub fn over_budget_message(used: u64, budget: u64, expected: u64) -> String {
    if used >= budget {
        format!(
            "The daily token budget is used up ({used} of {budget} tokens). It runs again tomorrow, or raise the budget."
        )
    } else {
        format!(
            "Not started: it usually needs about {expected} tokens, and {} of today's {budget} are left. It runs again tomorrow, or raise the budget.",
            budget - used
        )
    }
}

/// Deletes one add-on's usage.
pub(crate) fn delete_plugin(conn: &Connection, plugin: &dn_plugin_api::PluginId) -> Result<()> {
    conn.execute(
        "DELETE FROM jobs_usage WHERE plugin_id = ?1",
        [plugin.as_str()],
    )?;
    Ok(())
}

/// Deletes the usage of runs older than `before_day`, which the budget no longer needs
/// beyond the expected need of recent runs. Returns how many rows went.
pub fn forget_before(conn: &Connection, before_day: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM jobs_usage WHERE day < ?1", [before_day])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{self, tests::db, tests::key};
    use dn_plugin_api::FixedOffset;

    #[test]
    fn the_day_is_local() {
        // 2026-11-23 23:30 UTC is already the 24th at UTC+1.
        let t = 1_795_392_000 + 23 * 3600 + 1800;
        assert_eq!(local_day(t, &FixedOffset(0)), 20_780);
        assert_eq!(local_day(t, &FixedOffset(3600)), 20_781);
    }

    #[test]
    fn no_budget_means_no_limit() {
        let conn = db();
        let k = key("daily");
        store::set_enabled(&conn, &k, true, 1).unwrap();
        let run = store::start_run(&conn, &k, 5, false, 1, 6).unwrap();
        record_usage(&conn, run, 7, 1_000_000).unwrap();
        assert_eq!(check(&conn, &k, 7).unwrap(), Check::Fits);
        set_daily_budget(&conn, Some(10)).unwrap();
        assert_eq!(daily_budget(&conn).unwrap(), Some(10));
        set_daily_budget(&conn, None).unwrap();
        assert_eq!(daily_budget(&conn).unwrap(), None);
    }

    #[test]
    fn usage_counts_per_job_and_day() {
        let conn = db();
        let (a, b) = (key("brief"), key("triage"));
        for k in [&a, &b] {
            store::set_enabled(&conn, k, true, 1).unwrap();
        }
        let ra = store::start_run(&conn, &a, 5, false, 1, 6).unwrap();
        record_usage(&conn, ra, 7, 300).unwrap();
        record_usage(&conn, ra, 7, 200).unwrap();
        // The same run went on past midnight.
        record_usage(&conn, ra, 8, 50).unwrap();
        let rb = store::start_run(&conn, &b, 5, false, 1, 6).unwrap();
        record_usage(&conn, rb, 7, 100).unwrap();
        assert_eq!(used(&conn, 7).unwrap(), 600);
        assert_eq!(used(&conn, 8).unwrap(), 50);
        assert_eq!(
            usage_by_job(&conn, 7).unwrap(),
            [(a.clone(), 500), (b.clone(), 100)]
        );
        assert_eq!(expected(&conn, &a).unwrap(), 550);
        // A run that "Delete all" removed isn't counted.
        record_usage(&conn, Id::new(), 7, 999).unwrap();
        assert_eq!(used(&conn, 7).unwrap(), 600);
        assert_eq!(forget_before(&conn, 8).unwrap(), 2);
        assert_eq!(used(&conn, 7).unwrap(), 0);
    }

    #[test]
    fn a_run_that_would_not_fit_does_not_start() {
        let conn = db();
        let k = key("brief");
        store::set_enabled(&conn, &k, true, 1).unwrap();
        set_daily_budget(&conn, Some(1_000)).unwrap();
        assert_eq!(check(&conn, &k, 7).unwrap(), Check::Fits);
        let run = store::start_run(&conn, &k, 5, false, 1, 6).unwrap();
        record_usage(&conn, run, 7, 600).unwrap();
        // 400 left, the job needs about 600.
        assert_eq!(
            check(&conn, &k, 7).unwrap(),
            Check::Over {
                used: 600,
                budget: 1_000,
                expected: 600
            }
        );
        // Tomorrow it fits again.
        assert_eq!(check(&conn, &k, 8).unwrap(), Check::Fits);
        assert!(over_budget_message(600, 1_000, 600).contains("400 of today's 1000"));
        assert!(over_budget_message(1_200, 1_000, 0).contains("used up (1200 of 1000"));
    }
}

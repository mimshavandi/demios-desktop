//! When a job runs. This is a fixed rule, not a judgement: the same schedule, history and
//! clock always give the same answer (ADR-0013 §2).
//!
//! - A job's `since` is the latest of: when the user turned it on, its last scheduled
//!   time that was run or attempted, and the last scheduled time whose missed runs were
//!   settled (skipped, asked about). So a failed run is not retried in a loop; its status
//!   shows the error, and the next scheduled time runs as usual.
//! - Exactly one scheduled time came due within [`ON_TIME_GRACE_SECS`]: **on time**.
//! - Otherwise (several came due, or it's later than the grace, for example after the
//!   computer slept or quiet hours ended): `dn_plugin_api::missed_runs` decides, and the
//!   answer is at most one run.

use dn_plugin_api::{MissedRunRule, MissedRuns, Schedule, missed_runs};

/// How late a run may start and still count as on time, in seconds.
pub const ON_TIME_GRACE_SECS: i64 = 120;

/// What to do for one job now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing is due.
    Wait,
    /// Run now for `scheduled_for`.
    Run {
        scheduled_for: i64,
        late: bool,
        covers_runs: u64,
    },
    /// Ask the user "Run it now?" for the missed runs.
    Ask { scheduled_for: i64, missed: u64 },
    /// Record that the missed runs were skipped.
    Skip {
        scheduled_for: i64,
        missed: u64,
        not_fresh: bool,
    },
}

/// The decision for one enabled job at `now` (Unix seconds).
pub fn decide(
    schedule: &Schedule,
    rule: &MissedRunRule,
    since: i64,
    now: i64,
    utc_offset_secs: i64,
) -> Decision {
    let Some(due) = schedule.due_between(since, now, utc_offset_secs) else {
        return Decision::Wait;
    };
    if due.count == 1 && now - due.latest <= ON_TIME_GRACE_SECS {
        return Decision::Run {
            scheduled_for: due.latest,
            late: false,
            covers_runs: 1,
        };
    }
    match missed_runs(schedule, rule, since, now, utc_offset_secs) {
        MissedRuns::None => Decision::Wait,
        MissedRuns::RunLate {
            scheduled_for,
            missed,
        } => Decision::Run {
            scheduled_for,
            late: true,
            covers_runs: missed,
        },
        MissedRuns::Ask {
            scheduled_for,
            missed,
        } => Decision::Ask {
            scheduled_for,
            missed,
        },
        MissedRuns::Skip {
            scheduled_for,
            missed,
            not_fresh,
        } => Decision::Skip {
            scheduled_for,
            missed,
            not_fresh,
        },
    }
}

/// Whether local time `now` (Unix seconds, with the local offset) is inside quiet hours,
/// given as minutes after local midnight. A window may wrap past midnight (22:00–07:00).
/// Equal start and end means no quiet hours.
pub fn in_quiet_hours(now: i64, utc_offset_secs: i64, start_min: u16, end_min: u16) -> bool {
    if start_min == end_min {
        return false;
    }
    let minute = ((now + utc_offset_secs).rem_euclid(86_400) / 60) as u16;
    if start_min < end_min {
        (start_min..end_min).contains(&minute)
    } else {
        minute >= start_min || minute < end_min
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    // 2026-11-23 00:00 UTC.
    const MON: i64 = 1_795_392_000;
    const SEVEN: Schedule = Schedule::DailyAt { hour: 7, minute: 0 };
    const FRESH_6H: MissedRunRule = MissedRunRule::CatchUpOnce {
        fresh_for_secs: 6 * 3600,
    };

    #[test]
    fn a_run_within_the_grace_is_on_time() {
        assert_eq!(
            decide(&SEVEN, &FRESH_6H, MON, MON + 7 * H + 30, 0),
            Decision::Run {
                scheduled_for: MON + 7 * H,
                late: false,
                covers_runs: 1
            }
        );
        assert_eq!(
            decide(&SEVEN, &FRESH_6H, MON, MON + 7 * H - 1, 0),
            Decision::Wait
        );
    }

    #[test]
    fn a_late_start_follows_the_missed_run_rule() {
        // The app opened at 10:00: catch up once, marked late.
        assert_eq!(
            decide(&SEVEN, &FRESH_6H, MON, MON + 10 * H, 0),
            Decision::Run {
                scheduled_for: MON + 7 * H,
                late: true,
                covers_runs: 1
            }
        );
        // Not fresh any more at 14:00 the next day... but 07:00 that day is fresh.
        assert_eq!(
            decide(&SEVEN, &FRESH_6H, MON, MON + 24 * H + 14 * H, 0),
            Decision::Skip {
                scheduled_for: MON + 31 * H,
                missed: 2,
                not_fresh: true
            }
        );
        // Three days away: still at most one run.
        let Decision::Run { covers_runs, .. } =
            decide(&SEVEN, &FRESH_6H, MON, MON + 72 * H + 8 * H, 0)
        else {
            panic!("expected one late run");
        };
        assert_eq!(covers_runs, 4);
        assert_eq!(
            decide(&SEVEN, &MissedRunRule::Ask, MON, MON + 10 * H, 0),
            Decision::Ask {
                scheduled_for: MON + 7 * H,
                missed: 1
            }
        );
        assert_eq!(
            decide(&SEVEN, &MissedRunRule::Skip, MON, MON + 10 * H, 0),
            Decision::Skip {
                scheduled_for: MON + 7 * H,
                missed: 1,
                not_fresh: false
            }
        );
    }

    #[test]
    fn the_same_inputs_give_the_same_answer() {
        for now in [MON, MON + 7 * H, MON + 9 * H, MON + 90 * H] {
            assert_eq!(
                decide(&SEVEN, &FRESH_6H, MON, now, 3600),
                decide(&SEVEN, &FRESH_6H, MON, now, 3600)
            );
        }
    }

    #[test]
    fn quiet_hours_wrap_past_midnight() {
        let at = |h: i64, m: i64| MON + h * H + m * 60;
        assert!(in_quiet_hours(at(23, 0), 0, 22 * 60, 7 * 60));
        assert!(in_quiet_hours(at(6, 59), 0, 22 * 60, 7 * 60));
        assert!(!in_quiet_hours(at(7, 0), 0, 22 * 60, 7 * 60));
        assert!(in_quiet_hours(at(13, 0), 0, 12 * 60, 14 * 60));
        assert!(!in_quiet_hours(at(13, 0), 0, 600, 600));
        // 06:30 UTC is 07:30 at UTC+1, after the window.
        assert!(!in_quiet_hours(at(6, 30), 3600, 22 * 60, 7 * 60));
    }
}

//! Background-job schedules and the missed-run rules (ADR-0013 §2).
//!
//! Times are Unix seconds (UTC). A time of day is local: the caller passes its time zone
//! as a [`LocalTime`], so a day with a daylight-saving change (23 or 25 hours) still has
//! one run at its time. The decision for missed runs is a fixed rule
//! ([`missed_runs`]): the same schedule, last run and clock always give the same answer.

use crate::error::PluginError;
use serde::{Deserialize, Serialize};

const DAY: i64 = 24 * 60 * 60;
/// The shortest interval a job can run at, in seconds.
pub const MIN_INTERVAL_SECS: u64 = 60;

/// A time zone, as the schedule needs it: from an instant to the local wall-clock time and
/// back. Local times are written as seconds, as if the local date and time were UTC.
pub trait LocalTime {
    /// The local wall-clock time at the instant `utc`.
    fn to_local(&self, utc: i64) -> i64;
    /// The instant a local wall-clock time names. A time that doesn't exist (skipped when
    /// clocks go forward) is moved forward by the gap, so 02:30 becomes 03:30; a time that
    /// happens twice (when clocks go back) is the first of the two.
    fn to_utc(&self, local: i64) -> i64;
}

/// A fixed offset from UTC, in seconds (no daylight saving).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedOffset(pub i64);

impl LocalTime for FixedOffset {
    fn to_local(&self, utc: i64) -> i64 {
        utc + self.0
    }
    fn to_utc(&self, local: i64) -> i64 {
        local - self.0
    }
}

/// When a job runs.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Every `secs` seconds (at least [`MIN_INTERVAL_SECS`]), counted from the last run.
    Every {
        /// The interval in seconds.
        secs: u64,
    },
    /// Every day at a local time.
    DailyAt {
        /// Hour, 0–23.
        hour: u8,
        /// Minute, 0–59.
        minute: u8,
    },
}

impl Schedule {
    /// Checks the interval or the time of day.
    pub fn validate(&self) -> Result<(), PluginError> {
        match *self {
            Self::Every { secs } if secs < MIN_INTERVAL_SECS => Err(PluginError::invalid(format!(
                "A job can run at most once a minute; {secs} seconds is too often"
            ))),
            Self::DailyAt { hour, minute } if hour > 23 || minute > 59 => Err(
                PluginError::invalid(format!("{hour:02}:{minute:02} isn't a time of day")),
            ),
            _ => Ok(()),
        }
    }

    /// The scheduled times after `since` and at or before `now`: how many there are, and
    /// the latest. `zone` is the local time for [`Schedule::DailyAt`]; daily times are
    /// counted in local days.
    pub fn due_between(&self, since: i64, now: i64, zone: &dyn LocalTime) -> Option<Due> {
        if now <= since {
            return None;
        }
        match *self {
            Self::Every { secs } => {
                let step = i64::try_from(secs.max(MIN_INTERVAL_SECS)).ok()?;
                let count = (now - since) / step;
                (count > 0).then(|| Due {
                    count: u64::try_from(count).unwrap_or(u64::MAX),
                    latest: since + count * step,
                })
            }
            Self::DailyAt { hour, minute } => {
                let tod = i64::from(hour) * 3600 + i64::from(minute) * 60;
                // The run on local day `d` (days since 1970-01-01, local).
                let at = |d: i64| zone.to_utc(d * DAY + tod);
                let mut last = zone.to_local(now).div_euclid(DAY);
                if at(last) > now {
                    last -= 1;
                }
                let latest = at(last);
                if latest <= since {
                    return None;
                }
                let mut first = zone.to_local(since).div_euclid(DAY);
                if at(first) <= since {
                    first += 1;
                }
                Some(Due {
                    count: u64::try_from(last - first + 1).unwrap_or(u64::MAX),
                    latest,
                })
            }
        }
    }
}

/// Scheduled times that came due in a period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Due {
    /// How many.
    pub count: u64,
    /// The latest one, Unix seconds.
    pub latest: i64,
}

/// What happens to runs that were missed because no runtime was up. The add-on proposes it
/// for each job and the user can change it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum MissedRunRule {
    /// Run once, marked late, if the latest missed time is at most `fresh_for_secs` ago;
    /// otherwise skip it. The default.
    CatchUpOnce {
        /// How long a missed run stays worth doing, in seconds.
        fresh_for_secs: u64,
    },
    /// Ask the user once per job: "Run it now?" with Run now and Skip.
    Ask,
    /// Drop missed runs.
    Skip,
}

impl MissedRunRule {
    /// Checks `fresh_for_secs` isn't zero.
    pub fn validate(&self) -> Result<(), PluginError> {
        match self {
            Self::CatchUpOnce { fresh_for_secs: 0 } => Err(PluginError::invalid(
                "A catch-up window of zero would never catch up; use `skip` instead",
            )),
            _ => Ok(()),
        }
    }
}

/// The decision for a job's missed runs, when a runtime starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissedRuns {
    /// Nothing was missed.
    None,
    /// Run once now, marked late, as the run for `scheduled_for`. `missed` runs were missed
    /// in all; this one run covers them, and its context says so.
    RunLate {
        /// The latest missed time, Unix seconds.
        scheduled_for: i64,
        /// How many runs were missed.
        missed: u64,
    },
    /// Ask the user whether to run it now.
    Ask {
        /// The latest missed time, Unix seconds.
        scheduled_for: i64,
        /// How many runs were missed.
        missed: u64,
    },
    /// Don't run it. Its status says how many were skipped and why.
    Skip {
        /// The latest missed time, Unix seconds.
        scheduled_for: i64,
        /// How many runs were skipped.
        missed: u64,
        /// True when they were skipped because the latest one wasn't fresh any more under
        /// `catch_up_once`, false when the rule is `skip`.
        not_fresh: bool,
    },
}

/// What to do about a job's missed runs.
///
/// `since` is the later of the job's last successful run and the moment the user turned it
/// on. Whatever the number missed, the answer is at most one run, so a long absence never
/// starts a burst.
pub fn missed_runs(
    schedule: &Schedule,
    rule: &MissedRunRule,
    since: i64,
    now: i64,
    zone: &dyn LocalTime,
) -> MissedRuns {
    let Some(due) = schedule.due_between(since, now, zone) else {
        return MissedRuns::None;
    };
    let (scheduled_for, missed) = (due.latest, due.count);
    match *rule {
        MissedRunRule::CatchUpOnce { fresh_for_secs } => {
            let fresh = i64::try_from(fresh_for_secs).unwrap_or(i64::MAX);
            if now - due.latest <= fresh {
                MissedRuns::RunLate {
                    scheduled_for,
                    missed,
                }
            } else {
                MissedRuns::Skip {
                    scheduled_for,
                    missed,
                    not_fresh: true,
                }
            }
        }
        MissedRunRule::Ask => MissedRuns::Ask {
            scheduled_for,
            missed,
        },
        MissedRunRule::Skip => MissedRuns::Skip {
            scheduled_for,
            missed,
            not_fresh: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    // 2026-11-23 00:00 UTC, a Monday.
    const MON: i64 = 1_795_392_000;
    const SEVEN: Schedule = Schedule::DailyAt { hour: 7, minute: 0 };
    const SIX_HOURS: MissedRunRule = MissedRunRule::CatchUpOnce {
        fresh_for_secs: 6 * 3600,
    };

    #[test]
    fn schedules_are_checked() {
        assert!(Schedule::Every { secs: 59 }.validate().is_err());
        assert!(Schedule::Every { secs: 60 }.validate().is_ok());
        assert!(
            Schedule::DailyAt {
                hour: 24,
                minute: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            Schedule::DailyAt {
                hour: 7,
                minute: 60
            }
            .validate()
            .is_err()
        );
        assert!(SEVEN.validate().is_ok());
        assert!(
            MissedRunRule::CatchUpOnce { fresh_for_secs: 0 }
                .validate()
                .is_err()
        );
    }

    #[test]
    fn a_brief_missed_this_morning_runs_late_once() {
        // Last ran Monday 07:00; the app opens Tuesday 10:00.
        let d = missed_runs(
            &SEVEN,
            &SIX_HOURS,
            MON + 7 * H,
            MON + 34 * H,
            &FixedOffset(0),
        );
        assert_eq!(
            d,
            MissedRuns::RunLate {
                scheduled_for: MON + 31 * H,
                missed: 1
            }
        );
    }

    #[test]
    fn a_stale_run_is_skipped_not_run() {
        // Missed Tuesday 07:00; the app opens Tuesday 15:00, more than 6 hours later.
        let d = missed_runs(
            &SEVEN,
            &SIX_HOURS,
            MON + 7 * H,
            MON + 39 * H,
            &FixedOffset(0),
        );
        assert_eq!(
            d,
            MissedRuns::Skip {
                scheduled_for: MON + 31 * H,
                missed: 1,
                not_fresh: true
            }
        );
    }

    #[test]
    fn many_missed_days_give_one_run_for_the_latest() {
        // Last ran Monday 07:00; the app opens Thursday 07:05: Tue, Wed, Thu were missed.
        let d = missed_runs(
            &SEVEN,
            &SIX_HOURS,
            MON + 7 * H,
            MON + 79 * H + 300,
            &FixedOffset(0),
        );
        assert_eq!(
            d,
            MissedRuns::RunLate {
                scheduled_for: MON + 79 * H,
                missed: 3
            }
        );
    }

    #[test]
    fn nothing_is_missed_before_the_next_time() {
        assert_eq!(
            missed_runs(
                &SEVEN,
                &SIX_HOURS,
                MON + 7 * H,
                MON + 30 * H,
                &FixedOffset(0)
            ),
            MissedRuns::None
        );
        // A late run at 10:02 counts for that morning's 07:00.
        assert_eq!(
            missed_runs(
                &SEVEN,
                &SIX_HOURS,
                MON + 10 * H + 120,
                MON + 20 * H,
                &FixedOffset(0)
            ),
            MissedRuns::None
        );
    }

    #[test]
    fn the_time_of_day_is_local() {
        // UTC+2: 07:00 local is 05:00 UTC. Last ran Monday 05:00 UTC, now Tuesday 06:00 UTC.
        let d = missed_runs(
            &SEVEN,
            &SIX_HOURS,
            MON + 5 * H,
            MON + 30 * H,
            &FixedOffset(2 * H),
        );
        assert_eq!(
            d,
            MissedRuns::RunLate {
                scheduled_for: MON + 29 * H,
                missed: 1
            }
        );
    }

    #[test]
    fn ask_and_skip_report_what_was_missed() {
        let every_hour = Schedule::Every { secs: 3600 };
        assert_eq!(
            missed_runs(
                &every_hour,
                &MissedRunRule::Ask,
                MON,
                MON + 5 * H + 10,
                &FixedOffset(0)
            ),
            MissedRuns::Ask {
                scheduled_for: MON + 5 * H,
                missed: 5
            }
        );
        assert_eq!(
            missed_runs(
                &every_hour,
                &MissedRunRule::Skip,
                MON,
                MON + 2 * H,
                &FixedOffset(0)
            ),
            MissedRuns::Skip {
                scheduled_for: MON + 2 * H,
                missed: 2,
                not_fresh: false
            }
        );
    }

    #[test]
    fn the_decision_is_the_same_every_time() {
        let a = missed_runs(&SEVEN, &SIX_HOURS, MON, MON + 100 * H, &FixedOffset(3600));
        let b = missed_runs(&SEVEN, &SIX_HOURS, MON, MON + 100 * H, &FixedOffset(3600));
        assert_eq!(a, b);
    }
}

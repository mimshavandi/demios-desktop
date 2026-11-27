//! Time zones for jobs at a time of day (#257, #299), shared by the Node and the app.
//!
//! - [`Zone`] turns instants into local wall-clock time and back with the zone's
//!   daylight-saving rules from the time zone database: the system's
//!   (/usr/share/zoneinfo), or the one built in on Windows.
//! - A wall-clock time skipped in spring is moved forward by the gap; one repeated in
//!   autumn is the first of the two (see [`dn_plugin_api::LocalTime`]).

use dn_plugin_api::LocalTime;
use jiff::Timestamp;
use jiff::tz::TimeZone;

/// A time zone with its daylight-saving rules: a named IANA zone, or the system's.
#[derive(Clone, Debug)]
pub struct Zone {
    tz: TimeZone,
}

impl Zone {
    /// The zone `name`, or an error naming it when the time zone database doesn't know it.
    pub fn named(name: &str) -> Result<Self, String> {
        // `TimeZone::get` also takes some POSIX strings and file-like names; only IANA
        // names are accepted, so node.json says what the Node runs on.
        if name.is_empty()
            || name.starts_with('/')
            || name.contains("..")
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
        {
            return Err(unknown(name));
        }
        TimeZone::get(name)
            .map(|tz| Self { tz })
            .map_err(|_| unknown(name))
    }

    /// The system's time zone, as the system sets it now (`TZ`, `/etc/localtime`, or the
    /// Windows setting). jiff reads it again at most every 5 minutes, so a change of zone
    /// is picked up within that time. An error when it can't be read; never UTC instead.
    pub fn system() -> Result<Self, String> {
        TimeZone::try_system()
            .map(|tz| Self { tz })
            .map_err(|e| format!("couldn't read this computer's time zone: {e}"))
    }

    /// The IANA name, when the zone has one.
    pub fn name(&self) -> Option<&str> {
        self.tz.iana_name()
    }
}

fn unknown(name: &str) -> String {
    format!("`{name}` isn't a time zone this system knows; use an IANA name like Europe/Berlin")
}

/// Keeps an instant inside the range the time zone database handles (years -9999 to 9999).
fn clamp(secs: i64) -> Timestamp {
    let s = secs.clamp(Timestamp::MIN.as_second(), Timestamp::MAX.as_second());
    Timestamp::from_second(s).unwrap_or(Timestamp::UNIX_EPOCH)
}

impl LocalTime for Zone {
    fn to_local(&self, utc: i64) -> i64 {
        utc + i64::from(self.tz.to_offset(clamp(utc)).seconds())
    }

    fn to_utc(&self, local: i64) -> i64 {
        let wall = TimeZone::UTC.to_datetime(clamp(local));
        // `compatible`: a skipped time moves forward by the gap, a repeated time is the
        // first of the two (the same rule as calendars).
        match self.tz.to_ambiguous_timestamp(wall).compatible() {
            Ok(t) => t.as_second(),
            // Only at the very ends of the database's range.
            Err(_) => local - i64::from(self.tz.to_offset(clamp(local)).seconds()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_plugin_api::{MissedRunRule, MissedRuns, Schedule, missed_runs};

    const H: i64 = 3600;

    /// Unix seconds of a UTC date and time.
    fn utc(y: i16, mo: i8, d: i8, h: i8, mi: i8) -> i64 {
        TimeZone::UTC
            .to_ambiguous_timestamp(jiff::civil::date(y, mo, d).at(h, mi, 0, 0))
            .compatible()
            .unwrap()
            .as_second()
    }

    fn daily(hour: u8, minute: u8) -> Schedule {
        Schedule::DailyAt { hour, minute }
    }

    /// The run times of a daily job between two instants, found by asking minute by
    /// minute, as the scheduler's loop does.
    fn runs(zone: &Zone, s: Schedule, from: i64, to: i64) -> Vec<i64> {
        let mut since = from;
        let mut out = Vec::new();
        let mut now = from;
        while now <= to {
            if let Some(due) = s.due_between(since, now, zone) {
                assert_eq!(due.count, 1, "one run at a time at {now}");
                out.push(due.latest);
                since = now;
            }
            now += 60;
        }
        out
    }

    #[test]
    fn a_time_skipped_in_spring_runs_once_after_the_gap() {
        // Berlin: 2027-03-28 02:00 CET jumps to 03:00 CEST (01:00 UTC).
        let berlin = Zone::named("Europe/Berlin").unwrap();
        let r = runs(
            &berlin,
            daily(2, 30),
            utc(2027, 3, 27, 12, 0),
            utc(2027, 3, 29, 12, 0),
        );
        // 27th 02:30 CET is past; 28th: 02:30 doesn't exist, it runs at 03:30 CEST
        // (01:30 UTC); 29th: 02:30 CEST (00:30 UTC).
        assert_eq!(r, [utc(2027, 3, 28, 1, 30), utc(2027, 3, 29, 0, 30)]);
        // New York: 2027-03-14 02:00 EST jumps to 03:00 EDT (07:00 UTC).
        let ny = Zone::named("America/New_York").unwrap();
        let r = runs(
            &ny,
            daily(2, 30),
            utc(2027, 3, 13, 12, 0),
            utc(2027, 3, 15, 12, 0),
        );
        assert_eq!(r, [utc(2027, 3, 14, 7, 30), utc(2027, 3, 15, 6, 30)]);
    }

    #[test]
    fn a_time_repeated_in_autumn_runs_once_the_first_time() {
        // Berlin: 2027-10-31 03:00 CEST goes back to 02:00 CET (01:00 UTC); 02:30 happens
        // twice. 01:30 happens once.
        let berlin = Zone::named("Europe/Berlin").unwrap();
        let r = runs(
            &berlin,
            daily(1, 30),
            utc(2027, 10, 30, 12, 0),
            utc(2027, 11, 1, 12, 0),
        );
        assert_eq!(r, [utc(2027, 10, 30, 23, 30), utc(2027, 11, 1, 0, 30)]);
        let r = runs(
            &berlin,
            daily(2, 30),
            utc(2027, 10, 30, 12, 0),
            utc(2027, 11, 1, 12, 0),
        );
        assert_eq!(r, [utc(2027, 10, 31, 0, 30), utc(2027, 11, 1, 1, 30)]);
        // New York: 2027-11-07 02:00 EDT goes back to 01:00 EST (06:00 UTC); 01:30 happens
        // twice and runs at the first (05:30 UTC), not again an hour later.
        let ny = Zone::named("America/New_York").unwrap();
        let r = runs(
            &ny,
            daily(1, 30),
            utc(2027, 11, 6, 12, 0),
            utc(2027, 11, 8, 12, 0),
        );
        assert_eq!(r, [utc(2027, 11, 7, 5, 30), utc(2027, 11, 8, 6, 30)]);
    }

    #[test]
    fn a_7am_job_stays_at_7am_local_across_the_change() {
        let berlin = Zone::named("Europe/Berlin").unwrap();
        let r = runs(
            &berlin,
            daily(7, 0),
            utc(2027, 3, 27, 12, 0),
            utc(2027, 3, 29, 12, 0),
        );
        assert_eq!(r, [utc(2027, 3, 28, 5, 0), utc(2027, 3, 29, 5, 0)]);
    }

    #[test]
    fn missed_runs_are_counted_in_local_days_across_a_change() {
        let berlin = Zone::named("Europe/Berlin").unwrap();
        let rule = MissedRunRule::CatchUpOnce {
            fresh_for_secs: 6 * H as u64,
        };
        // Last ran Saturday 27 March 07:00 CET; the Node starts Tuesday 30 March 08:00 CEST.
        // Sunday (23 hours long), Monday and Tuesday were missed: three runs, the latest
        // Tuesday 07:00 CEST.
        let d = missed_runs(
            &daily(7, 0),
            &rule,
            utc(2027, 3, 27, 6, 0),
            utc(2027, 3, 30, 6, 0),
            &berlin,
        );
        assert_eq!(
            d,
            MissedRuns::RunLate {
                scheduled_for: utc(2027, 3, 30, 5, 0),
                missed: 3
            }
        );
        // The same across the autumn change (a 25-hour Sunday).
        let d = missed_runs(
            &daily(7, 0),
            &rule,
            utc(2027, 10, 30, 5, 0),
            utc(2027, 11, 2, 7, 0),
            &berlin,
        );
        assert_eq!(
            d,
            MissedRuns::RunLate {
                scheduled_for: utc(2027, 11, 2, 6, 0),
                missed: 3
            }
        );
    }

    #[test]
    fn only_known_iana_names_are_taken() {
        assert!(Zone::named("Europe/Berlin").is_ok());
        assert!(Zone::named("UTC").is_ok());
        for bad in [
            "",
            "Mars/Olympus",
            "/etc/localtime",
            "../etc/passwd",
            "EST5EDT,M3.2.0",
        ] {
            assert!(Zone::named(bad).is_err(), "{bad}");
        }
    }
}

//! The scheduler loop: decides what's due, runs it through the host, and records the
//! outcome. One scheduler runs per device group: the Node when one is paired, otherwise
//! the desktop app while it's open (ADR-0013 §2). The host decides which one starts it.
//!
//! - **One run at a time.** Catch-ups never start a burst, and a job never overlaps itself.
//! - **Time budget and cancel:** each run gets the job's budget. When the budget ends, or
//!   pause-all or "Delete all" cancels the run, the host's future is dropped and the run
//!   is recorded as `timed_out` or `cancelled`.
//! - **Pause-all and quiet hours:** nothing starts, not even a catch-up or a "Run now"
//!   answer, while either is on.

use crate::decide::{Decision, decide, in_quiet_hours};
use crate::store::{self, Outcome};
use crate::{JobKey, JobsError, Result};
use dn_plugin_api::{BoxFuture, CancelToken, JobContext, JobDecl, PluginError};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// How often the loop looks for due jobs when nothing wakes it, in seconds.
pub const TICK_SECS: u64 = 30;

/// The time source. The host supplies the local UTC offset (the app from the OS; the Node
/// from its configured time zone).
pub trait Clock: Send + Sync {
    /// Unix seconds.
    fn now(&self) -> i64;
    /// The local offset from UTC at `at`, in seconds.
    fn utc_offset_secs(&self, at: i64) -> i64;
}

/// What the scheduler tells the host, so its UI and notifications can follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started {
        job: JobKey,
        run: dn_core::Id,
        late: bool,
    },
    Finished {
        job: JobKey,
        run: dn_core::Id,
        outcome: Outcome,
    },
    Skipped {
        job: JobKey,
        missed: u64,
        not_fresh: bool,
    },
    Asked {
        job: JobKey,
        missed: u64,
    },
}

/// The host boundary: the desktop app and `dn node run` each implement it.
pub trait JobHost: Send + Sync {
    /// Runs one run of `job`: build the add-on's `Host` for this job (its held-approvals
    /// scope included) and call its `JobHandler`. The scheduler enforces the time budget
    /// and the cancel token around this future.
    fn run_job<'a>(
        &'a self,
        job: &'a JobKey,
        ctx: &'a JobContext,
    ) -> BoxFuture<'a, std::result::Result<(), PluginError>>;

    /// Something changed. Called on the scheduler's task, so keep it quick.
    fn changed(&self, event: Event);
}

/// A registered job, as the host's add-on registry knows it.
#[derive(Clone, Debug)]
pub struct ScheduledJob {
    pub key: JobKey,
    pub decl: JobDecl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Planned {
    scheduled_for: i64,
    late: bool,
    covers_runs: u64,
    /// The user's "Run now" answer; asking already settled its time.
    answered: bool,
}

/// The scheduler. The host keeps it in an `Arc`, calls [`Scheduler::run`] on a task, and
/// calls the other methods from its commands.
pub struct Scheduler {
    host: Arc<dyn JobHost>,
    jobs: Vec<ScheduledJob>,
    db: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
    wake: Notify,
    current: Mutex<Option<(JobKey, CancelToken)>>,
}

impl Scheduler {
    pub fn new(
        host: Arc<dyn JobHost>,
        jobs: Vec<ScheduledJob>,
        db: Arc<Mutex<Connection>>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            host,
            jobs,
            db,
            clock,
            wake: Notify::new(),
            current: Mutex::new(None),
        }
    }

    fn with_db<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.db.lock().unwrap_or_else(|p| p.into_inner());
        f(&conn)
    }

    fn quiet_or_paused(&self, conn: &Connection, now: i64) -> Result<bool> {
        if store::paused(conn)? {
            return Ok(true);
        }
        let (start, end) = store::quiet_hours(conn)?;
        Ok(in_quiet_hours(
            now,
            self.clock.utc_offset_secs(now),
            start,
            end,
        ))
    }

    /// Decides for every enabled job, records skips and questions, and returns the runs
    /// to start, on-time ones first.
    fn plan(&self) -> Result<Vec<(usize, Planned)>> {
        let now = self.clock.now();
        let offset = self.clock.utc_offset_secs(now);
        let mut runs = Vec::new();
        let mut events = Vec::new();
        self.with_db(|conn| {
            if self.quiet_or_paused(conn, now)? {
                return Ok(());
            }
            for (i, job) in self.jobs.iter().enumerate() {
                let state = store::job_state(conn, &job.key)?;
                let Some(since) = state.since().filter(|_| state.enabled) else {
                    continue;
                };
                let rule = state.missed_rule.unwrap_or(job.decl.missed_run);
                match decide(&job.decl.schedule, &rule, since, now, offset) {
                    Decision::Wait => {}
                    Decision::Run {
                        scheduled_for,
                        late,
                        covers_runs,
                    } => runs.push((
                        i,
                        Planned {
                            scheduled_for,
                            late,
                            covers_runs,
                            answered: false,
                        },
                    )),
                    Decision::Ask {
                        scheduled_for,
                        missed,
                    } => {
                        store::ask(conn, &job.key, scheduled_for, missed, now)?;
                        events.push(Event::Asked {
                            job: job.key.clone(),
                            missed,
                        });
                    }
                    Decision::Skip {
                        scheduled_for,
                        missed,
                        not_fresh,
                    } => {
                        store::record_skip(conn, &job.key, scheduled_for, missed, not_fresh, now)?;
                        events.push(Event::Skipped {
                            job: job.key.clone(),
                            missed,
                            not_fresh,
                        });
                    }
                }
            }
            Ok(())
        })?;
        for e in events {
            self.host.changed(e);
        }
        runs.sort_by_key(|(_, p)| p.late);
        Ok(runs)
    }

    /// Starts one planned run, or returns `None` when it may not start any more: pause-all
    /// or quiet hours began, the job was turned off or removed, or its time was settled
    /// meanwhile. The cancel token is registered before that check, so pause-all can
    /// always reach a run that does start.
    async fn run_planned(&self, index: usize, planned: Planned) -> Result<Option<Outcome>> {
        let job = &self.jobs[index];
        let cancel = CancelToken::new();
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((job.key.clone(), cancel.clone()));
        let now = self.clock.now();
        let started = self.with_db(|conn| {
            if self.quiet_or_paused(conn, now)? || cancel.is_cancelled() {
                return Ok(None);
            }
            let state = store::job_state(conn, &job.key)?;
            let settled =
                !planned.answered && state.since().is_some_and(|s| s >= planned.scheduled_for);
            if !state.enabled || settled {
                return Ok(None);
            }
            match store::start_run(
                conn,
                &job.key,
                planned.scheduled_for,
                planned.late,
                planned.covers_runs,
                now,
            ) {
                Ok(run) => Ok(Some((run, state.last_success))),
                Err(JobsError::JobOff) => Ok(None),
                Err(e) => Err(e),
            }
        });
        let (run, last_success) = match started {
            Ok(Some(started)) => started,
            other => {
                *self.current.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return other.map(|_| None);
            }
        };
        self.host.changed(Event::Started {
            job: job.key.clone(),
            run,
            late: planned.late,
        });
        let budget = job.decl.budget_secs.max(1);
        let ctx = JobContext::new(
            job.key.job.clone(),
            run,
            planned.scheduled_for,
            last_success,
            planned.late,
            planned.covers_runs,
            now.saturating_add(i64::try_from(budget).unwrap_or(i64::MAX)),
            cancel.clone(),
        );
        let work = self.host.run_job(&job.key, &ctx);
        let stopped = async {
            while !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        let (outcome, error) = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(budget), work) => match result {
                Ok(Ok(())) => (Outcome::Succeeded, None),
                Ok(Err(e)) => (Outcome::Failed, Some(e.to_string())),
                Err(_) => {
                    cancel.cancel();
                    (Outcome::TimedOut, Some(format!("stopped after its time budget of {budget} s")))
                }
            },
            () = stopped => (Outcome::Cancelled, Some("stopped".to_owned())),
        };
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let finished = self.clock.now();
        self.with_db(|conn| store::finish_run(conn, run, outcome, error.as_deref(), finished))?;
        self.host.changed(Event::Finished {
            job: job.key.clone(),
            run,
            outcome,
        });
        Ok(Some(outcome))
    }

    /// One pass: plan, then start the due runs one at a time. Returns how many ran.
    pub async fn run_due(&self) -> Result<usize> {
        let mut ran = 0;
        for (index, planned) in self.plan()? {
            // Pause-all or quiet hours may have started during an earlier run.
            let now = self.clock.now();
            if self.with_db(|conn| self.quiet_or_paused(conn, now))? {
                break;
            }
            if self.run_planned(index, planned).await?.is_some() {
                ran += 1;
            }
        }
        Ok(ran)
    }

    /// The user answered "Run it now?": run it late now, or record the skip.
    pub async fn answer_question(&self, key: &JobKey, run_now: bool) -> Result<Option<Outcome>> {
        let index = self
            .jobs
            .iter()
            .position(|j| &j.key == key)
            .ok_or(JobsError::NoQuestion)?;
        let now = self.clock.now();
        if run_now && self.with_db(|conn| self.quiet_or_paused(conn, now))? {
            // The question stays; it's answered once pause-all and quiet hours are over.
            return Ok(None);
        }
        let q = self.with_db(|conn| store::take_question(conn, key))?;
        if !run_now {
            self.with_db(|conn| {
                store::record_skip(conn, key, q.scheduled_for, q.missed, false, now)
            })?;
            self.host.changed(Event::Skipped {
                job: key.clone(),
                missed: q.missed,
                not_fresh: false,
            });
            return Ok(None);
        }
        let planned = Planned {
            scheduled_for: q.scheduled_for,
            late: true,
            covers_runs: q.missed,
            answered: true,
        };
        self.run_planned(index, planned).await
    }

    /// Pause-all: stops the running job at once and starts nothing until it's turned off.
    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.with_db(|conn| store::set_paused(conn, paused))?;
        if paused {
            self.cancel_current();
        } else {
            self.wake.notify_one();
        }
        Ok(())
    }

    /// Cancels the running job, if any (pause-all, "Delete all", shutdown).
    pub fn cancel_current(&self) {
        if let Some((_, c)) = self
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            c.cancel();
        }
    }

    /// Turns a job on or off. Turning off the job that is running stops it at once.
    pub fn set_enabled(&self, key: &JobKey, enabled: bool) -> Result<()> {
        let now = self.clock.now();
        self.with_db(|conn| store::set_enabled(conn, key, enabled, now))?;
        if enabled {
            self.wake.notify_one();
            return Ok(());
        }
        let current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((running, c)) = current.as_ref()
            && running == key
        {
            c.cancel();
        }
        Ok(())
    }

    /// "Delete all local data": stops the running job, then empties every job table and
    /// the held-approvals queue. Every job is then off, so nothing runs afterwards.
    pub fn delete_all(&self) -> Result<()> {
        self.cancel_current();
        self.with_db(|conn| {
            let tx = conn.unchecked_transaction()?;
            store::delete_all(&tx)?;
            crate::held::delete_all(&tx)?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Looks again now (a job was turned on, a setting changed).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Runs until `stop` is notified: marks runs a crash interrupted, then checks every
    /// [`TICK_SECS`] or when woken. An error in one pass is returned to the host, which
    /// shows it; the loop doesn't swallow it.
    pub async fn run(&self, stop: &Notify) -> Result<()> {
        let now = self.clock.now();
        self.with_db(|conn| {
            store::recover(conn, now)?;
            crate::held::recover(conn)?;
            Ok(())
        })?;
        loop {
            self.run_due().await?;
            tokio::select! {
                () = stop.notified() => {
                    self.cancel_current();
                    return Ok(());
                }
                () = self.wake.notified() => {}
                () = tokio::time::sleep(Duration::from_secs(TICK_SECS)) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{db, key};
    use dn_plugin_api::{MissedRunRule, Schedule};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    const H: i64 = 3600;
    const MON: i64 = 1_795_392_000;

    struct FakeClock(AtomicI64);
    impl Clock for FakeClock {
        fn now(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
        fn utc_offset_secs(&self, _: i64) -> i64 {
            0
        }
    }

    #[derive(Default)]
    struct FakeHost {
        runs: AtomicUsize,
        mode: AtomicUsize, // 0 ok, 1 fail, 2 hang
        events: Mutex<Vec<Event>>,
        contexts: Mutex<Vec<(bool, u64, i64)>>,
    }
    impl JobHost for FakeHost {
        fn run_job<'a>(
            &'a self,
            _job: &'a JobKey,
            ctx: &'a JobContext,
        ) -> BoxFuture<'a, std::result::Result<(), PluginError>> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            self.contexts
                .lock()
                .unwrap()
                .push((ctx.late, ctx.covers_runs, ctx.scheduled_for));
            let mode = self.mode.load(Ordering::SeqCst);
            Box::pin(async move {
                match mode {
                    0 => Ok(()),
                    1 => Err(PluginError::failed("the mail server refused")),
                    _ => {
                        std::future::pending::<()>().await;
                        Ok(())
                    }
                }
            })
        }
        fn changed(&self, event: Event) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn setup(
        rule: MissedRunRule,
        enabled_at: i64,
        now: i64,
    ) -> (Arc<FakeHost>, Scheduler, Arc<FakeClock>) {
        let conn = db();
        let k = key("daily");
        store::set_enabled(&conn, &k, true, enabled_at).unwrap();
        let host = Arc::new(FakeHost::default());
        let clock = Arc::new(FakeClock(AtomicI64::new(now)));
        let decl = JobDecl::new(
            "daily",
            "Morning brief",
            Schedule::DailyAt { hour: 7, minute: 0 },
            6 * 3600,
            60,
        )
        .with_missed_run(rule);
        let sched = Scheduler::new(
            host.clone(),
            vec![ScheduledJob { key: k, decl }],
            Arc::new(Mutex::new(conn)),
            clock.clone(),
        );
        (host, sched, clock)
    }

    const FRESH: MissedRunRule = MissedRunRule::CatchUpOnce {
        fresh_for_secs: 6 * 3600,
    };

    #[tokio::test]
    async fn an_on_time_run_happens_once() {
        let (host, s, clock) = setup(FRESH, MON, MON + 7 * H + 10);
        assert_eq!(s.run_due().await.unwrap(), 1);
        assert_eq!(s.run_due().await.unwrap(), 0, "not twice for the same time");
        clock.0.store(MON + 31 * H, Ordering::SeqCst);
        assert_eq!(s.run_due().await.unwrap(), 1);
        assert_eq!(host.runs.load(Ordering::SeqCst), 2);
        assert_eq!(host.contexts.lock().unwrap()[0], (false, 1, MON + 7 * H));
    }

    #[tokio::test]
    async fn missed_runs_catch_up_once_and_say_they_are_late() {
        // Off for three days, back at 08:00: one late run covering four.
        let (host, s, _) = setup(FRESH, MON, MON + 72 * H + 8 * H);
        assert_eq!(s.run_due().await.unwrap(), 1);
        assert_eq!(s.run_due().await.unwrap(), 0, "never a burst");
        assert_eq!(host.contexts.lock().unwrap()[0], (true, 4, MON + 79 * H));
        let h = store::history(&s.db.lock().unwrap(), &key("daily"), 5).unwrap();
        assert!(h[0].late);
    }

    #[tokio::test]
    async fn an_ask_rule_waits_for_the_user() {
        let (host, s, _) = setup(MissedRunRule::Ask, MON, MON + 10 * H);
        assert_eq!(s.run_due().await.unwrap(), 0);
        assert_eq!(s.run_due().await.unwrap(), 0, "asked once");
        assert_eq!(store::questions(&s.db.lock().unwrap()).unwrap().len(), 1);
        assert_eq!(
            s.answer_question(&key("daily"), true).await.unwrap(),
            Some(Outcome::Succeeded)
        );
        assert_eq!(host.runs.load(Ordering::SeqCst), 1);
        assert!(matches!(
            s.answer_question(&key("daily"), true).await,
            Err(JobsError::NoQuestion)
        ));
    }

    #[tokio::test]
    async fn skip_answers_and_stale_runs_are_recorded_not_run() {
        let (host, s, _) = setup(MissedRunRule::Ask, MON, MON + 10 * H);
        s.run_due().await.unwrap();
        assert_eq!(s.answer_question(&key("daily"), false).await.unwrap(), None);
        assert_eq!(host.runs.load(Ordering::SeqCst), 0);
        let (host, s, _) = setup(FRESH, MON, MON + 20 * H);
        assert_eq!(s.run_due().await.unwrap(), 0);
        assert_eq!(host.runs.load(Ordering::SeqCst), 0);
        let h = store::history(&s.db.lock().unwrap(), &key("daily"), 5).unwrap();
        assert_eq!(h[0].outcome, Outcome::Skipped);
        assert!(h[0].not_fresh);
    }

    #[tokio::test]
    async fn a_failure_is_recorded_and_not_retried_in_a_loop() {
        let (host, s, _) = setup(FRESH, MON, MON + 7 * H);
        host.mode.store(1, Ordering::SeqCst);
        s.run_due().await.unwrap();
        s.run_due().await.unwrap();
        assert_eq!(host.runs.load(Ordering::SeqCst), 1);
        let h = store::history(&s.db.lock().unwrap(), &key("daily"), 5).unwrap();
        assert_eq!(h[0].outcome, Outcome::Failed);
        assert_eq!(h[0].error.as_deref(), Some("the mail server refused"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_past_its_budget_is_stopped() {
        let (host, s, _) = setup(FRESH, MON, MON + 7 * H);
        host.mode.store(2, Ordering::SeqCst);
        s.run_due().await.unwrap();
        let h = store::history(&s.db.lock().unwrap(), &key("daily"), 5).unwrap();
        assert_eq!(h[0].outcome, Outcome::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn pause_all_cancels_the_running_job_and_starts_nothing() {
        let (host, s, _) = setup(FRESH, MON, MON + 7 * H);
        host.mode.store(2, Ordering::SeqCst);
        let s = Arc::new(s);
        let runner = {
            let s = s.clone();
            tokio::spawn(async move { s.run_due().await })
        };
        while host.runs.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        s.set_paused(true).unwrap();
        runner.await.unwrap().unwrap();
        let h = store::history(&s.db.lock().unwrap(), &key("daily"), 5).unwrap();
        assert_eq!(h[0].outcome, Outcome::Cancelled);
        // While paused, even a due run and a "Run now" answer wait.
        store::set_enabled(&s.db.lock().unwrap(), &key("daily"), true, MON).unwrap();
        assert_eq!(s.run_due().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn quiet_hours_hold_runs_until_they_end() {
        let (host, s, clock) = setup(FRESH, MON, MON + 7 * H);
        store::set_quiet_hours(&s.db.lock().unwrap(), 6 * 60, 8 * 60).unwrap();
        assert_eq!(s.run_due().await.unwrap(), 0);
        clock.0.store(MON + 8 * H + 5, Ordering::SeqCst);
        assert_eq!(s.run_due().await.unwrap(), 1);
        assert!(host.contexts.lock().unwrap()[0].0, "it ran late");
    }

    #[tokio::test]
    async fn delete_all_empties_jobs_and_held_calls() {
        let (_, s, _) = setup(FRESH, MON, MON + 7 * H);
        s.run_due().await.unwrap();
        crate::held::hold(
            &s.db.lock().unwrap(),
            &crate::held::HoldRequest {
                job: key("daily"),
                run: dn_core::Id::new(),
                tool: "plugin.brief.send_mail".into(),
                arguments: serde_json::json!({}),
                summary: "Send".into(),
            },
            None,
            MON,
        )
        .unwrap();
        s.delete_all().unwrap();
        let conn = s.db.lock().unwrap();
        assert!(store::history(&conn, &key("daily"), 5).unwrap().is_empty());
        assert!(crate::held::waiting(&conn, MON).unwrap().is_empty());
        assert!(!store::job_state(&conn, &key("daily")).unwrap().enabled);
    }

    #[tokio::test]
    async fn a_job_turned_off_while_another_runs_does_not_start() {
        // Two jobs due in one pass; the second is turned off while the first runs.
        let conn = db();
        let a = key("first");
        let b = key("second");
        store::set_enabled(&conn, &a, true, MON).unwrap();
        store::set_enabled(&conn, &b, true, MON).unwrap();
        let db = Arc::new(Mutex::new(conn));
        struct TurnsOffB(Arc<Mutex<Connection>>, AtomicUsize);
        impl JobHost for TurnsOffB {
            fn run_job<'a>(
                &'a self,
                _job: &'a JobKey,
                _ctx: &'a JobContext,
            ) -> BoxFuture<'a, std::result::Result<(), PluginError>> {
                self.1.fetch_add(1, Ordering::SeqCst);
                store::set_enabled(&self.0.lock().unwrap(), &key("second"), false, MON).unwrap();
                Box::pin(async { Ok(()) })
            }
            fn changed(&self, _: Event) {}
        }
        let host = Arc::new(TurnsOffB(db.clone(), AtomicUsize::new(0)));
        let decl = |id: &str| {
            JobDecl::new(
                id,
                id,
                Schedule::DailyAt { hour: 7, minute: 0 },
                6 * 3600,
                60,
            )
        };
        let s = Scheduler::new(
            host.clone(),
            vec![
                ScheduledJob {
                    key: a,
                    decl: decl("first"),
                },
                ScheduledJob {
                    key: b.clone(),
                    decl: decl("second"),
                },
            ],
            db,
            Arc::new(FakeClock(AtomicI64::new(MON + 7 * H))),
        );
        assert_eq!(s.run_due().await.unwrap(), 1);
        assert_eq!(host.1.load(Ordering::SeqCst), 1);
        assert!(
            store::history(&s.db.lock().unwrap(), &b, 5)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn turning_off_the_running_job_stops_it() {
        let (host, s, _) = setup(FRESH, MON, MON + 7 * H);
        host.mode.store(2, Ordering::SeqCst);
        let s = Arc::new(s);
        let runner = {
            let s = s.clone();
            tokio::spawn(async move { s.run_due().await })
        };
        while host.runs.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        s.set_enabled(&key("daily"), false).unwrap();
        runner.await.unwrap().unwrap();
        let h = store::history(&s.db.lock().unwrap(), &key("daily"), 5).unwrap();
        assert_eq!(h[0].outcome, Outcome::Cancelled);
    }

    #[tokio::test(start_paused = true)]
    async fn delete_all_during_a_run_stops_it_without_an_error() {
        let (host, s, _) = setup(FRESH, MON, MON + 7 * H);
        host.mode.store(2, Ordering::SeqCst);
        let s = Arc::new(s);
        let runner = {
            let s = s.clone();
            tokio::spawn(async move { s.run_due().await })
        };
        while host.runs.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        s.delete_all().unwrap();
        runner.await.unwrap().unwrap();
        assert!(
            store::history(&s.db.lock().unwrap(), &key("daily"), 5)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_job_that_is_off_never_runs() {
        let (host, s, _) = setup(FRESH, MON, MON + 7 * H);
        store::set_enabled(&s.db.lock().unwrap(), &key("daily"), false, MON).unwrap();
        assert_eq!(s.run_due().await.unwrap(), 0);
        assert_eq!(host.runs.load(Ordering::SeqCst), 0);
    }
}

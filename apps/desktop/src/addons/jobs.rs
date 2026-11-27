//! Add-on background jobs in the desktop app, on `dn-jobs` (ADR-0013 §2). This module only
//! connects the app to `dn-jobs`: the scheduler, missed runs, the held-approvals queue and
//! run history are all there.
//!
//! - Jobs at a time of day follow this computer's time zone, daylight saving included
//!   (#299, [`AppClock`]). The scheduler starts only once the zone is read
//!   ([`Jobs::start`]); if it can't be read, no job runs and the window says so, rather
//!   than running at the wrong local time.
//! - Only jobs of add-ons that are loaded, turned on and hold `jobs` are scheduled. Each
//!   job is still off until the person turns it on (dn-jobs' default).
//! - The app has no Node here, so held calls and "Run it now?" questions are answered in
//!   this window only ([`WindowOnly`]).

use super::{Addons, backend};
use crate::state::AppState;
use dn_jobs::JobKey;
use dn_jobs::held::{self, AnswerVerifier, HeldApproval, HoldRequest};
use dn_jobs::scheduler::{Clock, Event, JobHost, ScheduledJob, Scheduler};
use dn_jobs::store::{self, Outcome, RunRecord};
use dn_jobs::zone::Zone;
use dn_plugin_api::{
    BoxFuture, Caller, JobContext, LocalTime, MissedRunRule, Permission, PluginError, PluginId,
    Registrations, Schedule,
};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use tokio::sync::Notify;

/// Who answers in the app's own window.
pub const WINDOW: &str = "this-window";

/// The app with no Node: answers come from its own window, unsigned.
pub struct WindowOnly;

impl AnswerVerifier for WindowOnly {
    fn verify(
        &self,
        answered_by: &str,
        _message: &[u8],
        signature: Option<&[u8]>,
    ) -> Result<(), String> {
        if answered_by == WINDOW && signature.is_none() {
            Ok(())
        } else {
            Err("only this window can answer here".into())
        }
    }
}

/// Unix seconds now.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Where the clock reads the time zone from: the system's ([`Zone::system`]), or a test's.
pub type ZoneSource = dyn Fn() -> Result<Zone, String> + Send + Sync;

/// The time, and this computer's time zone with its daylight-saving rules.
///
/// The zone is read again at every scheduler tick (jiff re-reads the system's at most every
/// 5 minutes), so a job at 07:00 stays at 07:00 local time after the clocks change or the
/// person moves to another zone. If reading it fails later, the zone read last stays and
/// the failure is logged once.
pub struct AppClock {
    zone: RwLock<Zone>,
    source: Box<ZoneSource>,
    failed: Mutex<Option<String>>,
}

impl AppClock {
    /// Reads the zone once; an error when it can't be read.
    pub fn new(source: Box<ZoneSource>) -> Result<Self, String> {
        let zone = source()?;
        Ok(Self {
            zone: RwLock::new(zone),
            source,
            failed: Mutex::new(None),
        })
    }

    fn refresh(&self) {
        match (self.source)() {
            Ok(z) => {
                *self.zone.write().unwrap_or_else(|p| p.into_inner()) = z;
                *lock(&self.failed) = None;
            }
            Err(e) => {
                let mut failed = lock(&self.failed);
                if failed.as_deref() != Some(e.as_str()) {
                    eprintln!("add-on jobs: {e}; keeping the time zone read last");
                    *failed = Some(e);
                }
            }
        }
    }

    fn zone(&self) -> std::sync::RwLockReadGuard<'_, Zone> {
        self.zone.read().unwrap_or_else(|p| p.into_inner())
    }
}

impl Clock for AppClock {
    fn now(&self) -> i64 {
        self.refresh();
        now()
    }
}

impl LocalTime for AppClock {
    fn to_local(&self, utc: i64) -> i64 {
        self.zone().to_local(utc)
    }

    fn to_utc(&self, local: i64) -> i64 {
        self.zone().to_utc(local)
    }
}

struct Running {
    scheduler: Arc<Scheduler>,
    stop: Arc<Notify>,
}

/// The jobs side of the registry.
#[derive(Default)]
pub struct Jobs {
    /// Set once the time zone is read ([`Jobs::start`]).
    clock: OnceLock<Arc<AppClock>>,
    /// Set by "Delete all local data": nothing is scheduled again until restart.
    deleted: AtomicBool,
    running: Mutex<Option<Running>>,
    notify_ui: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn jobs_db(state: &AppState) -> Result<Arc<Mutex<Connection>>, String> {
    state
        .jobs_db
        .clone()
        .ok_or_else(|| "Background jobs aren't available.".to_owned())
}

fn err(e: dn_jobs::JobsError) -> String {
    e.to_string()
}

impl Jobs {
    /// Tells the UI something changed (set by the app at start).
    pub fn on_change(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *lock(&self.notify_ui) = Some(f);
    }

    fn changed(&self) {
        if let Some(f) = lock(&self.notify_ui).clone() {
            f();
        }
    }

    /// Reads this computer's time zone and starts the scheduler. Later calls do nothing.
    /// An error, and no jobs, when the zone can't be read.
    pub fn start(&self, state: &Arc<AppState>) -> Result<(), String> {
        self.start_with(state, Box::new(Zone::system))
    }

    pub(crate) fn start_with(
        &self,
        state: &Arc<AppState>,
        source: Box<ZoneSource>,
    ) -> Result<(), String> {
        if self.clock.get().is_some() {
            return Ok(());
        }
        let clock = Arc::new(AppClock::new(source)?);
        if self.clock.set(clock).is_ok() {
            self.rebuild(state);
        }
        Ok(())
    }

    fn scheduler(&self) -> Option<Arc<Scheduler>> {
        lock(&self.running)
            .as_ref()
            .map(|r| Arc::clone(&r.scheduler))
    }

    /// Stops the current scheduler and starts one with the jobs that may run now: of
    /// add-ons that are loaded, turned on and hold `jobs`.
    pub fn rebuild(&self, state: &Arc<AppState>) {
        if let Some(r) = lock(&self.running).take() {
            r.scheduler.cancel_current();
            r.stop.notify_one();
        }
        let Some(clock) = self.clock.get().cloned() else {
            return;
        };
        if self.deleted.load(Ordering::SeqCst) {
            return;
        }
        let Ok(db) = jobs_db(state) else {
            return;
        };
        let Ok(app_db) = state.db() else {
            return;
        };
        let mut scheduled = Vec::new();
        for (manifest, _, regs) in state.addons.loaded() {
            let ok = super::store::state(&app_db, &manifest.id)
                .is_ok_and(|s| s.enabled && s.allowed.contains(&Permission::Jobs));
            if !ok {
                continue;
            }
            for j in &regs.jobs {
                scheduled.push(ScheduledJob {
                    key: JobKey::new(manifest.id.clone(), j.decl.id.clone()),
                    decl: j.decl.clone(),
                });
            }
        }
        drop(app_db);
        if scheduled.is_empty() {
            return;
        }
        let host = Arc::new(AppJobHost {
            state: Arc::downgrade(state),
        });
        let scheduler = Arc::new(Scheduler::new(host, scheduled, db, clock as Arc<dyn Clock>));
        let stop = Arc::new(Notify::new());
        *lock(&self.running) = Some(Running {
            scheduler: Arc::clone(&scheduler),
            stop: Arc::clone(&stop),
        });
        tauri::async_runtime::spawn(async move {
            if let Err(e) = scheduler.run(&stop).await {
                eprintln!("background jobs stopped: {e}");
            }
        });
    }

    /// "Delete all local data": stop the running job and the loop, and schedule nothing
    /// again until restart.
    pub fn stop_for_deletion(&self, _state: &Arc<AppState>) {
        self.deleted.store(true, Ordering::SeqCst);
        if let Some(r) = lock(&self.running).take() {
            r.scheduler.cancel_current();
            r.stop.notify_one();
        }
    }

    /// Pause all background jobs.
    pub fn set_paused(&self, state: &AppState, paused: bool) -> Result<(), String> {
        match self.scheduler() {
            Some(s) => s.set_paused(paused).map_err(err)?,
            None => {
                let db = jobs_db(state)?;
                store::set_paused(&lock(&db), paused).map_err(err)?;
            }
        }
        self.changed();
        Ok(())
    }

    pub fn paused(&self, state: &AppState) -> Result<bool, String> {
        let db = jobs_db(state)?;
        store::paused(&lock(&db)).map_err(err)
    }

    /// Turn one job on or off.
    pub fn set_enabled(&self, state: &AppState, key: &JobKey, on: bool) -> Result<(), String> {
        owned_job(state, key)?;
        match self.scheduler() {
            Some(s) => s.set_enabled(key, on).map_err(err)?,
            None => {
                let db = jobs_db(state)?;
                let now = now();
                store::set_enabled(&lock(&db), key, on, now).map_err(err)?;
            }
        }
        self.changed();
        Ok(())
    }

    /// The person's missed-run rule for a job (`None`: the add-on's proposal).
    pub fn set_missed_rule(
        &self,
        state: &AppState,
        key: &JobKey,
        rule: Option<MissedRunRule>,
    ) -> Result<(), String> {
        owned_job(state, key)?;
        if let Some(r) = &rule {
            r.validate().map_err(|e| e.message)?;
        }
        let db = jobs_db(state)?;
        store::set_missed_rule(&lock(&db), key, rule).map_err(err)?;
        if let Some(s) = self.scheduler() {
            s.wake();
        }
        self.changed();
        Ok(())
    }

    /// "Run it now?": Run now or Skip.
    pub async fn answer_question(
        &self,
        state: &AppState,
        key: &JobKey,
        run_now: bool,
    ) -> Result<(), String> {
        owned_job(state, key)?;
        let s = self
            .scheduler()
            .ok_or_else(|| "Background jobs aren't running.".to_owned())?;
        s.answer_question(key, run_now).await.map_err(err)?;
        self.changed();
        Ok(())
    }

    /// The person allowed or refused a held call, for the exact arguments the window showed.
    /// An allowed call then runs, once, with that answer as its approval.
    pub async fn answer_held(
        &self,
        state: &Arc<AppState>,
        id: dn_core::Id,
        approve: bool,
        arguments_sha256: String,
    ) -> Result<(), String> {
        let db = jobs_db(state)?;
        let now = now();
        held::answer(
            &lock(&db),
            &WindowOnly,
            &held::Answer {
                approval: id,
                approve,
                arguments_sha256,
                answered_by: WINDOW.into(),
                signature: None,
            },
            now,
        )
        .map_err(err)?;
        self.changed();
        if !approve {
            return Ok(());
        }
        let call = held::take_approved(&lock(&db), id).map_err(err)?;
        let result = run_approved(state, call).await;
        held::finish(&lock(&db), id, result.clone()).map_err(err)?;
        self.changed();
        result
    }

    /// The person dismissed a held call: it won't run.
    pub fn dismiss_held(&self, state: &AppState, id: dn_core::Id) -> Result<(), String> {
        let db = jobs_db(state)?;
        held::dismiss(&lock(&db), id).map_err(err)?;
        self.changed();
        Ok(())
    }
}

/// Refuses a job key no loaded add-on owns.
fn owned_job(state: &AppState, key: &JobKey) -> Result<(), String> {
    let db = state.db().map_err(|e| e.message)?;
    match super::store::owner(&db, super::store::Owned::Job, &key.to_string()) {
        Ok(Some(o)) if o == key.plugin.as_str() => Ok(()),
        _ => Err(format!("There is no background job `{key}`.")),
    }
}

/// Runs a held call the person allowed: through the executor, with this answer as its only
/// approval for this one call. A tool that always needs a fresh approval (ADR-0013 §9.4)
/// runs with the one-time approval made from this answer; "run without asking" never
/// counts for it.
async fn run_approved(state: &Arc<AppState>, approved: held::Approved) -> Result<(), String> {
    let call: &HeldApproval = &approved;
    // What was turned off since the call was held takes effect now too: the add-on must
    // still be running and on, and still hold `tools.call` for a tool that isn't its own.
    let plugin = &call.job.plugin;
    let (manifest, _, _) = state
        .addons
        .loaded()
        .into_iter()
        .find(|(m, _, _)| &m.id == plugin)
        .ok_or_else(|| "The add-on isn't running, so the call wasn't run.".to_owned())?;
    let st = super::store::state(&*state.db().map_err(|e| e.message)?, plugin)
        .map_err(|e| e.to_string())?;
    if !st.enabled {
        return Err(format!("{} is off, so the call wasn't run.", manifest.name));
    }
    let own = call
        .tool
        .strip_prefix(&format!("plugin.{plugin}."))
        .is_some_and(|n| !n.contains('.'));
    if !own && !st.allowed.contains(&Permission::ToolsCall) {
        return Err(format!(
            "{} can't use other tools any more, so the call wasn't run.",
            manifest.name
        ));
    }
    let exec = state
        .executor
        .clone()
        .ok_or_else(|| "Tools aren't available.".to_owned())?;
    let name = state
        .addons
        .with_entry(&call.job.plugin, |e| e.manifest.name.clone())
        .ok_or_else(|| "The add-on is gone.".to_owned())?;
    let run = call.run;
    let (call, tc, token) = exec
        .approval_for_held(approved)
        .map_err(|e| e.to_string())?;
    let ctx = dn_tools::CallContext::for_plugin(
        call.run,
        dn_tools::PluginCall {
            plugin_id: call.job.plugin.to_string(),
            name,
            max_tier: dn_core::capability::RiskTier::Sensitive,
            job: Some(dn_tools::JobCall {
                id: call.job.job.clone(),
                name: call.job.job.clone(),
                auto_tools: vec![call.tool.clone()],
            }),
        },
        Vec::new(),
    )
    .with_approval(token);
    let result = exec
        .execute(&ctx, &tc)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    // Unused (a tool outside the class runs on its "run without asking" choice): gone.
    exec.revoke_held_approval(run, call.id);
    result
}

/// Holds a job's tool call for the person (the app's part of `HostBackend::call_tool`).
pub(crate) fn hold_tool(
    state: &AppState,
    caller: &Caller,
    job_id: &str,
    tool: &str,
    arguments: &serde_json::Value,
) -> Result<(), PluginError> {
    let db = jobs_db(state).map_err(PluginError::unavailable)?;
    let summary = state
        .executor
        .as_ref()
        .and_then(|e| e.resolve(tool))
        .map(|c| c.title.unwrap_or(c.tool_name))
        .unwrap_or_else(|| tool.to_owned());
    held::hold(
        &lock(&db),
        &HoldRequest {
            job: JobKey::new(caller.plugin_id.clone(), job_id),
            run: caller.run,
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            summary,
        },
        None,
        now(),
    )
    .map_err(|e| PluginError::failed(e.to_string()))?;
    state.addons.jobs.changed();
    Ok(())
}

/// Held calls and "Run it now?" questions, per add-on.
pub fn waiting_counts(state: &AppState) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let Ok(db) = jobs_db(state) else {
        return out;
    };
    let conn = lock(&db);
    let now = now();
    for h in held::waiting(&conn, now).unwrap_or_default() {
        *out.entry(h.job.plugin.to_string()).or_default() += 1;
    }
    for q in store::questions(&conn).unwrap_or_default() {
        *out.entry(q.job.plugin.to_string()).or_default() += 1;
    }
    out
}

/// One job on the add-on's page.
#[derive(Clone, Debug, Serialize)]
pub struct JobView {
    pub id: String,
    pub name: String,
    pub schedule: Schedule,
    pub enabled: bool,
    /// The rule in force, and whether it is the add-on's proposal.
    pub missed_rule: MissedRunRule,
    pub missed_rule_is_default: bool,
    pub proposed_rule: MissedRunRule,
    pub last_success: Option<i64>,
    /// Newest first, at most 20.
    pub history: Vec<RunRecord>,
    /// "Run it now?", if one is waiting.
    pub question: Option<QuestionView>,
    pub running: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuestionView {
    pub scheduled_for: i64,
    pub missed: u64,
}

/// A held call on the add-on's page.
#[derive(Clone, Debug, Serialize)]
pub struct HeldView {
    pub id: dn_core::Id,
    pub job_id: String,
    pub tool: String,
    pub summary: String,
    /// The exact arguments, as stored.
    pub arguments: String,
    pub arguments_sha256: String,
    pub created_at: i64,
}

pub fn views(
    state: &AppState,
    plugin: &PluginId,
    regs: &Registrations,
) -> Result<Vec<JobView>, String> {
    if regs.jobs.is_empty() {
        return Ok(Vec::new());
    }
    let db = jobs_db(state)?;
    let conn = lock(&db);
    let questions = store::questions(&conn).map_err(err)?;
    regs.jobs
        .iter()
        .map(|j| {
            let key = JobKey::new(plugin.clone(), j.decl.id.clone());
            let st = store::job_state(&conn, &key).map_err(err)?;
            let history = store::history(&conn, &key, 20).map_err(err)?;
            Ok(JobView {
                id: j.decl.id.clone(),
                name: j.decl.name.clone(),
                schedule: j.decl.schedule,
                enabled: st.enabled,
                missed_rule: st.missed_rule.unwrap_or(j.decl.missed_run),
                missed_rule_is_default: st.missed_rule.is_none(),
                proposed_rule: j.decl.missed_run,
                last_success: st.last_success,
                running: history
                    .first()
                    .is_some_and(|r| r.outcome == Outcome::Running),
                history,
                question: questions
                    .iter()
                    .find(|q| q.job == key)
                    .map(|q| QuestionView {
                        scheduled_for: q.scheduled_for,
                        missed: q.missed,
                    }),
            })
        })
        .collect()
}

pub fn held_for(state: &AppState, plugin: &PluginId) -> Result<Vec<HeldView>, String> {
    let Ok(db) = jobs_db(state) else {
        return Ok(Vec::new());
    };
    let now = now();
    let all = held::waiting(&lock(&db), now).map_err(err)?;
    Ok(all
        .into_iter()
        .filter(|h| &h.job.plugin == plugin)
        .map(|h| HeldView {
            id: h.id,
            job_id: h.job.job,
            tool: h.tool,
            summary: h.summary,
            arguments: h.arguments,
            arguments_sha256: h.arguments_sha256,
            created_at: h.created_at,
        })
        .collect())
}

/// dn-jobs' host: runs a job by building the add-on's `Host` and calling its handler.
struct AppJobHost {
    state: Weak<AppState>,
}

impl JobHost for AppJobHost {
    fn run_job<'a>(
        &'a self,
        job: &'a JobKey,
        ctx: &'a JobContext,
    ) -> BoxFuture<'a, Result<(), PluginError>> {
        Box::pin(async move {
            let state = self
                .state
                .upgrade()
                .ok_or_else(|| PluginError::unavailable("Demios is closing"))?;
            let found = state
                .addons
                .loaded()
                .into_iter()
                .find(|(m, _, _)| m.id == job.plugin)
                .and_then(|(m, g, r)| {
                    r.jobs
                        .iter()
                        .find(|j| j.decl.id == job.job)
                        .map(|j| (m, g, Arc::clone(&j.handler)))
                });
            let Some((manifest, grants, handler)) = found else {
                return Err(PluginError::unavailable("The add-on isn't running"));
            };
            let host = Addons::job_host(&state, &manifest, grants, &job.job, ctx.run);
            match backend::guarded(handler.run(&host, ctx)).await {
                Some(r) => r,
                None => {
                    state.addons.stop_after_panic(&manifest.id);
                    Err(PluginError::failed(format!(
                        "{} stopped after an error and is off until Demios restarts.",
                        manifest.name
                    )))
                }
            }
        })
    }

    fn changed(&self, _event: Event) {
        if let Some(state) = self.state.upgrade() {
            state.addons.jobs.changed();
        }
    }
}

//! dn-jobs: background jobs for every Demios runtime, the desktop app and the Node
//! (ADR-0013 §2, ADR-0014 §6).
//!
//! - [`decide`]: when a job runs. On time, or what to do about missed runs, using
//!   `dn-plugin-api`'s fixed missed-run rule ([`dn_plugin_api::missed_runs`]), never a
//!   copy of it.
//! - [`store`]: per-job settings (on or off, the user's missed-run rule), state, run
//!   history, "Run it now?" questions, pause-all and quiet hours, in `jobs_*` tables.
//! - [`held`]: the held-approvals queue, in `held_*` tables. A held call is never
//!   approved by itself. An answer must pass the host's [`held::AnswerVerifier`] (on a
//!   Node: signed by a paired device), applies once, and expiry is a refusal.
//! - [`budget`]: the daily token budget, counted per run, job and local day, and checked
//!   before a run starts. A run that doesn't fit is skipped with the reason, never started.
//! - [`scheduler`]: the loop that runs due jobs one at a time through a host-provided
//!   [`scheduler::JobHost`], with a time budget and a cancel token per run.
//! - [`zone`]: the time zone jobs at a time of day follow, daylight saving included.
//!
//! The host (`dn node run`, or the desktop app) owns the database connection and builds
//! each add-on's `Host`. This crate never sees add-on internals.

pub mod budget;
pub mod decide;
pub mod held;
pub mod scheduler;
pub mod store;
pub mod zone;

use dn_plugin_api::PluginId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Schema migrations for this crate (tables `jobs_*` and `held_*`), in order; new ones
/// are only appended. `dn-store` runs them.
pub fn migrations() -> &'static [&'static str] {
    // Appended in order: a migration that already ran is never changed or moved.
    &[
        store::MIGRATION,
        held::MIGRATION,
        budget::MIGRATION,
        held::MIGRATION_WITHDRAW,
    ]
}

/// One add-on job: the add-on's id and the job's id within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobKey {
    pub plugin: PluginId,
    pub job: String,
}

impl JobKey {
    pub fn new(plugin: PluginId, job: impl Into<String>) -> Self {
        Self {
            plugin,
            job: job.into(),
        }
    }
}

impl fmt::Display for JobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.plugin, self.job)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("stored data is damaged: {0}")]
    Damaged(String),
    #[error("no held approval with that id")]
    UnknownApproval,
    #[error("this approval was already answered, dismissed or expired")]
    NotHeld,
    /// The add-on withdrew the call, or replaced it with a newer version (`replaced_by`).
    /// An answer to it is refused.
    #[error("{}", match .replaced_by {
        Some(_) => "this was replaced by a newer version",
        None => "this was withdrawn by its add-on",
    })]
    Withdrawn { replaced_by: Option<dn_core::Id> },
    #[error("this approval expired and wasn't run")]
    Expired,
    #[error("the answer is for different arguments than the held call")]
    ArgumentsChanged,
    #[error("the answer couldn't be verified: {0}")]
    BadAnswer(String),
    #[error("this approval isn't approved, or it already ran")]
    NotApproved,
    #[error("that job is off or its add-on was removed")]
    JobOff,
    #[error("background jobs are paused")]
    Paused,
    #[error("no \"Run it now?\" question is waiting for that job")]
    NoQuestion,
}

pub type Result<T> = std::result::Result<T, JobsError>;

pub(crate) fn parse_id(text: &str) -> Result<dn_core::Id> {
    uuid::Uuid::parse_str(text)
        .map(dn_core::Id)
        .map_err(|_| JobsError::Damaged(format!("id `{text}`")))
}

pub(crate) fn parse_plugin(text: &str) -> Result<PluginId> {
    PluginId::new(text).map_err(|_| JobsError::Damaged(format!("add-on id `{text}`")))
}

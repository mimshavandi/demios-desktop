//! What an add-on registers (ADR-0013 §2): tools, skills, background jobs and a settings
//! panel. Everything is declared once, in `Plugin::register`; nothing is added later.

use crate::error::{ErrorKind, PluginError};
use crate::host::Host;
use crate::id::check_name;
use crate::manifest::{Permission, PluginManifest, risk_floor};
use crate::schedule::{MissedRunRule, Schedule};
use crate::settings::SettingsPanel;
use crate::{BoxFuture, Id, RiskTier, ToolResult, Untrusted};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The longest a job may run, in seconds, whatever it declares.
pub const MAX_JOB_BUDGET_SECS: u64 = 60 * 60;

/// A tool the add-on offers, as the model and the approval prompt see it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDecl {
    /// The tool's name within the add-on, `[a-z][a-z0-9_]*`. Its capability id is
    /// `plugin.<add-on id>.<name>`.
    pub name: String,
    /// A short title for approval prompts and Settings.
    pub title: String,
    /// What the tool does, for the model and for the user.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: serde_json::Value,
    /// JSON Schema of structured output, if any.
    pub output_schema: Option<serde_json::Value>,
    /// The tier the add-on declares (at least [`risk_floor`], which is `read` for every
    /// tool, ADR-0013 §9.1). Its handler's `Host` is limited to this tier: a `read` tool
    /// can't write or reach the network, a `low_write` tool can write only the add-on's own
    /// data. The user can make the tool ask more often; that never widens its access.
    pub risk: RiskTier,
    /// An argument whose value makes a write idempotent, so a retry replays the first
    /// result instead of repeating the side effect.
    pub idempotency_key_field: Option<String>,
    /// The tool always needs a fresh approval (ADR-0013 §9.4), for tools that act for the
    /// user in a way that can't be taken back: sending a message, answering an invitation,
    /// paying. The host then offers no standing permission for it, always holds a job's
    /// call to it, and runs it only after a person allowed that one call. No setting can
    /// remove the mark. Set it with [`ToolDecl::requiring_approval`].
    #[serde(default)]
    pub approval_required: bool,
}

impl ToolDecl {
    /// A tool taking an object of arguments described by `input_schema`.
    pub fn new(
        name: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
        risk: RiskTier,
    ) -> Self {
        Self {
            name: name.into(),
            title: title.into(),
            description: description.into(),
            input_schema,
            output_schema: None,
            risk,
            idempotency_key_field: None,
            approval_required: false,
        }
    }

    /// Marks the tool as always needing a fresh approval (see
    /// [`ToolDecl::approval_required`]).
    pub fn requiring_approval(mut self) -> Self {
        self.approval_required = true;
        self
    }
}

/// What a tool handler gets for one call.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct ToolInput {
    /// The arguments. The model wrote them, so they are untrusted.
    pub arguments: Untrusted<serde_json::Value>,
    /// Cancelled when the user stops the run.
    pub cancel: CancelToken,
}

impl ToolInput {
    /// The input for one call (built by the host).
    pub fn new(arguments: serde_json::Value, cancel: CancelToken) -> Self {
        Self {
            arguments: Untrusted::new(arguments),
            cancel,
        }
    }
}

/// Runs calls of one tool. The `Host` it gets is limited to the tool's tier.
pub trait ToolHandler: Send + Sync {
    /// Runs one call.
    fn call<'a>(
        &'a self,
        host: &'a Host,
        input: ToolInput,
    ) -> BoxFuture<'a, Result<ToolResult, PluginError>>;
}

/// A background job, as the add-on declares it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobDecl {
    /// The job's id within the add-on, `[a-z][a-z0-9_]*`.
    pub id: String,
    /// Its name in Settings and in held-approval prompts.
    pub name: String,
    /// When it runs.
    pub schedule: Schedule,
    /// The missed-run rule the add-on proposes; the user can change it.
    pub missed_run: MissedRunRule,
    /// The longest one run may take, in seconds (at most [`MAX_JOB_BUDGET_SECS`]). The
    /// host stops the job when it runs out.
    pub budget_secs: u64,
    /// The job's tier (ADR-0013 §9.1). Its `Host` is limited like a tool handler's of the
    /// same tier: a `read` job can't write or reach the network, a `low_write` job can
    /// write only its own data. Set it with [`JobDecl::with_tier`].
    ///
    /// `None` keeps the access jobs had before tiers: read and write, the network, and
    /// tools up to `sensitive`. That allowance ends at the next major version (#300); the
    /// conformance kit warns about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<RiskTier>,
}

impl JobDecl {
    /// A job with the default missed-run rule (`catch_up_once`, fresh for `fresh_for_secs`).
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        schedule: Schedule,
        fresh_for_secs: u64,
        budget_secs: u64,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            schedule,
            missed_run: MissedRunRule::CatchUpOnce { fresh_for_secs },
            budget_secs,
            tier: None,
        }
    }

    /// Declares the job's tier (see [`JobDecl::tier`]).
    pub fn with_tier(mut self, tier: RiskTier) -> Self {
        self.tier = Some(tier);
        self
    }

    /// Proposes another missed-run rule.
    pub fn with_missed_run(mut self, rule: MissedRunRule) -> Self {
        self.missed_run = rule;
        self
    }
}

/// A cancel flag shared between the host and a running handler or job. The host sets it
/// when the user stops the run, pauses all jobs, the time budget ends, or data is deleted;
/// the host also stops waiting for the handler then.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that isn't cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels it (host side).
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// True once cancelled. A long job should check this between steps.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What a job knows about the run it is in.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct JobContext {
    /// The job's id.
    pub job_id: String,
    /// This run.
    pub run: Id,
    /// The scheduled time this run is for, Unix seconds.
    pub scheduled_for: i64,
    /// When the job last finished successfully, Unix seconds, if ever. A late run should
    /// cover everything since then ("since Tuesday 07:00").
    pub last_success: Option<i64>,
    /// True when this run catches up on missed runs.
    pub late: bool,
    /// How many runs this one covers (1 when it is on time).
    pub covers_runs: u64,
    /// When the time budget ends, Unix seconds.
    pub deadline: i64,
    /// Cancelled when the run must stop.
    pub cancel: CancelToken,
}

impl JobContext {
    /// The context of one run (built by the host).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        job_id: impl Into<String>,
        run: Id,
        scheduled_for: i64,
        last_success: Option<i64>,
        late: bool,
        covers_runs: u64,
        deadline: i64,
        cancel: CancelToken,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            run,
            scheduled_for,
            last_success,
            late,
            covers_runs,
            deadline,
            cancel,
        }
    }
}

/// Runs a background job.
pub trait JobHandler: Send + Sync {
    /// Runs once. A job reports to the user only through notifications.
    fn run<'a>(
        &'a self,
        host: &'a Host,
        ctx: &'a JobContext,
    ) -> BoxFuture<'a, Result<(), PluginError>>;
}

/// A skill, as `SKILL.md` text. The host parses it with the same parser and rules as the
/// user's own skills: `allowed-tools` only narrows, and it can't take a built-in skill's
/// name. Its source shows as the add-on's name.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDecl {
    /// The whole `SKILL.md`, front matter included.
    pub skill_md: String,
}

impl SkillDecl {
    /// A skill from its `SKILL.md` text.
    pub fn new(skill_md: impl Into<String>) -> Self {
        Self {
            skill_md: skill_md.into(),
        }
    }
}

/// A registered tool.
#[derive(Clone)]
pub struct RegisteredTool {
    /// `plugin.<add-on id>.<name>`.
    pub capability_id: String,
    /// The declaration.
    pub decl: ToolDecl,
    /// The handler.
    pub handler: Arc<dyn ToolHandler>,
}

/// A registered job.
#[derive(Clone)]
pub struct RegisteredJob {
    /// The declaration.
    pub decl: JobDecl,
    /// The handler.
    pub handler: Arc<dyn JobHandler>,
}

/// Everything one add-on registered.
#[derive(Clone, Default)]
pub struct Registrations {
    /// Its tools.
    pub tools: Vec<RegisteredTool>,
    /// Its skills.
    pub skills: Vec<SkillDecl>,
    /// Its background jobs.
    pub jobs: Vec<RegisteredJob>,
    /// Its settings panel, if any.
    pub settings: Option<SettingsPanel>,
}

/// Collects an add-on's registrations, checking each one as it comes.
pub struct Registrar {
    manifest: PluginManifest,
    floor: RiskTier,
    out: Registrations,
}

impl Registrar {
    /// A registrar for an add-on with this (already validated) manifest. Built by the host.
    pub fn new(manifest: &PluginManifest) -> Self {
        Self {
            floor: risk_floor(manifest),
            manifest: manifest.clone(),
            out: Registrations::default(),
        }
    }

    /// The lowest tier this add-on's tools may declare ([`risk_floor`]).
    pub fn risk_floor(&self) -> RiskTier {
        self.floor
    }

    /// Registers a tool and returns its capability id. Refuses a bad or repeated name, an
    /// empty description, a schema that isn't an object schema, and a tier below the floor.
    pub fn tool(
        &mut self,
        decl: ToolDecl,
        handler: Arc<dyn ToolHandler>,
    ) -> Result<String, PluginError> {
        check_name("tool name", &decl.name)?;
        if self.out.tools.iter().any(|t| t.decl.name == decl.name) {
            return Err(PluginError::invalid(format!(
                "The tool `{}` is registered twice",
                decl.name
            )));
        }
        if decl.title.trim().is_empty() || decl.description.trim().is_empty() {
            return Err(PluginError::invalid(format!(
                "The tool `{}` needs a title and a description",
                decl.name
            )));
        }
        if decl.input_schema.get("type").and_then(|t| t.as_str()) != Some("object") {
            return Err(PluginError::invalid(format!(
                "The tool `{}` needs an input schema of type `object`",
                decl.name
            )));
        }
        if decl.risk < self.floor {
            return Err(PluginError::invalid(format!(
                "The tool `{}` declares the `{}` tier, but {} holds permissions that make \
                 every tool at least `{}`",
                decl.name,
                tier_name(decl.risk),
                self.manifest.name,
                tier_name(self.floor)
            )));
        }
        if let Some(f) = &decl.idempotency_key_field {
            check_name("idempotency field", f)?;
        }
        let capability_id = self.manifest.id.tool_capability_id(&decl.name);
        self.out.tools.push(RegisteredTool {
            capability_id: capability_id.clone(),
            decl,
            handler,
        });
        Ok(capability_id)
    }

    /// Registers a skill. The host parses it when it loads the add-on.
    pub fn skill(&mut self, skill: SkillDecl) -> Result<(), PluginError> {
        if skill.skill_md.trim().is_empty() {
            return Err(PluginError::invalid("A skill can't be empty"));
        }
        self.out.skills.push(skill);
        Ok(())
    }

    /// Registers a background job. Needs the `jobs` permission in the manifest; the job is
    /// still off until the user turns it on.
    pub fn job(&mut self, decl: JobDecl, handler: Arc<dyn JobHandler>) -> Result<(), PluginError> {
        if !self.manifest.asks_for(Permission::Jobs) {
            return Err(PluginError::new(
                ErrorKind::PermissionDenied,
                format!(
                    "{} registers the job `{}` without asking for the `jobs` permission",
                    self.manifest.name, decl.id
                ),
            ));
        }
        check_name("job id", &decl.id)?;
        if self.out.jobs.iter().any(|j| j.decl.id == decl.id) {
            return Err(PluginError::invalid(format!(
                "The job `{}` is registered twice",
                decl.id
            )));
        }
        if decl.name.trim().is_empty() {
            return Err(PluginError::invalid(format!(
                "The job `{}` has no name",
                decl.id
            )));
        }
        decl.schedule.validate()?;
        decl.missed_run.validate()?;
        if decl.budget_secs == 0 || decl.budget_secs > MAX_JOB_BUDGET_SECS {
            return Err(PluginError::invalid(format!(
                "The job `{}` needs a time budget between 1 and {MAX_JOB_BUDGET_SECS} seconds",
                decl.id
            )));
        }
        self.out.jobs.push(RegisteredJob { decl, handler });
        Ok(())
    }

    /// Registers the settings panel. An add-on has at most one.
    pub fn settings(&mut self, panel: SettingsPanel) -> Result<(), PluginError> {
        if self.out.settings.is_some() {
            return Err(PluginError::invalid("An add-on has one settings panel"));
        }
        panel.validate()?;
        self.out.settings = Some(panel);
        Ok(())
    }

    /// What was registered (host side, after `Plugin::register` returned).
    pub fn finish(self) -> Registrations {
        self.out
    }
}

fn tier_name(t: RiskTier) -> &'static str {
    match t {
        RiskTier::Read => "read",
        RiskTier::LowWrite => "low_write",
        RiskTier::ExternalWrite => "external_write",
        RiskTier::Destructive => "destructive",
        RiskTier::Sensitive => "sensitive",
    }
}

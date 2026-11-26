//! The chat agent (T-SHELL-04): manifest + system prompt from `skills/`, model resolution
//! through `dn-providers`' resolver, and the tool list narrowed to what the manifest allows.

use crate::error::IpcError;
use crate::models::ModelRegistry;
use crate::secrets::KeyCheck;
use crate::settings::Settings;
use dn_agent::{AgentManifest, BoxFuture, PromptRegistry, ToolContext, ToolExecutor, ToolOutput};
use dn_core::CoreError;
use dn_core::chat::{ToolCall, ToolSpec};
use dn_core::model::{Capability, ModelRef, ModelSlot};
use dn_providers::{ModelResolver, resolver::ResolveInput};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const AGENT_ID: &str = "assistant.default";

/// The app's chat slot maps to this agent slot.
const CHAT_SLOT: &str = "primary";

/// Where `skills/` lives: `DEMIOS_SKILLS_DIR`, else the bundled resources, else (debug)
/// the repository copy.
pub fn skills_dir(resource_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEMIOS_SKILLS_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(r) = resource_dir {
        let p = r.join("skills");
        if p.is_dir() {
            return Some(p);
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    (cfg!(debug_assertions) && dev.is_dir()).then_some(dev)
}

/// The loaded agent. If `skills/` is missing or broken there is no stand-in: the agent
/// records why ([`Self::load_error`]), the UI says so, and chats refuse to start
/// ([`Self::ready`]) instead of answering without the assistant's instructions (#141).
pub struct Agent {
    pub manifest: AgentManifest,
    system_prompt: Option<String>,
    load_error: Option<String>,
}

impl Agent {
    pub fn load(skills: Option<&Path>) -> Self {
        let result = match skills {
            None => Err("the skills folder wasn't found".to_owned()),
            Some(dir) => Self::try_load(dir).map_err(|e| {
                eprintln!(
                    "could not load agent `{AGENT_ID}` from {}: {e}",
                    dir.display()
                );
                e
            }),
        };
        result.unwrap_or_else(|e| Self {
            load_error: Some(e),
            ..Self::empty()
        })
    }

    /// Why the agent didn't load, if it didn't.
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Fails while the agent isn't loaded, so no chat starts without its instructions.
    pub fn ready(&self) -> Result<(), IpcError> {
        match &self.load_error {
            None => Ok(()),
            Some(e) => Err(IpcError::new(
                "agent_unavailable",
                format!(
                    "Demios couldn't load its assistant instructions ({e}), so chats can't start. \
                     Reinstall Demios, or report a problem."
                ),
            )),
        }
    }

    fn try_load(dir: &Path) -> Result<Self, String> {
        let manifest =
            dn_agent::manifest::load_agent(&dir.join("agents").join(AGENT_ID).join("agent.yaml"))
                .map_err(|e| e.to_string())?;
        let registry = PromptRegistry::load_dir(dir).map_err(|errs| {
            errs.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let vars: HashMap<&str, String> =
            HashMap::from([("user_name", "the user".to_string()), ("today", today())]);
        let system_prompt = registry
            .load_prompt(&manifest.prompt)
            .and_then(|p| p.render(&vars))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            manifest,
            system_prompt: Some(system_prompt),
            load_error: None,
        })
    }

    /// An agent with no prompt and no tools. Only a starting value: tests use it, and
    /// [`crate::state::AppState::open_with`] replaces it with [`Self::load`]. It never stands
    /// in for an agent that failed to load.
    pub fn empty() -> Self {
        Self {
            load_error: None,
            manifest: AgentManifest {
                id: AGENT_ID.into(),
                version: 1,
                description: "Empty agent".into(),
                prompt: String::new(),
                model_slots: vec![ModelSlot {
                    slot_id: CHAT_SLOT.into(),
                    capability: Capability::Chat,
                    default_model: None,
                    user_configurable: true,
                }],
                skills: vec![],
                tools: vec![],
                policies: serde_json::from_value(serde_json::json!({})).expect("default policies"),
            },
            system_prompt: None,
        }
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    fn chat_slot(&self) -> ModelSlot {
        self.manifest.slot(CHAT_SLOT).cloned().unwrap_or(ModelSlot {
            slot_id: CHAT_SLOT.into(),
            capability: Capability::Chat,
            default_model: None,
            user_configurable: true,
        })
    }

    /// Which model answers: the user's chat choice, and only that (#219: an empty Chat slot
    /// is never filled by an automatic pick here; the first suitable model fills it once, see
    /// [`Settings::fill_slot`]). Never falls back past the chosen model. `keys` says which
    /// provider keys are saved; when the keychain didn't answer and a provider model could
    /// have been the answer, the error is the keychain's, not "add a key" (#200).
    pub fn resolve_model(
        &self,
        settings: &Settings,
        models: &ModelRegistry,
        keys: &KeyCheck,
    ) -> Result<ModelRef, IpcError> {
        let slot = self.chat_slot();
        let Some(preference) = settings.slot("chat") else {
            return Err(choose_chat_model_error());
        };
        let preference = Some(preference);
        ModelResolver::new(models.sources(keys))
            .resolve(ResolveInput {
                agent_id: &self.manifest.id,
                slot: &slot,
                request_override: None,
                slot_preference: preference.as_ref(),
            })
            .map(|r| r.model)
            .map_err(|e| {
                let byok_possible = preference
                    .as_ref()
                    .is_none_or(|m| matches!(m, ModelRef::Byok { .. }));
                match keys.status.error() {
                    Some(keychain) if byok_possible => keychain,
                    _ => model_error(e),
                }
            })
    }
}

/// No Chat model is chosen: the task asks for one, with a button to the picker (#219).
pub fn choose_chat_model_error() -> IpcError {
    IpcError {
        code: "model_choice_required",
        message: "Choose a model for Chat in Settings → Models.".into(),
        recovery: vec![dn_core::RecoveryAction::ChooseModelForSlot {
            agent_id: AGENT_ID.into(),
            slot_id: "primary".into(),
        }],
    }
}

/// A `CoreError` from model resolution or a backend, as an IPC error with its recovery
/// actions (each becomes a button in the UI).
pub fn model_error(e: CoreError) -> IpcError {
    match e {
        CoreError::ModelSourceRequired { recovery, .. } => IpcError {
            code: "model_source_required",
            message: "No chat model is set up. Choose one in Settings → Models.".into(),
            recovery,
        },
        CoreError::ModelUnavailable {
            user_message,
            recovery,
            ..
        } => IpcError {
            code: "model_unavailable",
            message: user_message,
            recovery,
        },
        CoreError::Capability { user_message, .. } => IpcError::new("capability", user_message),
        CoreError::Cancelled => IpcError::new("cancelled", "Cancelled."),
    }
}

/// The dn-tools executor, limited to the capabilities the agent manifest allows.
pub struct ManifestTools {
    pub executor: Arc<dn_tools::Executor>,
    pub manifest: AgentManifest,
    /// Servers still connecting: their tools are hidden until their review policy applies.
    pub settling: Arc<crate::tool_review::Settling>,
}

impl ManifestTools {
    fn allowed_names(&self) -> std::collections::HashSet<String> {
        self.executor
            .capabilities()
            .into_iter()
            .filter(|c| {
                !c.disabled
                    && !self.settling.contains(&c.server_id)
                    && !self.executor.is_stale(&c.server_id)
                    && self.manifest.allows_tool(&c.spec.id)
            })
            .map(|c| c.llm_name)
            .collect()
    }
}

impl ToolExecutor for ManifestTools {
    fn tools(&self) -> Vec<ToolSpec> {
        let allowed = self.allowed_names();
        self.executor
            .tool_specs()
            .into_iter()
            .filter(|s| allowed.contains(&s.name))
            .collect()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, CoreError>> {
        Box::pin(async move {
            if !self.allowed_names().contains(&call.name) {
                return Ok(ToolOutput::error(format!(
                    "The tool `{}` isn't available to this assistant.",
                    call.name
                )));
            }
            ToolExecutor::execute(&*self.executor, call, ctx).await
        })
    }
}

/// Today's date as `YYYY-MM-DD` (UTC), without a date crate.
fn today() -> String {
    let days = dn_store::now_ms() / 86_400_000;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::RecoveryAction;

    #[test]
    fn loads_the_repo_agent() {
        let agent = Agent::load(skills_dir(None).as_deref());
        assert_eq!(agent.manifest.id, AGENT_ID);
        let p = agent.system_prompt().expect("system prompt rendered");
        assert!(p.contains("helpful assistant"));
        assert!(!p.contains("{today}"));
        assert!(agent.manifest.allows_tool("mcp.filesystem.read_file"));
        assert!(!agent.manifest.allows_tool("mcp.shell.exec"));
    }

    #[test]
    fn date_is_iso() {
        let t = today();
        assert_eq!(t.len(), 10);
        assert!(t.starts_with("20"));
    }

    #[test]
    fn missing_or_broken_skills_stop_chats_with_a_reason() {
        for agent in [
            Agent::load(Some(Path::new("/definitely/not/here"))),
            Agent::load(None),
        ] {
            assert!(agent.system_prompt().is_none());
            assert!(agent.load_error().is_some());
            assert!(
                agent.manifest.tools.is_empty(),
                "no tools without the manifest"
            );
            let e = agent.ready().unwrap_err();
            assert_eq!(e.code, "agent_unavailable");
            assert!(
                e.message
                    .contains("couldn't load its assistant instructions")
            );
        }
        assert!(Agent::load(skills_dir(None).as_deref()).ready().is_ok());
    }

    #[test]
    fn resolves_user_choice_and_reports_missing_key() {
        let agent = Agent::load(skills_dir(None).as_deref());
        let keys = crate::secrets::Keys::memory();
        let check = |k: &crate::secrets::Keys| KeyCheck::run(k, ["anthropic"]);
        let models = ModelRegistry::default();
        let mut s = Settings::default();
        // Nothing chosen: the task asks for a choice, with a button to the picker (#219).
        let e = agent.resolve_model(&s, &models, &check(&keys)).unwrap_err();
        assert_eq!(e.code, "model_choice_required");
        assert!(!e.recovery.is_empty());
        // User picked a model whose key is missing: no fallback, a fix instead.
        s.slots
            .insert("chat".into(), "byok:anthropic:claude-sonnet-5-5".into());
        let e = agent.resolve_model(&s, &models, &check(&keys)).unwrap_err();
        assert_eq!(e.code, "model_unavailable");
        assert!(e.recovery.contains(&RecoveryAction::AddProviderKey));
        // With the key, it resolves to exactly that model.
        keys.set("anthropic", &crate::secrets::Secret::new("test-key"))
            .unwrap();
        let m = agent.resolve_model(&s, &models, &check(&keys)).unwrap();
        assert_eq!(m.to_string(), "byok:anthropic:claude-sonnet-5-5");
    }

    #[test]
    fn a_keychain_failure_is_reported_as_itself_not_as_a_missing_key() {
        use crate::secrets::{SecretError, tests::failing};
        let agent = Agent::load(skills_dir(None).as_deref());
        let models = ModelRegistry::default();
        let mut s = Settings::default();
        s.slots
            .insert("chat".into(), "byok:anthropic:claude-sonnet-5-5".into());
        let keys = failing(|| SecretError::Denied("locked".into()));
        let e = agent
            .resolve_model(&s, &models, &KeyCheck::run(&keys, ["anthropic"]))
            .unwrap_err();
        assert_eq!(e.code, "key_store_denied");
        assert!(!e.recovery.contains(&RecoveryAction::AddProviderKey));
        // A chosen model that isn't a provider's keeps its own error.
        s.slots.insert("chat".into(), "local:not-there".into());
        let e = agent
            .resolve_model(&s, &models, &KeyCheck::run(&keys, ["anthropic"]))
            .unwrap_err();
        assert_ne!(e.code, "key_store_denied");
    }
}

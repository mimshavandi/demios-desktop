//! The chat agent (T-SHELL-04): manifest + system prompt from `skills/`, model resolution
//! through `dn-providers`' resolver, and the tool list narrowed to what the manifest allows.

use crate::error::IpcError;
use crate::models::ModelRegistry;
use crate::secrets::Keys;
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

/// The loaded agent. Missing or broken skills fall back to a prompt-less default so chat
/// still works; the problem is logged.
pub struct Agent {
    pub manifest: AgentManifest,
    system_prompt: Option<String>,
}

impl Agent {
    pub fn load(skills: Option<&Path>) -> Self {
        let loaded = skills.and_then(|dir| match Self::try_load(dir) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!(
                    "could not load agent `{AGENT_ID}` from {}: {e}",
                    dir.display()
                );
                None
            }
        });
        loaded.unwrap_or_else(Self::fallback)
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
        })
    }

    fn fallback() -> Self {
        Self {
            manifest: AgentManifest {
                id: AGENT_ID.into(),
                version: 1,
                description: "Built-in fallback".into(),
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

    /// Which model answers: the user's chat choice, the manifest default, then the first
    /// ready source. Never falls back past a model the user named. Blocking (keychain).
    pub fn resolve_model(
        &self,
        settings: &Settings,
        models: &ModelRegistry,
        keys: &Keys,
    ) -> Result<ModelRef, IpcError> {
        let slot = self.chat_slot();
        let preference = settings.slot("chat");
        ModelResolver::new(models.sources(keys))
            .resolve(ResolveInput {
                agent_id: &self.manifest.id,
                slot: &slot,
                request_override: None,
                slot_preference: preference.as_ref(),
            })
            .map(|r| r.model)
            .map_err(model_error)
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
    fn missing_skills_fall_back() {
        let agent = Agent::load(Some(Path::new("/definitely/not/here")));
        assert!(agent.system_prompt().is_none());
        assert_eq!(agent.chat_slot().slot_id, CHAT_SLOT);
    }

    #[test]
    fn resolves_user_choice_and_reports_missing_key() {
        let agent = Agent::load(skills_dir(None).as_deref());
        let keys = Keys::memory();
        let models = ModelRegistry::default();
        let mut s = Settings::default();
        // Nothing configured, nothing ready.
        let e = agent.resolve_model(&s, &models, &keys).unwrap_err();
        assert_eq!(e.code, "model_source_required");
        // User picked a model whose key is missing: no fallback, a fix instead.
        s.slots
            .insert("chat".into(), "byok:anthropic:claude-sonnet-5-5".into());
        let e = agent.resolve_model(&s, &models, &keys).unwrap_err();
        assert_eq!(e.code, "model_unavailable");
        assert!(e.recovery.contains(&RecoveryAction::AddProviderKey));
        // With the key, it resolves to exactly that model.
        keys.set("anthropic", &crate::secrets::Secret::new("test-key"))
            .unwrap();
        let m = agent.resolve_model(&s, &models, &keys).unwrap();
        assert_eq!(m.to_string(), "byok:anthropic:claude-sonnet-5-5");
    }
}

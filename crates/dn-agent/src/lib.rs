//! dn-agent — the model-driven tool-use loop, agent/skill manifests and the prompt
//! registry (ADR-0005).
//!
//! The loop talks to models only through [`dn_core::chat::ChatModel`] and to tools only
//! through [`ToolExecutor`], so local backends, BYOK providers, MCP tools and builtin
//! skills all plug in the same way.
//!
//! - [`manifest`]: `agent.yaml` / `skill.yaml` / prompt `manifest.yaml` loaders, validated
//!   against `schemas/*.schema.json` (T-AGT-01).
//! - [`run_loop`]: streaming, tool execution, step limit, cancellation, token accounting
//!   (T-AGT-02).
//! - [`chat_skill`]: `SKILL.md` chat skills the user picks per chat (T-SKL-01).
//! - [`PromptRegistry`]: `load_prompt("id@v1")`, active versions, `{placeholder}`
//!   rendering (T-AGT-03).

mod agent_loop;
pub mod cancel;
pub mod chat_skill;
pub mod manifest;
pub mod prompt;
pub mod schema;
pub mod tool;

pub use agent_loop::{AgentEvent, LoopConfig, RunOutcome, StopReason, run_loop};
pub use cancel::CancelToken;
pub use chat_skill::ChatSkill;
pub use manifest::{AgentManifest, ManifestError, SkillKind, SkillManifest, tool_name};
pub use prompt::{Prompt, PromptError, PromptRegistry};
pub use tool::{BoxFuture, NoTools, ToolContext, ToolExecutor, ToolOutput};

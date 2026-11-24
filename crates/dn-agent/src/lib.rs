//! dn-agent — the model-driven tool-use loop, agent/skill manifests and the prompt
//! registry (ADR-0005).
//!
//! The loop talks to models only through [`dn_core::chat::ChatModel`] and to tools only
//! through [`ToolExecutor`], so local backends, BYOK providers, MCP tools and builtin
//! skills all plug in the same way.

pub mod cancel;
pub mod tool;

pub use cancel::CancelToken;
pub use tool::{BoxFuture, NoTools, ToolContext, ToolExecutor, ToolOutput};

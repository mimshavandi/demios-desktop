//! The tool boundary of the agent loop (ADR-0005, ADR-0006).
//!
//! `dn-agent` defines this trait; `dn-tools` implements it for MCP tools, builtin
//! capabilities and artifact tools (approval, preflight and the ledger live there).
//! The loop never sees MCP, approval UI or risk tiers — only this trait.

use crate::CancelToken;
use dn_core::CoreError;
use dn_core::chat::{ContentPart, ToolCall, ToolSpec};
use std::future::Future;
use std::pin::Pin;

/// Boxed, `Send` future, so [`ToolExecutor`] stays object-safe (`&dyn ToolExecutor`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Context for one tool call, passed by the agent loop.
#[derive(Clone, Debug)]
pub struct ToolContext {
    /// Agent manifest id running the loop, e.g. `assistant.default`.
    pub agent_id: String,
    /// Caller tag of the loop's model calls, e.g. `chat.reply`.
    pub caller: String,
    /// Conversation the call belongs to, if any (ledger / approval scope).
    pub conversation_id: Option<dn_core::Id>,
    /// Fresh UUIDv7 per agent run (one user turn). Scope for tool-call idempotency:
    /// key ledgers and replays on `(run_id, call.id)`, because model-provided call ids are
    /// not unique (local models reuse `call_0` or send "").
    pub run_id: dn_core::Id,
    /// 0-based model step that produced this call.
    pub step: u32,
    /// Fires when the user stops the run. Long-running tools and approval waits must honour it.
    pub cancel: CancelToken,
}

/// Result of a tool call, fed back to the model as a `Role::Tool` message.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub content: Vec<ContentPart>,
    /// The tool ran (or was refused) and failed in a way the model should see and may
    /// recover from (bad arguments, not found, user denied approval, ...).
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::Text { text: text.into() }],
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::Text { text: text.into() }],
            is_error: true,
        }
    }
}

/// Runs the tools a model asks for.
///
/// Contract:
/// - [`tools`](Self::tools) lists what the model may call this run; names are unique.
/// - [`execute`](Self::execute) may wait (e.g. for user approval) and must return
///   promptly with `Err(CoreError::Cancelled)` once `ctx.cancel` fires.
/// - Failures the model can react to are `Ok(ToolOutput { is_error: true, .. })`
///   (including unknown tool names); `Err` ends the whole agent run.
pub trait ToolExecutor: Send + Sync {
    fn tools(&self) -> Vec<ToolSpec>;

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, CoreError>>;
}

/// No tools: the loop degenerates to a single streamed model reply.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTools;

impl ToolExecutor for NoTools {
    fn tools(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, CoreError>> {
        Box::pin(async move { Ok(ToolOutput::error(format!("unknown tool `{}`", call.name))) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn is_object_safe_and_unknown_tools_are_model_visible_errors() {
        let exec: &dyn ToolExecutor = &NoTools;
        let ctx = ToolContext {
            agent_id: "test".into(),
            caller: "chat.reply".into(),
            conversation_id: None,
            run_id: dn_core::Id::new(),
            step: 0,
            cancel: CancelToken::new(),
        };
        let call = ToolCall {
            id: "c1".into(),
            name: "nope".into(),
            arguments: serde_json::json!({}),
        };
        let out = exec.execute(&call, &ctx).await.unwrap();
        assert!(out.is_error);
        assert!(exec.tools().is_empty());
    }
}

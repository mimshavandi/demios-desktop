//! In-process tools of first-party add-ons (ADR-0013 §2, §4).
//!
//! An add-on's tool is registered with [`Executor::add_plugin`](crate::Executor::add_plugin)
//! and then runs through **the same path as an MCP tool**: preflight, risk tier, the user's
//! per-tool choices, the approval gate, the side-effect ledger with idempotency, and
//! narrowing. Only the last step differs: instead of an MCP client, the executor calls the
//! tool's [`InProcessTool`] handler.
//!
//! When an add-on calls a tool itself, the host builds a [`CallContext`] with
//! [`CallContext::for_plugin`]: the run gets its own approval scope (`plugin:<id>:<run>`,
//! never shared with a chat), keeps every allowlist it inherited, and can't call a tool
//! above its tier. A background job's call runs at once only for tools the user set to run
//! without asking for that add-on; every other call is refused as
//! [`ToolErrorKind::Held`](crate::error::ToolErrorKind::Held) for the host to queue, and is
//! never approved on its own.

use crate::client::McpCallOutput;
use crate::error::ToolError;
use crate::executor::CallContext;
use dn_agent::BoxFuture;
use dn_core::capability::RiskTier;
use std::sync::Arc;

/// Runs calls of one add-on tool. The host implements it as a bridge to the add-on's
/// handler, giving the handler a `Host` limited to the tool's tier and to `ctx`'s
/// allowlists.
pub trait InProcessTool: Send + Sync {
    /// Runs one call. The executor has already checked and approved it; dropping the future
    /// cancels it.
    fn call<'a>(
        &'a self,
        ctx: &'a CallContext,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<McpCallOutput, ToolError>>;
}

/// An add-on tool, as the host registers it.
#[derive(Clone)]
pub struct PluginTool {
    /// The tool's name within the add-on, `[a-z][a-z0-9_]*`.
    pub name: String,
    /// Title for approval prompts and Settings.
    pub title: String,
    /// What it does, for the model and the user.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: serde_json::Value,
    /// JSON Schema of structured output, if any.
    pub output_schema: Option<serde_json::Value>,
    /// The tier the add-on declared.
    pub risk: RiskTier,
    /// The lowest tier the add-on's permissions allow (ADR-0013 §2). Neither the add-on nor
    /// a user override can put the tool below it.
    pub floor: RiskTier,
    /// An argument whose value makes a write idempotent.
    pub idempotency_key_field: Option<String>,
    /// Runs the calls.
    pub handler: Arc<dyn InProcessTool>,
}

impl std::fmt::Debug for PluginTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginTool")
            .field("name", &self.name)
            .field("risk", &self.risk)
            .field("floor", &self.floor)
            .finish_non_exhaustive()
    }
}

/// The executor's server id for an add-on's tools, `plugin:<id>`. MCP server ids can't
/// contain `:`, so it never equals one.
pub fn plugin_server_id(plugin_id: &str) -> String {
    format!("plugin:{plugin_id}")
}

/// True for `plugin:<id>` server ids.
pub fn is_plugin_server(server_id: &str) -> bool {
    server_id.starts_with("plugin:")
}

/// Checks an add-on id: dot-separated segments of `[a-z][a-z0-9]*` (ADR-0013 §3).
pub fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.split('.').all(|s| {
            let mut c = s.chars();
            c.next().is_some_and(|f| f.is_ascii_lowercase())
                && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Checks a tool name: `[a-z][a-z0-9_]*`, at most 64 bytes.
pub fn valid_tool_name(name: &str) -> bool {
    let mut c = name.chars();
    name.len() <= 64
        && c.next().is_some_and(|f| f.is_ascii_lowercase())
        && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The name the model sees for an add-on tool: `plugin__<id with . as _>__<name>`, with a
/// hash suffix when it would be longer than 64 characters. Add-on ids have no `_`, so the
/// mapping is one-to-one.
pub fn plugin_llm_name(plugin_id: &str, tool: &str) -> String {
    let name = format!("plugin__{}__{tool}", plugin_id.replace('.', "_"));
    if name.len() <= 64 {
        name
    } else {
        crate::capability::with_hash_suffix(&name, &format!("plugin.{plugin_id}.{tool}"))
    }
}

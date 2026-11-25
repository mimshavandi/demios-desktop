//! Narrowing a tool set to an allowlist (T-SKL-01: a chat skill's `allowed-tools`).
//!
//! [`Narrowed`] only ever removes tools. Every call it lets through goes to the inner
//! executor unchanged, so the approval gate, risk tiers and the user's per-tool choices
//! (T-SHELL-11) still decide what runs and what asks first. A pattern can't name an
//! approval or a risk, so an allowlist can't make a tool run without asking.

use dn_agent::{BoxFuture, ToolContext, ToolExecutor, ToolOutput};
use dn_core::CoreError;
use dn_core::chat::{ToolCall, ToolSpec};
use std::sync::Arc;

/// Capability-id patterns: an exact id (`artifact.create`) or a prefix ending in `*`
/// (`mcp.files.*`), the same syntax as an agent manifest's `tools`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolAllowlist {
    patterns: Vec<String>,
}

impl ToolAllowlist {
    /// Checks each pattern: lowercase letters, digits, `.`, `_` and `-`, with an optional
    /// trailing `*`. A lone `*` is allowed and means "whatever the assistant already has".
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, String> {
        let mut out = Vec::with_capacity(patterns.len());
        for p in patterns {
            let p = p.as_ref().trim();
            if !dn_agent::chat_skill::valid_tool_pattern(p) {
                return Err(format!(
                    "`{p}` isn't a tool pattern: use a tool id such as `artifact.create`, or a \
                     prefix ending in `*` such as `mcp.files.*`"
                ));
            }
            out.push(p.to_owned());
        }
        Ok(Self { patterns: out })
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Whether `capability_id` matches a pattern. An empty list allows nothing.
    pub fn allows(&self, capability_id: &str) -> bool {
        self.patterns.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => capability_id.starts_with(prefix),
            None => p == capability_id,
        })
    }
}

/// Maps a tool's model-facing name (`mcp__files__read`, `artifact_create`) to its
/// capability id (`mcp.files.read`, `artifact.create`). `None` means unknown: refused.
pub type CapabilityResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// `inner`, limited to the tools whose capability id the allowlist matches.
pub struct Narrowed {
    inner: Arc<dyn ToolExecutor>,
    allow: ToolAllowlist,
    resolve: CapabilityResolver,
}

impl Narrowed {
    pub fn new(
        inner: Arc<dyn ToolExecutor>,
        allow: ToolAllowlist,
        resolve: CapabilityResolver,
    ) -> Self {
        Self {
            inner,
            allow,
            resolve,
        }
    }

    fn permits(&self, tool_name: &str) -> bool {
        (self.resolve)(tool_name).is_some_and(|id| self.allow.allows(&id))
    }
}

impl ToolExecutor for Narrowed {
    fn tools(&self) -> Vec<ToolSpec> {
        self.inner
            .tools()
            .into_iter()
            .filter(|s| self.permits(&s.name))
            .collect()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, CoreError>> {
        Box::pin(async move {
            if !self.permits(&call.name) {
                return Ok(ToolOutput::error(format!(
                    "The tool `{}` isn't available with the skill chosen for this chat.",
                    call.name
                )));
            }
            self.inner.execute(call, ctx).await
        })
    }
}

impl crate::Executor {
    /// A [`CapabilityResolver`] for this executor's MCP tools, read live (servers that
    /// connect later resolve too).
    pub fn capability_resolver(self: &Arc<Self>) -> CapabilityResolver {
        let ex = Arc::clone(self);
        Arc::new(move |name| ex.resolve(name).map(|c| c.spec.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_exact_ids_and_prefixes() {
        let a = ToolAllowlist::new(&["artifact.create", "mcp.files.*"]).unwrap();
        assert!(a.allows("artifact.create"));
        assert!(!a.allows("artifact.update"));
        assert!(a.allows("mcp.files.read"));
        assert!(!a.allows("mcp.filesystem.read"));
        assert!(!a.allows("mcp.other.read"));
        assert!(
            !ToolAllowlist::new::<&str>(&[])
                .unwrap()
                .allows("artifact.create")
        );
        assert!(ToolAllowlist::new(&["*"]).unwrap().allows("mcp.any.thing"));
    }

    #[test]
    fn bad_patterns_are_refused() {
        for p in [
            "",
            " ",
            "Artifact.create",
            "mcp.*.read",
            "a b",
            "mcp/files",
            "*x",
        ] {
            assert!(ToolAllowlist::new(&[p]).is_err(), "{p:?}");
        }
    }
}

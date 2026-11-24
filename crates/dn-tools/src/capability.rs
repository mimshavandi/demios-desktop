//! Capability wrapper (T-CON-02): every MCP tool becomes a [`CapabilitySpec`] with id
//! `mcp.<server_id>.<tool>`, a risk tier seeded from the tool's annotations, user overrides,
//! and a [`Preflight`] evaluation.

use crate::client::{ConnectionStatus, McpTool, ToolHints};
use dn_core::capability::{ApprovalPolicy, CapabilitySource, CapabilitySpec, Preflight, RiskTier};
use serde::{Deserialize, Serialize};

pub fn capability_id(server_id: &str, tool_name: &str) -> String {
    format!("mcp.{server_id}.{tool_name}")
}

/// Seed a risk tier from MCP annotations.
///
/// Annotations are untrusted hints from the server (MCP spec). Rules:
/// - `readOnlyHint: true` → [`RiskTier::Read`]
/// - `destructiveHint: true` → [`RiskTier::Destructive`]
/// - otherwise `openWorldHint: false` → [`RiskTier::LowWrite`]; anything else (including the
///   spec default, open world) → [`RiskTier::ExternalWrite`].
///
/// A server that lies about `readOnlyHint` gets auto-run reads; that is why the user can
/// override, and why the threat model (`docs/security/mcp-tools.md`) treats servers as
/// trusted-by-install only.
pub fn seed_risk(h: &ToolHints) -> RiskTier {
    if h.read_only == Some(true) {
        RiskTier::Read
    } else if h.destructive == Some(true) {
        RiskTier::Destructive
    } else if h.open_world == Some(false) {
        RiskTier::LowWrite
    } else {
        RiskTier::ExternalWrite
    }
}

/// User-set overrides for one capability. Persisted by the app (settings), keyed by id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalPolicy>,
    /// Name an input field whose value makes this write idempotent (enables replay/retry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key_field: Option<String>,
    /// Turn the tool off entirely (preflight → `Blocked`).
    #[serde(default)]
    pub disabled: bool,
}

/// An MCP tool wrapped as a capability, with the effective approval policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpCapability {
    pub spec: CapabilitySpec,
    pub server_id: String,
    pub tool_name: String,
    pub title: Option<String>,
    pub hints: ToolHints,
    pub approval: ApprovalPolicy,
    pub disabled: bool,
}

impl McpCapability {
    pub fn from_tool(server_id: &str, tool: &McpTool, ov: Option<&CapabilityOverride>) -> Self {
        let seeded = seed_risk(&tool.hints);
        let risk = ov.and_then(|o| o.risk).unwrap_or(seeded);
        let approval = ov
            .and_then(|o| o.approval)
            .unwrap_or_else(|| risk.default_approval());
        Self {
            spec: CapabilitySpec {
                id: capability_id(server_id, &tool.name),
                source: CapabilitySource::Mcp {
                    server_id: server_id.to_owned(),
                },
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
                output_schema: tool.output_schema.clone(),
                risk,
                idempotency_key_field: ov.and_then(|o| o.idempotency_key_field.clone()),
            },
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            title: tool.title.clone(),
            hints: tool.hints,
            approval,
            disabled: ov.is_some_and(|o| o.disabled),
        }
    }

    /// Name safe for LLM tool-calling APIs (`^[a-zA-Z0-9_-]{1,64}$`): `mcp__<server>__<tool>`.
    pub fn llm_name(&self) -> String {
        llm_tool_name(&self.server_id, &self.tool_name)
    }

    /// Required top-level fields (from `input_schema.required`) absent or null in `args`.
    pub fn missing_fields(&self, args: &serde_json::Value) -> Vec<String> {
        let required = self
            .spec
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        required
            .iter()
            .filter_map(|f| f.as_str())
            .filter(|f| args.get(*f).is_none_or(|v| v.is_null()))
            .map(str::to_owned)
            .collect()
    }
}

pub fn llm_tool_name(server_id: &str, tool_name: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let mut name = format!("mcp__{}__{}", clean(server_id), clean(tool_name));
    name.truncate(64);
    name
}

/// Evaluate preflight in the documented order: blocked → connection → input → approval.
///
/// `approval_satisfied` is whether the approval gate would let this run without asking
/// (policy `auto`, or an `ask_once_per_conversation` grant already given).
pub fn preflight(
    cap: &McpCapability,
    args: &serde_json::Value,
    connection: &ConnectionStatus,
    approval_satisfied: bool,
) -> Preflight {
    if cap.disabled {
        return Preflight::Blocked {
            reason: "disabled by user".into(),
        };
    }
    match connection {
        ConnectionStatus::NeedsAuth => {
            return Preflight::NeedsConnection {
                server_id: cap.server_id.clone(),
            };
        }
        ConnectionStatus::Failed { .. } => {
            return Preflight::NeedsConnection {
                server_id: cap.server_id.clone(),
            };
        }
        ConnectionStatus::Connected | ConnectionStatus::Disconnected => {}
    }
    if !(args.is_object() || args.is_null()) {
        return Preflight::NeedsInput {
            missing_fields: vec![],
        };
    }
    let missing = cap.missing_fields(args);
    if !missing.is_empty() {
        return Preflight::NeedsInput {
            missing_fields: missing,
        };
    }
    if !approval_satisfied {
        return Preflight::ApprovalRequired;
    }
    Preflight::Ready
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(hints: ToolHints) -> McpTool {
        McpTool {
            name: "write_file".into(),
            title: None,
            description: "Write".into(),
            input_schema: serde_json::json!({"type":"object","required":["path","content"]}),
            output_schema: None,
            hints,
        }
    }

    #[test]
    fn risk_seeding() {
        let h = |r, d, o| ToolHints {
            read_only: r,
            destructive: d,
            idempotent: None,
            open_world: o,
        };
        assert_eq!(seed_risk(&h(Some(true), Some(true), None)), RiskTier::Read);
        assert_eq!(seed_risk(&h(None, Some(true), None)), RiskTier::Destructive);
        assert_eq!(
            seed_risk(&h(Some(false), Some(false), Some(false))),
            RiskTier::LowWrite
        );
        assert_eq!(seed_risk(&h(None, None, None)), RiskTier::ExternalWrite);
        assert_eq!(
            seed_risk(&h(None, Some(false), Some(true))),
            RiskTier::ExternalWrite
        );
    }

    #[test]
    fn overrides_and_ids() {
        let t = tool(ToolHints::default());
        let c = McpCapability::from_tool("fs", &t, None);
        assert_eq!(c.spec.id, "mcp.fs.write_file");
        assert_eq!(c.llm_name(), "mcp__fs__write_file");
        assert_eq!(c.spec.risk, RiskTier::ExternalWrite);
        assert_eq!(c.approval, ApprovalPolicy::AlwaysAsk);
        assert!(!c.spec.retry_safe());

        let ov = CapabilityOverride {
            risk: Some(RiskTier::LowWrite),
            idempotency_key_field: Some("path".into()),
            ..Default::default()
        };
        let c = McpCapability::from_tool("fs", &t, Some(&ov));
        assert_eq!(c.spec.risk, RiskTier::LowWrite);
        assert_eq!(c.approval, ApprovalPolicy::AskOncePerConversation);
        assert!(c.spec.retry_safe());
    }

    #[test]
    fn preflight_order() {
        let t = tool(ToolHints::default());
        let mut c = McpCapability::from_tool("fs", &t, None);
        let ok_args = serde_json::json!({"path": "a", "content": "b"});
        assert_eq!(
            preflight(
                &c,
                &serde_json::json!({"path": "a"}),
                &ConnectionStatus::Connected,
                true
            ),
            Preflight::NeedsInput {
                missing_fields: vec!["content".into()]
            }
        );
        assert_eq!(
            preflight(&c, &ok_args, &ConnectionStatus::Connected, false),
            Preflight::ApprovalRequired
        );
        assert_eq!(
            preflight(&c, &ok_args, &ConnectionStatus::Disconnected, true),
            Preflight::Ready
        );
        assert_eq!(
            preflight(&c, &ok_args, &ConnectionStatus::NeedsAuth, true),
            Preflight::NeedsConnection {
                server_id: "fs".into()
            }
        );
        c.disabled = true;
        assert!(matches!(
            preflight(&c, &ok_args, &ConnectionStatus::Connected, true),
            Preflight::Blocked { .. }
        ));
    }
}

//! Capability wrapper (T-CON-02): every MCP tool becomes a [`CapabilitySpec`] with id
//! `mcp.<server_id>.<tool>`, a risk tier seeded from the tool's annotations, user overrides,
//! and a [`Preflight`] evaluation.

use crate::client::{ConnectionStatus, McpTool, ToolHints};
use dn_core::capability::{ApprovalPolicy, CapabilitySource, CapabilitySpec, Preflight, RiskTier};
use serde::{Deserialize, Serialize};

pub fn capability_id(server_id: &str, tool_name: &str) -> String {
    format!("mcp.{server_id}.{tool_name}")
}

/// Seed a risk tier from MCP annotations. The most dangerous hint wins:
/// - `destructiveHint: true` → [`RiskTier::Destructive`] (even if `readOnlyHint` is also true)
/// - `readOnlyHint: true` → [`RiskTier::Read`]
/// - otherwise `openWorldHint: false` → [`RiskTier::LowWrite`]; anything else (including the
///   spec default, open world) → [`RiskTier::ExternalWrite`].
///
/// Annotations are untrusted hints (MCP spec). When the server is not trusted for annotations
/// (`trusted == false`, the default for HTTP servers, see
/// [`McpServerConfig::trusts_annotations`](crate::config::McpServerConfig::trusts_annotations)),
/// the seeded tier is never below [`RiskTier::LowWrite`]: nothing from that server auto-runs
/// unless the user overrides it.
pub fn seed_risk(h: &ToolHints, trusted: bool) -> RiskTier {
    let seeded = if h.destructive == Some(true) {
        RiskTier::Destructive
    } else if h.read_only == Some(true) {
        RiskTier::Read
    } else if h.open_world == Some(false) {
        RiskTier::LowWrite
    } else {
        RiskTier::ExternalWrite
    };
    if trusted {
        seeded
    } else {
        seeded.max(RiskTier::LowWrite)
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
    /// The user marked the tool "always needs a fresh approval" (ADR-0013 §9.4). It adds to
    /// a mark from the add-on or the Connect catalogue and never removes one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approval_required: bool,
}

/// The approval policy that applies, and the stored choice it ignores, if any.
///
/// A tool in the "approval required" class asks every time: a standing choice ("run
/// without asking", "ask once per chat") is ignored, and returned so Settings can say so.
pub fn effective_approval(
    approval_required: bool,
    risk: RiskTier,
    ov: Option<&CapabilityOverride>,
) -> (ApprovalPolicy, Option<ApprovalPolicy>) {
    let chosen = ov.and_then(|o| o.approval);
    if approval_required {
        let ignored = chosen.filter(|p| *p != ApprovalPolicy::AlwaysAsk);
        (ApprovalPolicy::AlwaysAsk, ignored)
    } else {
        (chosen.unwrap_or_else(|| risk.default_approval()), None)
    }
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
    /// A stored choice that doesn't apply because the tool always needs a fresh approval
    /// (ADR-0013 §9.4). Settings shows that it is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_ignored: Option<ApprovalPolicy>,
    pub disabled: bool,
    /// Name the model sees (`^[a-zA-Z0-9_-]{1,64}$`); unique within an [`Executor`](crate::Executor).
    pub llm_name: String,
}

impl McpCapability {
    /// `trust_annotations`: whether this server's annotations may seed a tier below
    /// `LowWrite` (see [`seed_risk`]).
    pub fn from_tool(
        server_id: &str,
        tool: &McpTool,
        ov: Option<&CapabilityOverride>,
        trust_annotations: bool,
    ) -> Self {
        let seeded = seed_risk(&tool.hints, trust_annotations);
        let risk = ov.and_then(|o| o.risk).unwrap_or(seeded);
        // MCP tools are in the class only by the user's mark for now; the Connect
        // catalogue's marks come with its entries' tool lists.
        let approval_required = ov.is_some_and(|o| o.approval_required);
        let (approval, approval_ignored) = effective_approval(approval_required, risk, ov);
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
                approval_required,
            },
            server_id: server_id.to_owned(),
            tool_name: tool.name.clone(),
            title: tool.title.clone(),
            hints: tool.hints,
            approval,
            approval_ignored,
            disabled: ov.is_some_and(|o| o.disabled),
            llm_name: llm_tool_name(server_id, &tool.name),
        }
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

/// 64-bit FNV-1a, for short stable suffixes (not security-relevant).
pub(crate) fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:08x}", (h >> 32) as u32 ^ h as u32)
}

/// Name safe for LLM tool-calling APIs (`^[a-zA-Z0-9_-]{1,64}$`): `mcp__<server>__<tool>`.
///
/// When sanitising or truncating loses information, a hash of the full capability id is
/// appended (`…_1a2b3c4d`) so distinct tools never share a name. Remaining collisions are
/// resolved by the executor (see `Executor::refresh`).
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
    let raw = format!("mcp__{server_id}__{tool_name}");
    let name = format!("mcp__{}__{}", clean(server_id), clean(tool_name));
    if name == raw && name.len() <= 64 {
        return name;
    }
    with_hash_suffix(&name, &capability_id(server_id, tool_name))
}

/// `base` cut to fit 64 chars with `_<hash(key)>` appended.
pub(crate) fn with_hash_suffix(base: &str, key: &str) -> String {
    let suffix = format!("_{}", short_hash(key));
    let mut name = base.to_owned();
    name.truncate(64 - suffix.len());
    name.push_str(&suffix);
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
        // Contradictory hints: the most dangerous one wins.
        assert_eq!(
            seed_risk(&h(Some(true), Some(true), None), true),
            RiskTier::Destructive
        );
        assert_eq!(seed_risk(&h(Some(true), None, None), true), RiskTier::Read);
        assert_eq!(
            seed_risk(&h(None, Some(true), None), true),
            RiskTier::Destructive
        );
        assert_eq!(
            seed_risk(&h(Some(false), Some(false), Some(false)), true),
            RiskTier::LowWrite
        );
        assert_eq!(
            seed_risk(&h(None, None, None), true),
            RiskTier::ExternalWrite
        );
        assert_eq!(
            seed_risk(&h(None, Some(false), Some(true)), true),
            RiskTier::ExternalWrite
        );
        // Untrusted server: hints can't seed below LowWrite.
        assert_eq!(
            seed_risk(&h(Some(true), None, None), false),
            RiskTier::LowWrite
        );
        assert_eq!(
            seed_risk(&h(None, Some(true), None), false),
            RiskTier::Destructive
        );
    }

    #[test]
    fn overrides_and_ids() {
        let t = tool(ToolHints::default());
        let c = McpCapability::from_tool("fs", &t, None, true);
        assert_eq!(c.spec.id, "mcp.fs.write_file");
        assert_eq!(c.llm_name, "mcp__fs__write_file");
        assert_eq!(c.spec.risk, RiskTier::ExternalWrite);
        assert_eq!(c.approval, ApprovalPolicy::AlwaysAsk);
        assert!(!c.spec.retry_safe());

        let ov = CapabilityOverride {
            risk: Some(RiskTier::LowWrite),
            idempotency_key_field: Some("path".into()),
            ..Default::default()
        };
        let c = McpCapability::from_tool("fs", &t, Some(&ov), true);
        assert_eq!(c.spec.risk, RiskTier::LowWrite);
        assert_eq!(c.approval, ApprovalPolicy::AskOncePerConversation);
        assert!(c.spec.retry_safe());
    }

    #[test]
    fn lossy_names_get_hash_suffix() {
        let a = llm_tool_name("srv", "a.b");
        let b = llm_tool_name("srv", "a/b");
        assert_ne!(a, b);
        assert!(a.starts_with("mcp__srv__a_b_"));
        let long = "x".repeat(80);
        let l1 = llm_tool_name("srv", &format!("{long}1"));
        let l2 = llm_tool_name("srv", &format!("{long}2"));
        assert_ne!(l1, l2);
        assert!(l1.len() <= 64 && l2.len() <= 64);
        assert!(
            l1.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }

    #[test]
    fn preflight_order() {
        let t = tool(ToolHints::default());
        let mut c = McpCapability::from_tool("fs", &t, None, true);
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

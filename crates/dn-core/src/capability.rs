//! Tool capabilities, risk and preflight (ADR-0006).
//! Ported concepts: Demios `AppCapability`, `capability_preflight.py`, `approval_policy.py`.

use serde::{Deserialize, Serialize};

/// Side-effect risk of a capability. Ordered from least to most dangerous.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    Read,
    LowWrite,
    ExternalWrite,
    Destructive,
    Sensitive,
}

impl RiskTier {
    /// Only reads run without asking by default (ADR-0006).
    pub fn default_approval(self) -> ApprovalPolicy {
        match self {
            Self::Read => ApprovalPolicy::Auto,
            Self::LowWrite => ApprovalPolicy::AskOncePerConversation,
            _ => ApprovalPolicy::AlwaysAsk,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    AlwaysAsk,
    AskOncePerConversation,
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapabilitySource {
    Builtin,
    Mcp {
        server_id: String,
    },
    Skill {
        skill_id: String,
    },
    /// A tool of a first-party add-on, run in-process through the same executor path as MCP
    /// tools (ADR-0013 §2). Its id is `plugin.<plugin_id>.<name>`.
    Plugin {
        plugin_id: String,
    },
}

/// Who asked for a tool call, shown on the approval prompt so an add-on's request never
/// looks like the model's own (ADR-0013 §4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalRequester {
    /// The model in a chat run.
    #[default]
    Model,
    /// An add-on, from one of its tool handlers or background jobs.
    Plugin {
        plugin_id: String,
        /// The add-on's name, shown as plain text.
        name: String,
        /// The background job's name, when the call comes from a job.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CapabilitySpec {
    /// Dotted, globally unique, e.g. `mcp.filesystem.write_file`.
    pub id: String,
    pub source: CapabilitySource,
    pub description: String,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub output_schema: Option<serde_json::Value>,
    pub risk: RiskTier,
    /// Input field whose value makes a write idempotent (enables safe retry/replay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key_field: Option<String>,
}

impl CapabilitySpec {
    /// Auto-retry is allowed only for reads and idempotent writes (Demios rule).
    pub fn retry_safe(&self) -> bool {
        self.risk == RiskTier::Read || self.idempotency_key_field.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Preflight {
    Ready,
    NeedsInput { missing_fields: Vec<String> },
    NeedsConnection { server_id: String },
    ScopeMissing { scopes: Vec<String> },
    ApprovalRequired,
    Blocked { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reads_are_auto() {
        assert_eq!(RiskTier::Read.default_approval(), ApprovalPolicy::Auto);
        assert_eq!(
            RiskTier::ExternalWrite.default_approval(),
            ApprovalPolicy::AlwaysAsk
        );
        assert!(RiskTier::Destructive > RiskTier::LowWrite);
    }
}

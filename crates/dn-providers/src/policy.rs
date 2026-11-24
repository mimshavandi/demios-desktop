//! Gateway policies shared by all providers.
//!
//! Reasoning: `Reasoning::Auto` turns thinking on only for callers that write prose the
//! user reads, and off whenever structured output is requested. Port of the Demios
//! caller-tagged policy (concept only, see the PR body for the source reference).

use dn_core::chat::{ChatRequest, Reasoning};

/// Callers whose output is prose for the user. Entries ending in `.*` match a prefix.
pub const DEFAULT_PROSE_CALLERS: &[&str] = &[
    "chat.reply",
    "chat.continue",
    "artifact.*",
    "document.*",
    "report.*",
    "twin.reply",
];

#[derive(Clone, Debug)]
pub struct ReasoningPolicy {
    pub prose_callers: Vec<String>,
}

impl Default for ReasoningPolicy {
    fn default() -> Self {
        Self {
            prose_callers: DEFAULT_PROSE_CALLERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl ReasoningPolicy {
    pub fn is_prose_caller(&self, caller: &str) -> bool {
        self.prose_callers
            .iter()
            .any(|p| match p.strip_suffix(".*") {
                Some(prefix) => caller
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.')),
                None => p == caller,
            })
    }

    /// Whether to ask a reasoning-capable model to think for this request. Structured
    /// output (`response_schema`) always turns thinking off, even with `Reasoning::On`.
    pub fn thinking(&self, req: &ChatRequest) -> bool {
        if req.response_schema.is_some() {
            return false;
        }
        match req.reasoning {
            Reasoning::On => true,
            Reasoning::Off => false,
            Reasoning::Auto => self.is_prose_caller(&req.caller),
        }
    }
}

/// Best-effort guess from the model id; `probe_capabilities` refines it per provider.
pub fn is_reasoning_model(model_id: &str) -> bool {
    let m = model_id.to_ascii_lowercase();
    let m = m.rsplit('/').next().unwrap_or(&m);
    // OpenAI o-series and gpt-5 family.
    if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.starts_with("gpt-5") {
        return true;
    }
    // Anthropic: 3.7 Sonnet and every 4+ generation model.
    if let Some(rest) = m.strip_prefix("claude-") {
        if rest.contains("3-7") {
            return true;
        }
        return rest
            .split('-')
            .filter_map(|p| p.parse::<u32>().ok())
            .next()
            .is_some_and(|major| major >= 4);
    }
    // Gemini 2.5+ thinks.
    if let Some(rest) = m.strip_prefix("gemini-") {
        let ver = rest.split('-').next().unwrap_or("");
        let mut it = ver.split('.');
        let major: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let minor: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        return major > 2 || (major == 2 && minor >= 5);
    }
    // Common open reasoning models behind OpenAI-compatible servers.
    ["deepseek-r1", "qwq", "qwen3", "gpt-oss", "magistral"]
        .iter()
        .any(|k| m.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dn_core::model::ModelRef;

    fn req(caller: &str, reasoning: Reasoning, schema: bool) -> ChatRequest {
        ChatRequest {
            model: ModelRef::Local {
                model_id: "x".into(),
            },
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            response_schema: schema.then(|| serde_json::json!({"type": "object"})),
            reasoning,
            caller: caller.into(),
        }
    }

    #[test]
    fn auto_thinks_only_for_prose_callers_without_schema() {
        let p = ReasoningPolicy::default();
        assert!(p.thinking(&req("chat.reply", Reasoning::Auto, false)));
        assert!(p.thinking(&req("artifact.update", Reasoning::Auto, false)));
        assert!(!p.thinking(&req("artifact", Reasoning::Auto, false)));
        assert!(!p.thinking(&req("artifactx.y", Reasoning::Auto, false)));
        assert!(!p.thinking(&req("chat.title", Reasoning::Auto, false)));
        assert!(!p.thinking(&req("chat.reply", Reasoning::Auto, true)));
        assert!(p.thinking(&req("chat.title", Reasoning::On, false)));
        assert!(!p.thinking(&req("chat.title", Reasoning::On, true)));
        assert!(!p.thinking(&req("chat.reply", Reasoning::Off, false)));
    }

    #[test]
    fn reasoning_model_guess() {
        for m in [
            "o3-mini",
            "gpt-5-mini",
            "claude-sonnet-4-5",
            "claude-3-7-sonnet-latest",
            "claude-opus-5-5",
            "gemini-2.5-flash",
            "models/gemini-3-pro",
            "qwen3:8b",
        ] {
            assert!(is_reasoning_model(m), "{m}");
        }
        for m in [
            "gpt-4o",
            "claude-3-5-haiku-latest",
            "gemini-2.0-flash",
            "llama3.3:70b",
        ] {
            assert!(!is_reasoning_model(m), "{m}");
        }
    }
}

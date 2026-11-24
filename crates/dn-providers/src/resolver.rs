//! Model resolver (T-PRV-02, ADR-0005): which model answers for an agent slot.
//!
//! Order: request override → agent slot preference (user choice, then the manifest's
//! `default_model`) → function default for the capability → first ready source →
//! `CoreError::ModelSourceRequired`.
//!
//! Rule (from Demios): **never silently fall back across sources.** If a level names a
//! model, that model is used or the call fails with a typed error that says how to fix
//! it. Only when no level names a model does the resolver pick the first ready source.

use crate::http::unavailable;
use dn_core::model::{Capability, ModelRef, ModelSlot, Readiness};
use dn_core::{CoreError, RecoveryAction};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A model the app knows about, with what it can do and whether it is usable now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEntry {
    pub model: ModelRef,
    pub capabilities: Vec<Capability>,
    pub readiness: Readiness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedFrom {
    RequestOverride,
    SlotPreference,
    SlotDefault,
    FunctionDefault,
    FirstReady,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub model: ModelRef,
    pub from: ResolvedFrom,
}

/// What to resolve: one slot of one agent, plus optional per-request and per-user choices.
#[derive(Clone, Copy, Debug)]
pub struct ResolveInput<'a> {
    pub agent_id: &'a str,
    pub slot: &'a ModelSlot,
    /// Explicit model for this one request (e.g. the model picker in the composer).
    pub request_override: Option<&'a ModelRef>,
    /// The user's saved choice for this agent slot.
    pub slot_preference: Option<&'a ModelRef>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelResolver {
    sources: Vec<SourceEntry>,
    function_defaults: HashMap<Capability, ModelRef>,
}

impl ModelResolver {
    pub fn new(sources: Vec<SourceEntry>) -> Self {
        Self {
            sources,
            function_defaults: HashMap::new(),
        }
    }

    /// App-wide default model for a capability (Settings → "default chat model").
    pub fn with_function_default(mut self, cap: Capability, model: ModelRef) -> Self {
        self.function_defaults.insert(cap, model);
        self
    }

    pub fn sources(&self) -> &[SourceEntry] {
        &self.sources
    }

    pub fn resolve(&self, input: ResolveInput<'_>) -> Result<Resolved, CoreError> {
        let cap = input.slot.capability;
        let levels = [
            (input.request_override, ResolvedFrom::RequestOverride),
            (input.slot_preference, ResolvedFrom::SlotPreference),
            (input.slot.default_model.as_ref(), ResolvedFrom::SlotDefault),
            (
                self.function_defaults.get(&cap),
                ResolvedFrom::FunctionDefault,
            ),
        ];
        if let Some((model, from)) = levels
            .into_iter()
            .find_map(|(m, from)| m.map(|m| (m, from)))
        {
            // The first level that names a model decides; no fallback past it.
            self.check(model, &input)?;
            return Ok(Resolved {
                model: model.clone(),
                from,
            });
        }
        self.sources
            .iter()
            .find(|s| s.readiness == Readiness::Ready && s.capabilities.contains(&cap))
            .map(|s| Resolved {
                model: s.model.clone(),
                from: ResolvedFrom::FirstReady,
            })
            .ok_or_else(|| CoreError::ModelSourceRequired {
                capability: cap,
                agent_id: input.agent_id.to_string(),
                slot_id: input.slot.slot_id.clone(),
                recovery: source_required_recovery(&input),
            })
    }

    fn check(&self, model: &ModelRef, input: &ResolveInput<'_>) -> Result<(), CoreError> {
        let cap = input.slot.capability;
        let entry = self.sources.iter().find(|s| &s.model == model);
        let mut recovery = Vec::new();
        let problem = match entry {
            None => {
                recovery.push(fix_for(model, cap));
                format!("`{model}` is not installed or connected")
            }
            Some(e) if !e.capabilities.contains(&cap) => format!("`{model}` can't do {cap:?}"),
            Some(e) => match e.readiness {
                Readiness::Ready | Readiness::Degraded => return Ok(()),
                Readiness::Missing => {
                    recovery.push(fix_for(model, cap));
                    format!("`{model}` is missing")
                }
                Readiness::Disabled => format!("`{model}` is disabled"),
            },
        };
        if input.slot.user_configurable {
            recovery.push(RecoveryAction::ChooseModelForSlot {
                agent_id: input.agent_id.to_string(),
                slot_id: input.slot.slot_id.clone(),
            });
        }
        Err(unavailable(
            format!("The model chosen for this task isn't available: {problem}."),
            format!(
                "resolver: agent `{}` slot `{}`: {problem}",
                input.agent_id, input.slot.slot_id
            ),
            recovery,
        ))
    }
}

fn fix_for(model: &ModelRef, cap: Capability) -> RecoveryAction {
    match model {
        ModelRef::Local { .. } => RecoveryAction::DownloadModel { capability: cap },
        ModelRef::Byok { .. } => RecoveryAction::AddProviderKey,
        ModelRef::Remote { endpoint_id, .. } => RecoveryAction::ReconnectServer {
            server_id: endpoint_id.clone(),
        },
    }
}

fn source_required_recovery(input: &ResolveInput<'_>) -> Vec<RecoveryAction> {
    let mut r = vec![
        RecoveryAction::DownloadModel {
            capability: input.slot.capability,
        },
        RecoveryAction::AddProviderKey,
    ];
    if input.slot.user_configurable {
        r.push(RecoveryAction::ChooseModelForSlot {
            agent_id: input.agent_id.to_string(),
            slot_id: input.slot.slot_id.clone(),
        });
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(default: Option<&str>) -> ModelSlot {
        ModelSlot {
            slot_id: "primary".into(),
            capability: Capability::Chat,
            default_model: default.map(|d| d.parse().unwrap()),
            user_configurable: true,
        }
    }

    fn src(m: &str, r: Readiness) -> SourceEntry {
        SourceEntry {
            model: m.parse().unwrap(),
            capabilities: vec![Capability::Chat],
            readiness: r,
        }
    }

    fn input<'a>(
        slot: &'a ModelSlot,
        ovr: Option<&'a ModelRef>,
        pref: Option<&'a ModelRef>,
    ) -> ResolveInput<'a> {
        ResolveInput {
            agent_id: "assistant",
            slot,
            request_override: ovr,
            slot_preference: pref,
        }
    }

    #[test]
    fn nothing_configured_is_model_source_required_with_recovery() {
        let r = ModelResolver::new(vec![]);
        let s = slot(None);
        let err = r.resolve(input(&s, None, None)).unwrap_err();
        match err {
            CoreError::ModelSourceRequired {
                capability,
                agent_id,
                slot_id,
                recovery,
            } => {
                assert_eq!(capability, Capability::Chat);
                assert_eq!(
                    (agent_id.as_str(), slot_id.as_str()),
                    ("assistant", "primary")
                );
                assert!(recovery.contains(&RecoveryAction::AddProviderKey));
                assert!(recovery.contains(&RecoveryAction::DownloadModel {
                    capability: Capability::Chat
                }));
                assert!(recovery.contains(&RecoveryAction::ChooseModelForSlot {
                    agent_id: "assistant".into(),
                    slot_id: "primary".into()
                }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn precedence_override_then_pref_then_default_then_function_then_first_ready() {
        let a: ModelRef = "byok:openai:gpt-5-mini".parse().unwrap();
        let b: ModelRef = "byok:anthropic:claude-sonnet-5-5".parse().unwrap();
        let c: ModelRef = "local:qwen3-8b".parse().unwrap();
        let d: ModelRef = "remote:ollama:llama3.3:70b".parse().unwrap();
        let all = [&a, &b, &c, &d].map(|m| src(&m.to_string(), Readiness::Ready));
        let r = ModelResolver::new(all.to_vec()).with_function_default(Capability::Chat, d.clone());
        let s = slot(Some("local:qwen3-8b"));
        let got = |ovr, pref, s: &ModelSlot| r.resolve(input(s, ovr, pref)).unwrap();
        assert_eq!(
            got(Some(&a), Some(&b), &s).from,
            ResolvedFrom::RequestOverride
        );
        assert_eq!(got(None, Some(&b), &s).model, b);
        assert_eq!(got(None, None, &s).from, ResolvedFrom::SlotDefault);
        assert_eq!(got(None, None, &slot(None)).model, d);
        let r2 = ModelResolver::new(vec![
            src("local:not-ready", Readiness::Missing),
            src("local:qwen3-8b", Readiness::Ready),
        ]);
        let first = r2.resolve(input(&slot(None), None, None)).unwrap();
        assert_eq!((first.model, first.from), (c, ResolvedFrom::FirstReady));
    }

    #[test]
    fn never_falls_back_when_the_chosen_model_is_not_ready() {
        let r = ModelResolver::new(vec![
            src("byok:openai:gpt-5-mini", Readiness::Missing),
            src("local:qwen3-8b", Readiness::Ready),
        ]);
        let pref: ModelRef = "byok:openai:gpt-5-mini".parse().unwrap();
        let s = slot(None);
        match r.resolve(input(&s, None, Some(&pref))).unwrap_err() {
            CoreError::ModelUnavailable { recovery, .. } => {
                assert_eq!(recovery[0], RecoveryAction::AddProviderKey);
                assert!(matches!(
                    recovery[1],
                    RecoveryAction::ChooseModelForSlot { .. }
                ));
            }
            other => panic!("{other:?}"),
        }
        // Unknown local model → download it.
        let ovr: ModelRef = "local:gone".parse().unwrap();
        match r.resolve(input(&s, Some(&ovr), None)).unwrap_err() {
            CoreError::ModelUnavailable { recovery, .. } => assert_eq!(
                recovery[0],
                RecoveryAction::DownloadModel {
                    capability: Capability::Chat
                }
            ),
            other => panic!("{other:?}"),
        }
    }
}

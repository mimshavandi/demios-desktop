//! Which `ChatModel`s the shell can call, and the catalog the settings screen shows.
//! Real backends (`dn-inference`, `dn-providers`) register here when they merge; until
//! then only the dev fake (T-SHELL-03) is callable.

use crate::secrets::{KeyStore, provider_account};
use crate::settings::{SLOTS, Settings, SlotInfo};
use dn_core::chat::ChatModel;
use dn_core::model::{Capability, ModelRef};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct ModelRegistry {
    models: HashMap<String, Arc<dyn ChatModel>>,
}

impl ModelRegistry {
    /// The registry this build ships with.
    pub fn with_builtin() -> Self {
        #[allow(unused_mut)]
        let mut r = Self::default();
        #[cfg(feature = "dev-fake-model")]
        r.register(
            crate::fake::FAKE_MODEL_REF.parse().expect("valid ref"),
            Arc::new(crate::fake::FakeChatModel::default()),
        );
        r
    }

    pub fn register(&mut self, model: ModelRef, backend: Arc<dyn ChatModel>) {
        self.models.insert(model.to_string(), backend);
    }

    pub fn get(&self, model: &ModelRef) -> Option<Arc<dyn ChatModel>> {
        self.models.get(&model.to_string()).cloned()
    }

    pub fn contains(&self, model: &str) -> bool {
        self.models.contains_key(model)
    }
}

/// A BYOK provider the settings screen offers a key field for.
pub struct ProviderDef {
    pub id: &'static str,
    pub name: &'static str,
    /// Model ids suggested once a key is set. Placeholder until `dn-providers` lists them.
    pub suggested_models: &'static [&'static str],
}

pub const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "anthropic",
        name: "Anthropic",
        suggested_models: &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
    },
    ProviderDef {
        id: "openai",
        name: "OpenAI",
        suggested_models: &["gpt-5", "gpt-5-mini"],
    },
    ProviderDef {
        id: "gemini",
        name: "Google Gemini",
        suggested_models: &["gemini-2.5-pro", "gemini-2.5-flash"],
    },
];

pub fn provider(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|p| p.id == id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    Local,
    Byok,
    Remote,
    Dev,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelOption {
    pub model_ref: String,
    pub label: String,
    pub source: ModelSource,
    pub capabilities: Vec<Capability>,
    /// Whether a backend in this build can answer it right now.
    pub available: bool,
    /// Why it is not available, if it is not.
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub has_key: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalModelPlaceholder {
    pub model_id: String,
    pub label: String,
    pub size: String,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelCatalog {
    pub slots: Vec<SlotInfo>,
    pub models: Vec<ModelOption>,
    pub providers: Vec<ProviderInfo>,
    /// Placeholder list until `dn-models` provides the real catalogue and downloads.
    pub local_models: Vec<LocalModelPlaceholder>,
    pub key_store: String,
    pub settings: Settings,
}

pub fn catalog(registry: &ModelRegistry, keys: &dyn KeyStore, settings: &Settings) -> ModelCatalog {
    let mut models = vec![];
    if registry.contains("local:dev-fake") {
        models.push(ModelOption {
            model_ref: "local:dev-fake".into(),
            label: "Dev fake model (canned replies)".into(),
            source: ModelSource::Dev,
            capabilities: vec![Capability::Chat],
            available: true,
            note: None,
        });
    }
    let providers: Vec<ProviderInfo> = PROVIDERS
        .iter()
        .map(|p| ProviderInfo {
            id: p.id.into(),
            name: p.name.into(),
            has_key: keys.has(&provider_account(p.id)).unwrap_or(false),
        })
        .collect();
    for (p, info) in PROVIDERS.iter().zip(&providers) {
        if !info.has_key {
            continue;
        }
        for m in p.suggested_models {
            let model_ref = format!("byok:{}:{m}", p.id);
            let available = registry.contains(&model_ref);
            models.push(ModelOption {
                label: format!("{} · {m}", p.name),
                available,
                note: (!available)
                    .then(|| "Provider backend not in this build yet (dn-providers).".into()),
                model_ref,
                source: ModelSource::Byok,
                capabilities: vec![Capability::Chat],
            });
        }
    }
    let local_models = [
        ("qwen3-8b-q4_k_m", "Qwen3 8B · Q4_K_M", "5.0 GB"),
        ("llama-3.2-3b-q4_k_m", "Llama 3.2 3B · Q4_K_M", "2.0 GB"),
        ("nomic-embed-text-v1.5", "Nomic Embed v1.5", "0.3 GB"),
    ]
    .into_iter()
    .map(|(id, label, size)| LocalModelPlaceholder {
        model_id: id.into(),
        label: label.into(),
        size: size.into(),
        note: "Downloads and local inference arrive with dn-models / dn-inference.".into(),
    })
    .collect();
    ModelCatalog {
        slots: SLOTS.to_vec(),
        models,
        providers,
        local_models,
        key_store: keys.describe().into(),
        settings: settings.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryKeyStore;

    #[test]
    fn byok_models_appear_only_with_a_key() {
        let keys = MemoryKeyStore::default();
        let reg = ModelRegistry::default();
        let c = catalog(&reg, &keys, &Settings::default());
        assert!(c.models.iter().all(|m| m.source != ModelSource::Byok));
        assert!(c.providers.iter().all(|p| !p.has_key));
        assert_eq!(c.slots.len(), 3);

        keys.set(&provider_account("anthropic"), "x").unwrap();
        let c = catalog(&reg, &keys, &Settings::default());
        let byok: Vec<_> = c
            .models
            .iter()
            .filter(|m| m.source == ModelSource::Byok)
            .collect();
        assert!(!byok.is_empty());
        assert!(byok.iter().all(|m| !m.available && m.note.is_some()));
        assert!(c.providers.iter().any(|p| p.id == "anthropic" && p.has_key));
    }

    #[cfg(feature = "dev-fake-model")]
    #[test]
    fn builtin_has_fake() {
        let reg = ModelRegistry::with_builtin();
        assert!(reg.get(&"local:dev-fake".parse().unwrap()).is_some());
    }
}

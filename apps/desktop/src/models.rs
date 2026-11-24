//! Which `ChatModel`s the shell can call, and the catalog the settings screen shows.
//!
//! - BYOK models (`byok:<provider>:<model>`) are `dn-providers` adapters built from the
//!   key in the OS keychain, cached per provider and dropped when the key changes.
//! - The dev fake (T-SHELL-03, debug builds) is a fixed entry.
//! - Local llama.cpp models join when `dn-inference` lands (#12).

use crate::secrets::Keys;
use crate::settings::{SLOTS, Settings, SlotInfo};
use dn_core::chat::ChatModel;
use dn_core::model::{Capability, ModelRef, Readiness};
use dn_core::{CoreError, RecoveryAction};
use dn_providers::{ProviderAdapter, ProviderConfig, ProviderKind, SourceEntry};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct ModelRegistry {
    fixed: HashMap<String, Arc<dyn ChatModel>>,
    providers: Mutex<Providers>,
    /// Catalogue models on this device (llama.cpp); `None` in tests that don't need them.
    pub local: Option<Arc<crate::local::LocalModels>>,
}

/// Cached adapters plus a generation per provider: a key change bumps it, so an adapter
/// built from the old key while the change happened is used once but never cached.
#[derive(Default)]
struct Providers {
    adapters: HashMap<String, Arc<dyn ProviderAdapter>>,
    generation: HashMap<String, u64>,
}

impl ModelRegistry {
    /// The registry this build ships with.
    pub fn with_builtin() -> Self {
        #[allow(unused_mut)]
        let mut r = Self::default();
        #[cfg(fake_model)]
        r.register(
            crate::fake::FAKE_MODEL_REF.parse().expect("valid ref"),
            Arc::new(crate::fake::FakeChatModel::default()),
        );
        r
    }

    /// Registers a fixed backend (dev fake, tests).
    pub fn register(&mut self, model: ModelRef, backend: Arc<dyn ChatModel>) {
        self.fixed.insert(model.to_string(), backend);
    }

    pub fn contains_fixed(&self, model: &str) -> bool {
        self.fixed.contains_key(model)
    }

    /// The backend for `model`. Blocking: may read the key from the keychain.
    pub fn get(&self, model: &ModelRef, keys: &Keys) -> Result<Arc<dyn ChatModel>, CoreError> {
        self.get_with(model, keys, dn_providers::build)
    }

    /// [`Self::get`] with the adapter constructor passed in (tests use it to change the key
    /// mid-build). The cache logic is the same code path either way.
    fn get_with(
        &self,
        model: &ModelRef,
        keys: &Keys,
        build: impl FnOnce(ProviderConfig) -> Result<Arc<dyn ProviderAdapter>, CoreError>,
    ) -> Result<Arc<dyn ChatModel>, CoreError> {
        if let Some(m) = self.fixed.get(&model.to_string()) {
            return Ok(Arc::clone(m));
        }
        if let (Some(local), ModelRef::Local { model_id }) = (&self.local, model) {
            // Called on a blocking thread (chat turns run in spawn_blocking): loading the
            // weights can take seconds.
            return tauri::async_runtime::block_on(local.chat_model(model_id));
        }
        let ModelRef::Byok { provider_id, .. } = model else {
            return Err(unavailable(
                format!("`{model}` isn't available in this build yet."),
                vec![RecoveryAction::ChooseModelForSlot {
                    agent_id: crate::agent::AGENT_ID.into(),
                    slot_id: "primary".into(),
                }],
            ));
        };
        let def = provider(provider_id)
            .ok_or_else(|| unavailable(format!("Unknown provider `{provider_id}`."), vec![]))?;
        let generation = {
            let p = lock(&self.providers);
            if let Some(a) = p.adapters.get(provider_id) {
                let a: Arc<dyn ChatModel> = a.clone();
                return Ok(a);
            }
            p.generation.get(provider_id).copied().unwrap_or(0)
        };
        let key = keys
            .get(provider_id)
            .map_err(|e| unavailable(crate::error::IpcError::from(e).message, vec![]))?
            .ok_or_else(|| {
                unavailable(
                    format!(
                        "Add your {} key in Settings → Models to use this model.",
                        def.name
                    ),
                    vec![RecoveryAction::AddProviderKey],
                )
            })?;
        let adapter = build(ProviderConfig::new(provider_id, def.kind).with_key(key))?;
        self.cache_if_current(provider_id, generation, &adapter);
        let a: Arc<dyn ChatModel> = adapter;
        Ok(a)
    }

    /// Caches `adapter` only if no key change happened since `generation` was read: an
    /// adapter built from an old key is used for this one call and never cached.
    fn cache_if_current(
        &self,
        provider_id: &str,
        generation: u64,
        adapter: &Arc<dyn ProviderAdapter>,
    ) {
        let mut p = lock(&self.providers);
        if p.generation.get(provider_id).copied().unwrap_or(0) == generation {
            p.adapters
                .insert(provider_id.to_string(), Arc::clone(adapter));
        }
    }

    /// Forget a cached adapter (its key was replaced or removed).
    pub fn invalidate(&self, provider_id: &str) {
        let mut p = lock(&self.providers);
        p.adapters.remove(provider_id);
        *p.generation.entry(provider_id.to_string()).or_default() += 1;
    }

    #[cfg(test)]
    fn cached(&self, provider_id: &str) -> bool {
        lock(&self.providers).adapters.contains_key(provider_id)
    }

    /// What the resolver may pick from. Blocking: checks the keychain.
    pub fn sources(&self, keys: &Keys) -> Vec<SourceEntry> {
        let mut out: Vec<SourceEntry> = self
            .fixed
            .keys()
            .filter_map(|r| r.parse().ok())
            .map(|model| SourceEntry {
                model,
                capabilities: vec![Capability::Chat],
                readiness: Readiness::Ready,
            })
            .collect();
        if let Some(local) = &self.local {
            for e in local.entries() {
                out.push(SourceEntry {
                    model: ModelRef::Local {
                        model_id: e.id.clone(),
                    },
                    capabilities: e.capabilities.clone(),
                    readiness: if local.is_ready(&e.id) {
                        Readiness::Ready
                    } else {
                        Readiness::Missing
                    },
                });
            }
        }
        for p in PROVIDERS {
            let ready = keys.has(p.id).unwrap_or(false);
            for m in p.suggested_models {
                out.push(SourceEntry {
                    model: ModelRef::Byok {
                        provider_id: p.id.into(),
                        model_id: (*m).into(),
                    },
                    capabilities: vec![Capability::Chat],
                    readiness: if ready {
                        Readiness::Ready
                    } else {
                        Readiness::Missing
                    },
                });
            }
        }
        out
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn unavailable(user_message: String, recovery: Vec<RecoveryAction>) -> CoreError {
    CoreError::ModelUnavailable {
        developer_message: user_message.clone(),
        user_message,
        recovery,
    }
}

/// A BYOK provider the settings screen offers a key field for.
pub struct ProviderDef {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: ProviderKind,
    /// Model ids offered once a key is set (`list_models` can extend this later).
    pub suggested_models: &'static [&'static str],
}

pub const PROVIDERS: &[ProviderDef] = &[
    ProviderDef {
        id: "anthropic",
        name: "Anthropic",
        kind: ProviderKind::Anthropic,
        suggested_models: &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
    },
    ProviderDef {
        id: "openai",
        name: "OpenAI",
        kind: ProviderKind::OpenAi,
        suggested_models: &["gpt-5", "gpt-5-mini"],
    },
    ProviderDef {
        id: "gemini",
        name: "Google Gemini",
        kind: ProviderKind::Gemini,
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
pub struct ModelCatalog {
    pub slots: Vec<SlotInfo>,
    pub models: Vec<ModelOption>,
    pub providers: Vec<ProviderInfo>,
    /// Catalogue models: download state and fit per row (live updates via
    /// `watch_local_models`).
    pub local_models: Vec<crate::local::LocalModelRow>,
    pub key_store: String,
    pub settings: Settings,
    /// The model chat uses now (the user's choice, or the one picked automatically when no
    /// chat model is chosen); `None` if nothing is usable.
    pub chat_model: Option<String>,
}

pub fn catalog(registry: &ModelRegistry, keys: &Keys, settings: &Settings) -> ModelCatalog {
    let mut models = vec![];
    if registry.contains_fixed("local:dev-fake") {
        models.push(ModelOption {
            model_ref: "local:dev-fake".into(),
            label: "Dev fake model (canned replies)".into(),
            source: ModelSource::Dev,
            capabilities: vec![Capability::Chat],
            available: true,
            note: None,
        });
    }
    if let Some(local) = &registry.local {
        for e in local.ready() {
            models.push(ModelOption {
                model_ref: format!("local:{}", e.id),
                label: format!("{} · {}", e.name, e.quant),
                source: ModelSource::Local,
                capabilities: e.capabilities.clone(),
                available: true,
                note: None,
            });
        }
    }
    let providers: Vec<ProviderInfo> = PROVIDERS
        .iter()
        .map(|p| ProviderInfo {
            id: p.id.into(),
            name: p.name.into(),
            has_key: keys.has(p.id).unwrap_or(false),
        })
        .collect();
    for (p, info) in PROVIDERS.iter().zip(&providers) {
        if !info.has_key {
            continue;
        }
        for m in p.suggested_models {
            models.push(ModelOption {
                label: format!("{} · {m}", p.name),
                available: true,
                note: None,
                model_ref: format!("byok:{}:{m}", p.id),
                source: ModelSource::Byok,
                capabilities: vec![Capability::Chat],
            });
        }
    }
    let local_models = registry
        .local
        .as_ref()
        .map(|l| l.rows())
        .unwrap_or_default();
    ModelCatalog {
        slots: SLOTS.to_vec(),
        models,
        providers,
        local_models,
        key_store: keys.description.into(),
        settings: settings.clone(),
        chat_model: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::Secret;

    #[test]
    fn byok_models_appear_only_with_a_key() {
        let keys = Keys::memory();
        let reg = ModelRegistry::default();
        let c = catalog(&reg, &keys, &Settings::default());
        assert!(c.models.iter().all(|m| m.source != ModelSource::Byok));
        assert!(c.providers.iter().all(|p| !p.has_key));
        assert_eq!(c.slots.len(), 3);

        keys.set("anthropic", &Secret::new("x")).unwrap();
        let c = catalog(&reg, &keys, &Settings::default());
        let byok: Vec<_> = c
            .models
            .iter()
            .filter(|m| m.source == ModelSource::Byok)
            .collect();
        assert!(!byok.is_empty());
        assert!(byok.iter().all(|m| m.available));
        assert!(c.providers.iter().any(|p| p.id == "anthropic" && p.has_key));
    }

    #[test]
    fn adapters_are_cached_per_key_and_never_from_an_old_key() {
        let keys = Keys::memory();
        let reg = ModelRegistry::default();
        let m: ModelRef = "byok:anthropic:claude-sonnet-5-5".parse().unwrap();
        let e = reg.get(&m, &keys).err().expect("no key yet");
        assert!(
            matches!(e, CoreError::ModelUnavailable { ref recovery, .. } if recovery.contains(&RecoveryAction::AddProviderKey))
        );
        keys.set("anthropic", &Secret::new("first")).unwrap();
        reg.get(&m, &keys).unwrap();
        assert!(reg.cached("anthropic"));
        reg.invalidate("anthropic");
        assert!(!reg.cached("anthropic"));
        // The key changes while the real get() builds the adapter: it's not cached.
        reg.get_with(&m, &keys, |cfg| {
            reg.invalidate("anthropic");
            dn_providers::build(cfg)
        })
        .unwrap();
        assert!(!reg.cached("anthropic"));
        reg.get(&m, &keys).unwrap();
        assert!(reg.cached("anthropic"));
    }

    #[cfg(fake_model)]
    #[test]
    fn builtin_has_fake() {
        let reg = ModelRegistry::with_builtin();
        assert!(
            reg.get(&"local:dev-fake".parse().unwrap(), &Keys::memory())
                .is_ok()
        );
    }
}

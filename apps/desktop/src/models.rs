//! Which `ChatModel`s the shell can call, and the catalog the settings screen shows.
//!
//! - BYOK models (`byok:<provider>:<model>`) are `dn-providers` adapters built from the
//!   key in the OS keychain, cached per provider and dropped when the key changes.
//! - OpenAI-compatible servers (`remote:<server>:<model>`, T-PRV-05) are `dn-providers`
//!   adapters for the servers saved in settings, with their optional key from the keychain.
//! - The dev fake (T-SHELL-03, debug builds) is a fixed entry.
//! - Local llama.cpp models (`local:<id>`) come from `dn-inference` (#12).

use crate::secrets::Keys;
use crate::settings::{RemoteServer, SLOTS, Settings, SlotInfo};
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
    /// Fixed models that can't make tool calls: they get no `artifact_*` tools and can't
    /// make artifacts.
    no_tools: std::collections::HashSet<String>,
    providers: Mutex<Providers>,
    /// The OpenAI-compatible servers in settings (kept in step by `AppState::save_settings`).
    remotes: Mutex<Vec<RemoteServer>>,
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

    /// Registers a fixed backend that supports tool calls (dev fake, tests).
    pub fn register(&mut self, model: ModelRef, backend: Arc<dyn ChatModel>) {
        self.no_tools.remove(&model.to_string());
        self.fixed.insert(model.to_string(), backend);
    }

    /// Registers a fixed backend that can't make tool calls.
    pub fn register_without_tools(&mut self, model: ModelRef, backend: Arc<dyn ChatModel>) {
        self.no_tools.insert(model.to_string());
        self.fixed.insert(model.to_string(), backend);
    }

    /// Whether `model` can make tool calls (and so gets the `artifact_*` tools). Exactly
    /// `tool_support(model) == ToolSupport::Supported`.
    pub fn supports_tools(&self, model: &ModelRef) -> bool {
        self.tool_support(model) == ToolSupport::Supported
    }

    /// Tool-call support for `model`:
    /// - BYOK providers support tool calls on every chat model;
    /// - the llama.cpp backend reports it for the models it loads;
    /// - an OpenAI-compatible server's model has what the server reports (Ollama
    ///   `/api/show`, LM Studio `/api/v0/models`, llama.cpp `/props`), or the user's answer
    ///   when the server doesn't say. Nothing is assumed.
    pub fn tool_support(&self, model: &ModelRef) -> ToolSupport {
        let key = model.to_string();
        if self.fixed.contains_key(&key) {
            return ToolSupport::from_bool(!self.no_tools.contains(&key));
        }
        match model {
            ModelRef::Byok { .. } => ToolSupport::Supported,
            ModelRef::Local { .. } => {
                ToolSupport::from_bool(self.local.as_ref().is_some_and(|l| l.tool_calls()))
            }
            ModelRef::Remote {
                endpoint_id,
                model_id,
            } => {
                let remotes = lock(&self.remotes);
                let m = remotes
                    .iter()
                    .find(|r| &r.id == endpoint_id)
                    .and_then(|r| r.model(model_id));
                match m.map(|m| (m.tool_calls, m.tool_calls_user)) {
                    Some((Some(reported), _)) => ToolSupport::from_bool(reported),
                    Some((None, Some(user))) => ToolSupport::from_bool(user),
                    _ => ToolSupport::Unknown,
                }
            }
        }
    }

    /// Replaces the known OpenAI-compatible servers (after every settings save). Cached
    /// adapters of servers whose address or family changed, or that were removed, are dropped.
    pub fn set_remote_servers(&self, servers: &[RemoteServer]) {
        let old = std::mem::replace(&mut *lock(&self.remotes), servers.to_vec());
        for o in old {
            let same = servers
                .iter()
                .any(|n| n.id == o.id && n.base_url == o.base_url && n.family == o.family);
            if !same {
                self.invalidate(&o.key_id());
            }
        }
    }

    pub fn remote_server(&self, id: &str) -> Option<RemoteServer> {
        lock(&self.remotes).iter().find(|r| r.id == id).cloned()
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
            let r = tauri::async_runtime::block_on(local.chat_model(model_id));
            // A Hub or cache file that fails to load may have changed or gone on disk: re-scan
            // so the list shows what is there now.
            if r.is_err() && local.is_external(model_id) {
                local.refresh_externals();
            }
            return r;
        }
        if let ModelRef::Remote { endpoint_id, .. } = model {
            return self.remote_adapter(endpoint_id, keys, build);
        }
        let ModelRef::Byok { provider_id, .. } = model else {
            return Err(unavailable(
                format!("`{model}` isn't available in this build yet."),
                vec![choose_chat_model()],
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

    /// The adapter for an OpenAI-compatible server, cached under its keychain id.
    fn remote_adapter(
        &self,
        server_id: &str,
        keys: &Keys,
        build: impl FnOnce(ProviderConfig) -> Result<Arc<dyn ProviderAdapter>, CoreError>,
    ) -> Result<Arc<dyn ChatModel>, CoreError> {
        let server = self.remote_server(server_id).ok_or_else(|| {
            unavailable(
                "This model's server was removed. Choose another model in Settings → Models."
                    .into(),
                vec![choose_chat_model()],
            )
        })?;
        let cache_id = server.key_id();
        let generation = {
            let p = lock(&self.providers);
            if let Some(a) = p.adapters.get(&cache_id) {
                let a: Arc<dyn ChatModel> = a.clone();
                return Ok(a);
            }
            p.generation.get(&cache_id).copied().unwrap_or(0)
        };
        let adapter = build(remote_config(&server, keys)?)?;
        self.cache_if_current(&cache_id, generation, &adapter);
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
                    // A model too big for this machine is never picked automatically.
                    readiness: if !local.is_ready(&e.id) {
                        Readiness::Missing
                    } else if local.wont_fit(&e.id) {
                        Readiness::Disabled
                    } else {
                        Readiness::Ready
                    },
                });
            }
        }
        for r in lock(&self.remotes).iter() {
            for m in &r.models {
                out.push(SourceEntry {
                    model: ModelRef::Remote {
                        endpoint_id: r.id.clone(),
                        model_id: m.id.clone(),
                    },
                    capabilities: vec![remote_capability(m)],
                    readiness: Readiness::Ready,
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

/// How sure the app is that a model makes tool calls (see [`ModelRegistry::tool_support`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSupport {
    /// The provider or server says so, or the user said so for a server that doesn't say.
    Supported,
    /// The server says the model has no tool calls (or the user said so).
    Unsupported,
    /// The server doesn't say and the user hasn't answered: treated as no tool calls.
    Unknown,
}

impl ToolSupport {
    fn from_bool(b: bool) -> Self {
        if b {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }
}

/// The adapter config for a saved server, with its key from the keychain if it has one.
/// Blocking (keychain).
pub fn remote_config(server: &RemoteServer, keys: &Keys) -> Result<ProviderConfig, CoreError> {
    let mut cfg = ProviderConfig::new(&server.id, ProviderKind::OpenAiCompatible)
        .with_base_url(&server.base_url);
    cfg.server_family = server.family;
    // No keychain at all: a server without a key still works (see `remote::stored_key`).
    let key = crate::remote::stored_key(keys, &server.key_id())
        .map_err(|e| unavailable(crate::error::IpcError::from(e).message, vec![]))?;
    Ok(match key {
        Some(k) => cfg.with_key(k),
        None => cfg,
    })
}

fn remote_capability(m: &crate::settings::RemoteModel) -> Capability {
    if m.embedding {
        Capability::Embedding
    } else {
        Capability::Chat
    }
}

fn choose_chat_model() -> RecoveryAction {
    RecoveryAction::ChooseModelForSlot {
        agent_id: crate::agent::AGENT_ID.into(),
        slot_id: "primary".into(),
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

/// An OpenAI-compatible server as the settings screen shows it.
#[derive(Clone, Debug, Serialize)]
pub struct RemoteServerView {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub has_key: bool,
    pub models: Vec<RemoteModelView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RemoteModelView {
    pub id: String,
    pub model_ref: String,
    pub embedding: bool,
    pub tool_support: ToolSupport,
    /// Whether the server reported tool support itself (else the user's answer counts).
    pub tool_support_reported: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelCatalog {
    pub slots: Vec<SlotInfo>,
    pub models: Vec<ModelOption>,
    pub providers: Vec<ProviderInfo>,
    /// OpenAI-compatible servers (T-PRV-05).
    pub remote_servers: Vec<RemoteServerView>,
    /// Catalogue models: download state and fit per row (live updates via
    /// `watch_local_models`).
    pub local_models: Vec<crate::local::LocalModelRow>,
    pub key_store: String,
    pub settings: Settings,
    /// The model chat uses now (the user's choice, or the one picked automatically when no
    /// chat model is chosen); `None` if nothing is usable.
    pub chat_model: Option<String>,
    /// Whether `chat_model` makes tool calls (without them the model can't make artifacts).
    pub chat_model_tools: bool,
    /// Where local models run on this computer (T-INF-24): shown above the local models, so
    /// running on the processor is a stated mode. `None` when local models are off.
    pub compute: Option<LocalCompute>,
}

/// [`dn_models::compute::Compute`] with its sentence for the UI.
#[derive(Clone, Debug, Serialize)]
pub struct LocalCompute {
    #[serde(flatten)]
    pub compute: dn_models::compute::Compute,
    pub description: String,
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
            let fits = !local.wont_fit(&e.id);
            models.push(ModelOption {
                model_ref: format!("local:{}", e.id),
                label: format!("{} · {}", e.name, e.quant),
                source: ModelSource::Local,
                capabilities: e.capabilities.clone(),
                available: fits,
                note: (!fits).then(|| "Won't fit in this computer's memory".into()),
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
    let mut remote_servers = Vec::new();
    for r in &settings.remote_servers {
        let mut views = Vec::new();
        for m in &r.models {
            let model = ModelRef::Remote {
                endpoint_id: r.id.clone(),
                model_id: m.id.clone(),
            };
            models.push(ModelOption {
                model_ref: model.to_string(),
                label: format!("{} · {}", r.name, m.id),
                source: ModelSource::Remote,
                capabilities: vec![remote_capability(m)],
                available: true,
                note: None,
            });
            views.push(RemoteModelView {
                id: m.id.clone(),
                model_ref: model.to_string(),
                embedding: m.embedding,
                tool_support: registry.tool_support(&model),
                tool_support_reported: m.tool_calls.is_some(),
            });
        }
        remote_servers.push(RemoteServerView {
            id: r.id.clone(),
            name: r.name.clone(),
            base_url: r.base_url.clone(),
            has_key: keys.has(&r.key_id()).unwrap_or(false),
            models: views,
        });
    }
    let local_models = registry
        .local
        .as_ref()
        .map(|l| l.rows())
        .unwrap_or_default();
    ModelCatalog {
        slots: SLOTS.iter().filter(|s| s.shown).cloned().collect(),
        models,
        providers,
        remote_servers,
        local_models,
        key_store: keys.description.into(),
        settings: settings.clone(),
        chat_model: None,
        chat_model_tools: false,
        compute: registry
            .local
            .as_ref()
            .and_then(|l| l.compute())
            .map(|c| LocalCompute {
                description: c.describe(),
                compute: c,
            }),
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
        // "fast" is valid but hidden until something reads it.
        let ids: Vec<_> = c.slots.iter().map(|s| s.slot_id).collect();
        assert_eq!(ids, ["chat", "embed"]);

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

    fn remote(id: &str, base_url: &str, models: Vec<crate::settings::RemoteModel>) -> RemoteServer {
        RemoteServer {
            id: id.into(),
            name: format!("{id} server"),
            base_url: base_url.into(),
            family: dn_providers::ServerFamily::Unknown,
            models,
        }
    }

    fn rm(id: &str, reported: Option<bool>, user: Option<bool>) -> crate::settings::RemoteModel {
        crate::settings::RemoteModel {
            id: id.into(),
            tool_calls: reported,
            tool_calls_user: user,
            embedding: false,
        }
    }

    #[test]
    fn remote_tool_support_is_what_the_server_or_user_says() {
        let reg = ModelRegistry::default();
        reg.set_remote_servers(&[remote(
            "lan",
            "http://192.168.1.2:11434/v1",
            vec![
                rm("tools", Some(true), None),
                rm("plain", Some(false), Some(true)),
                rm("quiet", None, None),
                rm("quiet-on", None, Some(true)),
                rm("quiet-off", None, Some(false)),
            ],
        )]);
        let ts = |m: &str| reg.tool_support(&format!("remote:lan:{m}").parse().unwrap());
        assert_eq!(ts("tools"), ToolSupport::Supported);
        // The server's "no" wins over a stale user answer.
        assert_eq!(ts("plain"), ToolSupport::Unsupported);
        assert_eq!(ts("quiet"), ToolSupport::Unknown);
        assert_eq!(ts("quiet-on"), ToolSupport::Supported);
        assert_eq!(ts("quiet-off"), ToolSupport::Unsupported);
        assert_eq!(ts("not-listed"), ToolSupport::Unknown);
        assert!(!reg.supports_tools(&"remote:gone:m".parse().unwrap()));
        assert!(reg.supports_tools(&"remote:lan:tools".parse().unwrap()));
        assert!(reg.supports_tools(&"byok:openai:gpt-5".parse().unwrap()));
    }

    #[test]
    fn remote_models_are_listed_and_a_removed_server_is_an_error() {
        let keys = Keys::memory();
        let reg = ModelRegistry::default();
        let settings = Settings {
            remote_servers: vec![remote(
                "box",
                "http://localhost:11434/v1",
                vec![rm("qwen3:8b", Some(true), None), {
                    let mut e = rm("nomic-embed-text", Some(false), None);
                    e.embedding = true;
                    e
                }],
            )],
            ..Default::default()
        };
        reg.set_remote_servers(&settings.remote_servers);
        let c = catalog(&reg, &keys, &settings);
        let refs: Vec<_> = c
            .models
            .iter()
            .filter(|m| m.source == ModelSource::Remote)
            .map(|m| (m.model_ref.as_str(), m.capabilities.clone()))
            .collect();
        assert_eq!(
            refs,
            [
                ("remote:box:qwen3:8b", vec![Capability::Chat]),
                ("remote:box:nomic-embed-text", vec![Capability::Embedding]),
            ]
        );
        assert_eq!(
            c.remote_servers[0].models[0].tool_support,
            ToolSupport::Supported
        );
        assert!(!c.remote_servers[0].has_key);
        assert!(reg.sources(&keys).iter().any(
            |s| s.model.to_string() == "remote:box:qwen3:8b" && s.readiness == Readiness::Ready
        ));
        // The adapter is built (no request yet) and cached under the server's key id.
        reg.get(&"remote:box:qwen3:8b".parse().unwrap(), &keys)
            .unwrap();
        assert!(reg.cached("remote.box"));
        // Moving the server drops the cached adapter; removing it makes the model an error.
        reg.set_remote_servers(&[remote("box", "http://localhost:1234/v1", vec![])]);
        assert!(!reg.cached("remote.box"));
        reg.set_remote_servers(&[]);
        let e = reg
            .get(&"remote:box:qwen3:8b".parse().unwrap(), &keys)
            .err()
            .unwrap();
        assert!(
            matches!(e, CoreError::ModelUnavailable { ref user_message, .. } if user_message.contains("removed"))
        );
    }

    /// One canned SSE answer from a local "server"; returns its base URL and the request.
    async fn one_shot_server(
        sse: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", l.local_addr().unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = l.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len: usize = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                sse.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(sse.as_bytes()).await.unwrap();
        });
        (base, rx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_remote_model_streams_text_and_tool_calls_with_its_key() {
        use dn_core::chat::{ChatMessage, ChatRequest, Role, StreamEvent};
        let sse = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"clock__now\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (base, req) = one_shot_server(sse).await;
        let keys = Keys::memory();
        keys.set(
            "remote.lan",
            &crate::secrets::Secret::new("sk-test-not-real"),
        )
        .unwrap();
        let reg = ModelRegistry::default();
        reg.set_remote_servers(&[remote("lan", &base, vec![rm("m1", Some(true), None)])]);
        let model: ModelRef = "remote:lan:m1".parse().unwrap();
        let backend = tokio::task::spawn_blocking({
            let keys = keys.clone();
            let model = model.clone();
            move || reg.get(&model, &keys)
        })
        .await
        .unwrap()
        .unwrap();
        let mut stream = backend.chat(ChatRequest {
            model,
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![dn_core::chat::ToolSpec {
                name: "clock__now".into(),
                description: "time".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            max_tokens: None,
            temperature: None,
            response_schema: None,
            reasoning: Default::default(),
            caller: "chat.reply".into(),
        });
        let mut text = String::new();
        let mut calls = vec![];
        while let Some(ev) = dn_providers::next_event(&mut stream).await {
            match ev.unwrap() {
                StreamEvent::TextDelta { text: t } => text.push_str(&t),
                StreamEvent::ToolCall { call } => calls.push(call.name),
                _ => {}
            }
        }
        assert_eq!(text, "Hello");
        assert_eq!(calls, ["clock__now"]);
        let raw = req.await.unwrap();
        assert!(raw.starts_with("POST /v1/chat/completions"), "{raw}");
        assert!(
            raw.to_ascii_lowercase()
                .contains("authorization: bearer sk-test-not-real")
        );
        assert!(raw.contains("\"stream\":true"), "streaming is always on");
        assert!(
            raw.contains("\"tools\""),
            "tools are sent as real tool definitions"
        );
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

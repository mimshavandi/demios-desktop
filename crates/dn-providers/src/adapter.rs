//! Provider configuration and the adapter trait every BYOK provider implements.

use crate::http::unavailable;
use crate::policy::{ReasoningPolicy, is_reasoning_model};
use crate::util::BoxFuture;
use crate::{anthropic, gemini, openai};
use dn_core::chat::{ChatModel, ChatRequest, ContentPart};
use dn_core::model::ModelRef;
use dn_core::{CoreError, RecoveryAction};
use dn_secrets::{AsyncSecrets, Secret, SecretId, SecretStore};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini API (Generative Language, `v1beta`).
    Gemini,
    /// Any server speaking OpenAI Chat Completions: Ollama, LM Studio, vLLM, OpenRouter, ...
    OpenAiCompatible,
}

impl ProviderKind {
    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Anthropic => Some("https://api.anthropic.com/v1"),
            Self::Gemini => Some("https://generativelanguage.googleapis.com/v1beta"),
            Self::OpenAiCompatible => None,
        }
    }

    /// Hosted providers need a key; local OpenAI-compatible servers usually don't.
    pub fn requires_key(self) -> bool {
        !matches!(self, Self::OpenAiCompatible)
    }
}

/// Everything needed to talk to one provider account or endpoint.
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    /// The id used in `byok:<provider_id>:<model>` / `remote:<provider_id>:<model>` and
    /// as the keychain id (`SecretId::provider(provider_id)`).
    pub provider_id: String,
    pub kind: ProviderKind,
    /// Overrides [`ProviderKind::default_base_url`]; required for `OpenAiCompatible`.
    pub base_url: Option<String>,
    pub api_key: Option<Secret>,
    pub reasoning: ReasoningPolicy,
    /// Thinking budget for providers that take one (Anthropic, Gemini).
    pub thinking_budget_tokens: u32,
    pub connect_timeout: Duration,
}

impl ProviderConfig {
    pub fn new(provider_id: impl Into<String>, kind: ProviderKind) -> Self {
        Self {
            provider_id: provider_id.into(),
            kind,
            base_url: None,
            api_key: None,
            reasoning: ReasoningPolicy::default(),
            thinking_budget_tokens: 2048,
            connect_timeout: Duration::from_secs(15),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    pub fn with_key(mut self, key: Secret) -> Self {
        self.api_key = Some(key);
        self
    }

    /// Loads the key from the keychain (`provider/<provider_id>`). A missing key for a
    /// hosted provider is `ModelUnavailable` with `AddProviderKey`.
    ///
    /// Blocking: from async code use [`with_key_from_async`](Self::with_key_from_async)
    /// (the Linux keychain backend panics inside a Tokio runtime).
    pub fn with_key_from(self, store: &dyn SecretStore) -> Result<Self, CoreError> {
        let key = store.get(&SecretId::provider(&self.provider_id));
        self.apply_key(key)
    }

    /// [`with_key_from`](Self::with_key_from) for async callers (runs on the blocking pool).
    pub async fn with_key_from_async(self, store: &AsyncSecrets) -> Result<Self, CoreError> {
        let key = store.get(SecretId::provider(&self.provider_id)).await;
        self.apply_key(key)
    }

    fn apply_key(
        mut self,
        key: Result<Option<Secret>, dn_secrets::SecretError>,
    ) -> Result<Self, CoreError> {
        let key = key.map_err(|e| {
            unavailable(
                "Couldn't read the API key from the system keychain.",
                format!("{}: {e}", self.provider_id),
                vec![RecoveryAction::AddProviderKey],
            )
        })?;
        if key.is_none() && self.kind.requires_key() {
            return Err(missing_key(&self.provider_id));
        }
        self.api_key = key;
        Ok(self)
    }

    pub(crate) fn base_url(&self) -> Result<String, CoreError> {
        self.base_url
            .as_deref()
            .or(self.kind.default_base_url())
            .map(|u| u.trim_end_matches('/').to_string())
            .ok_or_else(|| {
                unavailable(
                    format!("Set the server URL for {}.", self.provider_id),
                    format!(
                        "{}: OpenAI-compatible provider without base_url",
                        self.provider_id
                    ),
                    vec![],
                )
            })
    }
}

pub(crate) fn missing_key(provider_id: &str) -> CoreError {
    unavailable(
        format!("Add an API key for {provider_id} to use its models."),
        format!("{provider_id}: no API key configured"),
        vec![RecoveryAction::AddProviderKey],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Provider-reported methods, e.g. Gemini `generateContent` / `embedContent`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCaps {
    pub chat: bool,
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
    pub embedding: bool,
    pub context_window: Option<u32>,
}

/// A BYOK provider: streams chat (via [`ChatModel`]) and can check itself.
pub trait ProviderAdapter: ChatModel {
    fn provider_id(&self) -> &str;
    fn kind(&self) -> ProviderKind;
    /// Checks the key/endpoint with a cheap authenticated call.
    fn validate(&self) -> BoxFuture<'_, Result<(), CoreError>> {
        Box::pin(async move { self.list_models().await.map(|_| ()) })
    }
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, CoreError>>;
    /// What a model can do. Uses provider metadata where it exists, otherwise the model id.
    fn probe_capabilities<'a>(
        &'a self,
        model_id: &'a str,
    ) -> BoxFuture<'a, Result<ModelCaps, CoreError>> {
        Box::pin(async move {
            let models = self.list_models().await?;
            let info = models.iter().find(|m| m.id == model_id);
            if info.is_none() && self.kind() != ProviderKind::OpenAiCompatible {
                return Err(unavailable(
                    format!("{} doesn't offer `{model_id}`.", self.provider_id()),
                    format!(
                        "{}: model `{model_id}` not in list_models",
                        self.provider_id()
                    ),
                    vec![],
                ));
            }
            Ok(guess_caps(self.kind(), model_id, info))
        })
    }
}

pub(crate) fn guess_caps(
    kind: ProviderKind,
    model_id: &str,
    info: Option<&ModelInfo>,
) -> ModelCaps {
    let m = model_id.to_ascii_lowercase();
    let embedding =
        m.contains("embed") || info.is_some_and(|i| i.methods.iter().any(|x| x == "embedContent"));
    let chat = !embedding
        && ![
            "tts",
            "whisper",
            "dall-e",
            "moderation",
            "imagen",
            "transcribe",
        ]
        .iter()
        .any(|k| m.contains(k));
    let vision = chat
        && match kind {
            ProviderKind::Anthropic | ProviderKind::Gemini => true,
            ProviderKind::OpenAi => {
                m.starts_with("gpt-4o")
                    || m.starts_with("gpt-4.1")
                    || m.starts_with("gpt-5")
                    || m.starts_with("o")
            }
            ProviderKind::OpenAiCompatible => ["vision", "llava", "-vl", "gemma3"]
                .iter()
                .any(|k| m.contains(k)),
        };
    ModelCaps {
        chat,
        tools: chat,
        vision,
        reasoning: chat && is_reasoning_model(model_id),
        embedding,
        context_window: info.and_then(|i| i.context_window),
    }
}

/// Builds the adapter for `cfg.kind`.
pub fn build(cfg: ProviderConfig) -> Result<Arc<dyn ProviderAdapter>, CoreError> {
    if cfg.kind.requires_key() && cfg.api_key.is_none() {
        return Err(missing_key(&cfg.provider_id));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(cfg.connect_timeout)
        .build()
        .map_err(|e| unavailable("Couldn't start the HTTP client.", e.to_string(), vec![]))?;
    let base = cfg.base_url()?;
    let inner = Arc::new(Inner { cfg, client, base });
    Ok(match inner.cfg.kind {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => Arc::new(openai::OpenAi(inner)),
        ProviderKind::Anthropic => Arc::new(anthropic::Anthropic(inner)),
        ProviderKind::Gemini => Arc::new(gemini::Gemini(inner)),
    })
}

/// Shared state of every adapter.
#[derive(Debug)]
pub(crate) struct Inner {
    pub cfg: ProviderConfig,
    pub client: reqwest::Client,
    pub base: String,
}

impl Inner {
    pub fn id(&self) -> &str {
        &self.cfg.provider_id
    }

    pub fn key(&self) -> Option<&str> {
        self.cfg.api_key.as_ref().map(Secret::expose)
    }

    /// The model id this request targets. A ref for another provider is an error, never a
    /// silent re-route.
    pub fn model_id(&self, req: &ChatRequest) -> Result<String, CoreError> {
        match &req.model {
            ModelRef::Byok {
                provider_id,
                model_id,
            }
            | ModelRef::Remote {
                endpoint_id: provider_id,
                model_id,
            } if provider_id == &self.cfg.provider_id => Ok(model_id.clone()),
            other => Err(unavailable(
                "This model isn't served by the selected provider.",
                format!("{}: got request for `{other}`", self.cfg.provider_id),
                vec![],
            )),
        }
    }
}

/// Text of a message; images are not sent yet (the attachment store is wired in later).
pub(crate) fn text_of(parts: &[ContentPart]) -> String {
    let mut out = String::new();
    for p in parts {
        match p {
            ContentPart::Text { text } => out.push_str(text),
            ContentPart::Image { mime_type, .. } => {
                out.push_str(&format!("[image attachment ({mime_type}) not sent]"))
            }
        }
    }
    out
}

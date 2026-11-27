//! Types for loading and describing models (ADR-0003).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// What a backend can do. Agents use this to decide which features to ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCaps {
    pub chat: bool,
    pub embed: bool,
    pub vision: bool,
    pub stt: bool,
    pub tts: bool,
    /// Emits [`dn_core::chat::StreamEvent::ToolCall`] for `ChatRequest::tools`.
    pub tool_calls: bool,
    /// Honours `ChatRequest::response_schema` with constrained decoding.
    pub grammar: bool,
}

/// Where the model's weights or server live.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelLocation {
    /// A local GGUF file (llama.cpp backend).
    Gguf { path: PathBuf },
    /// An OpenAI-compatible server. `base_url` ends before `/chat/completions`, e.g.
    /// `http://localhost:11434/v1`.
    Endpoint {
        base_url: String,
        model: String,
        /// Bearer token, if the server needs one. Taken from `dn-secrets` by the caller;
        /// never logged or serialised.
        #[serde(skip)]
        api_key: Option<String>,
    },
}

impl fmt::Debug for ModelLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gguf { path } => f.debug_struct("Gguf").field("path", path).finish(),
            Self::Endpoint {
                base_url,
                model,
                api_key,
            } => f
                .debug_struct("Endpoint")
                .field("base_url", base_url)
                .field("model", model)
                .field("api_key", &api_key.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

/// A model to load.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Id used in `ModelRef::Local { model_id }` / `ModelRef::Remote { model_id }`.
    pub model_id: String,
    pub location: ModelLocation,
}

impl ModelSpec {
    pub fn gguf(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let model_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model".into());
        Self {
            model_id,
            location: ModelLocation::Gguf { path },
        }
    }

    pub fn endpoint(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        let model = model.into();
        Self {
            model_id: model.clone(),
            location: ModelLocation::Endpoint {
                base_url: base_url.into(),
                model,
                api_key,
            },
        }
    }
}

/// Mixture-of-experts expert placement (the "FreeToken idea", ADR-0003): keep attention and
/// shared tensors on the GPU and the large, sparsely used expert FFN tensors in CPU RAM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "layers", rename_all = "snake_case")]
pub enum MoeOffload {
    /// All tensors follow `gpu_layers`.
    #[default]
    None,
    /// Expert tensors of every layer stay in CPU RAM.
    All,
    /// Expert tensors of the first `n` layers stay in CPU RAM (llama.cpp `--n-cpu-moe`).
    FirstLayers(u32),
}

/// KV-cache element type. Quantised caches need flash attention, which llama.cpp turns on
/// automatically where the device supports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvCacheQuant {
    #[default]
    F16,
    Q8_0,
    Q4_0,
}

impl KvCacheQuant {
    /// Bytes per cached element (block overhead included).
    pub fn bytes_per_element(self) -> f64 {
        match self {
            Self::F16 => 2.0,
            Self::Q8_0 => 34.0 / 32.0,
            Self::Q4_0 => 18.0 / 32.0,
        }
    }
}

/// How to load a model. Defaults are what `dn run` uses without flags.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoadOptions {
    /// Context window in tokens. `None` = min(model's training context, 8192).
    pub n_ctx: Option<u32>,
    /// Layers to offload to the GPU. `None` = all (no-op on CPU-only builds).
    pub gpu_layers: Option<u32>,
    pub moe_offload: MoeOffload,
    pub kv_cache: KvCacheQuant,
    /// CPU threads. `None` = available parallelism.
    pub threads: Option<u32>,
    /// Logical batch size for prompt processing.
    pub n_batch: u32,
    /// Memory-map the weights (lets the OS page them; default on).
    pub mmap: bool,
    /// Reuse prompt prefixes between calls (llama.cpp only).
    #[serde(default)]
    pub prompt_cache: PromptCache,
    /// Speculative decoding with a smaller model of the same family (llama.cpp only).
    #[serde(default)]
    pub draft: Option<DraftModel>,
}

/// A draft model for speculative decoding (ADR-0020 §6). It must share the target's
/// vocabulary; a pair that doesn't fails to load with the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftModel {
    pub path: std::path::PathBuf,
    /// The most tokens drafted per step (the backend adjusts the length as it goes).
    pub max_tokens: u32,
}

impl DraftModel {
    pub const DEFAULT_MAX_TOKENS: u32 = 8;

    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_tokens: Self::DEFAULT_MAX_TOKENS,
        }
    }
}

/// Prompt reuse between calls on a loaded model (ADR-0020 §6). Off by default until the speed
/// suite shows the gain on each hardware tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCache {
    /// Keep the last call's tokens in the context and reuse the shared prefix.
    pub enabled: bool,
    /// RAM for sequences other than the live one, in bytes. When it's full, the least
    /// recently used goes first.
    pub ram_bytes: u64,
}

impl PromptCache {
    /// RAM for saved sequences unless set: 2 GiB.
    pub const DEFAULT_RAM_BYTES: u64 = 2 << 30;

    pub fn on() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

impl Default for PromptCache {
    fn default() -> Self {
        Self {
            enabled: false,
            ram_bytes: Self::DEFAULT_RAM_BYTES,
        }
    }
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            n_ctx: None,
            gpu_layers: None,
            moe_offload: MoeOffload::None,
            kv_cache: KvCacheQuant::F16,
            threads: None,
            n_batch: 512,
            mmap: true,
            prompt_cache: PromptCache::default(),
            draft: None,
        }
    }
}

/// Opaque handle to a loaded model. Cheap to clone; only valid on the backend that issued it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelHandle {
    pub(crate) id: u64,
    pub model_id: String,
}

impl ModelHandle {
    pub fn new(id: u64, model_id: impl Into<String>) -> Self {
        Self {
            id,
            model_id: model_id.into(),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_api_key() {
        let s = ModelSpec::endpoint("http://x/v1", "m", Some("sk-secret".into()));
        let d = format!("{s:?}");
        assert!(!d.contains("sk-secret"));
        assert!(d.contains("<redacted>"));
        assert!(!serde_json::to_string(&s).unwrap().contains("sk-secret"));
    }

    #[test]
    fn gguf_spec_uses_file_stem() {
        assert_eq!(
            ModelSpec::gguf("/m/qwen2.5-0.5b-q4_k_m.gguf").model_id,
            "qwen2.5-0.5b-q4_k_m"
        );
    }
}

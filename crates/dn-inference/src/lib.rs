//! `dn-inference` — the inference layer of ADR-0003.
//!
//! One [`InferenceBackend`] trait (load / chat / embed / unload, [`BackendCaps`]) with two
//! backends today:
//!
//! - [`LlamaCppBackend`] (feature `llamacpp`, default): llama.cpp in-process. Metal on macOS,
//!   CPU elsewhere, CUDA / Vulkan with the `cuda` / `vulkan` features. Streams tokens, emits
//!   tool calls (Hermes `<tool_call>` convention + lazy JSON grammar), honours
//!   `response_schema` with constrained decoding, supports MoE expert offload to CPU and
//!   quantised KV caches.
//! - [`OpenAiCompatBackend`] (always built): any `/v1/chat/completions` server (Ollama,
//!   LM Studio, vLLM, llama-server, FreeToken), through `dn-providers`; `/v1/embeddings`
//!   directly.
//!
//! Every loaded model can be wrapped in [`LoadedModel`], which implements
//! [`dn_core::chat::ChatModel`], so the agent loop never knows which backend answered.
//!
//! Cancellation: dropping the returned stream stops generation (the llama.cpp thread exits
//! between prompt batches or on its next token; the provider's HTTP task stops).
//!
//! GPU features in CI: CI builds the default features (Metal on macOS, CPU elsewhere).
//! `cuda` and `vulkan` are not built there: each needs its SDK on the runner (CUDA toolkit;
//! Vulkan headers + `glslc` for llama.cpp's shaders) and compiles llama.cpp again, which adds
//! many minutes per job. Check them locally with `cargo check -p dn-inference --features
//! vulkan` on a machine with the SDK; a separate manual CI job can come later.

mod backend;
mod error;
pub mod prompt;
mod stream;
pub mod tags;
mod types;

#[cfg(feature = "llamacpp")]
pub mod llamacpp;
pub mod openai_compat;

pub use backend::{InferenceBackend, LoadedModel, TokenStream};
pub use error::{InferenceError, Result};
pub use stream::next_event;
pub use types::{
    BackendCaps, KvCacheQuant, LoadOptions, ModelHandle, ModelLocation, ModelSpec, MoeOffload,
};

#[cfg(feature = "llamacpp")]
pub use llamacpp::LlamaCppBackend;
pub use openai_compat::OpenAiCompatBackend;

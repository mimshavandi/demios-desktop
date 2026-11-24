//! dn-providers — BYOK chat providers and the model resolver (ADR-0003, ADR-0005).
//!
//! - [`build`] turns a [`ProviderConfig`] into a [`ProviderAdapter`] (which is a
//!   [`dn_core::chat::ChatModel`]) for OpenAI, Anthropic, Gemini or any
//!   OpenAI-compatible server. Streaming over SSE, tool calls mapped to/from
//!   [`dn_core::chat::ToolCall`].
//! - Gateway policies: unsupported optional params are dropped and the call retried
//!   once; [`ReasoningPolicy`] decides thinking on/off from the caller tag; failures are
//!   typed [`CoreError::ModelUnavailable`] with recovery actions.
//! - [`resolver`]: request override → agent slot → function default → first ready
//!   source → `ModelSourceRequired`. Never a silent fallback across sources.

mod adapter;
mod anthropic;
mod gemini;
mod http;
mod openai;
pub mod policy;
pub mod resolver;
pub mod sse;
mod util;

pub use adapter::{ModelCaps, ModelInfo, ProviderAdapter, ProviderConfig, ProviderKind, build};
pub use dn_core::CoreError;
pub use policy::ReasoningPolicy;
pub use resolver::{ModelResolver, Resolved, ResolvedFrom, SourceEntry};
pub use util::{BoxFuture, next_event};

use dn_core::chat::{FinishReason, StreamEvent, Usage};
use util::Emit;

/// Fresh, globally unique tool-call id for providers that send none (UUIDv7), so ids
/// never repeat across model calls in a conversation.
pub(crate) fn new_call_id(prefix: &str) -> String {
    format!("{prefix}_{}", dn_core::Id::new())
}

/// Sends one event; `Err(Cancelled)` if the consumer dropped the stream.
pub(crate) async fn emit(tx: &Emit, ev: StreamEvent) -> Result<(), CoreError> {
    tx.send(Ok(ev)).await.map_err(|_| CoreError::Cancelled)
}

pub(crate) async fn send_done(tx: &Emit, finish_reason: FinishReason, usage: Usage) {
    let _ = tx
        .send(Ok(StreamEvent::Done {
            finish_reason,
            usage,
        }))
        .await;
}

/// Tool arguments arrive as streamed JSON text. Empty → `{}`; unparseable text is kept
/// as a JSON string so the tool executor can report it back to the model.
pub(crate) fn finish_tool_args(raw: &str) -> serde_json::Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

/// `f32` → JSON number without float noise (`0.2f32` → `0.2`, not `0.20000000298`).
pub(crate) fn f32_json(v: f32) -> serde_json::Value {
    v.to_string()
        .parse::<f64>()
        .map(serde_json::Value::from)
        .unwrap_or(serde_json::Value::Null)
}

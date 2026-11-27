//! Provider-neutral chat request and streaming events (ADR-0003). Local backends and
//! BYOK providers both speak these types, so agents never know which one answered.

use crate::model::ModelRef;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// Image bytes referenced by attachment id (bytes live in the store, not in the message).
    Image {
        attachment_id: String,
        mime_type: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ChatMessage {
    pub role: Role,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Set when `role == Tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub input_schema: serde_json::Value,
}

/// Whether the model should think before answering (port of Demios's caller-tagged policy).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    /// Decide from `caller` (prose-for-the-user callers keep thinking on).
    #[default]
    Auto,
    On,
    Off,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ChatRequest {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub model: ModelRef,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// JSON Schema the output must satisfy (constrained decoding where supported).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub response_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub reasoning: Reasoning,
    /// Dotted id of the code path making the call, e.g. `chat.reply`, `artifact.update`.
    pub caller: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Usage {
    /// The whole prompt, cached tokens included.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Of `input_tokens`, those served from a cache (llama.cpp prefix or slot reuse, a
    /// provider's prompt cache). ADR-0020 §6, §8.
    #[serde(default)]
    pub cached_input_tokens: u32,
    /// Of `input_tokens`, those written to a provider's prompt cache.
    #[serde(default)]
    pub cache_write_tokens: u32,
    /// Tokens a draft model proposed (speculative decoding); 0 without one.
    #[serde(default)]
    pub draft_tokens: u32,
    /// Of `draft_tokens`, those the model accepted.
    #[serde(default)]
    pub draft_accepted_tokens: u32,
}

impl Usage {
    /// Adds another call's usage (a run sums its model calls).
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cached_input_tokens += o.cached_input_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
        self.draft_tokens += o.draft_tokens;
        self.draft_accepted_tokens += o.draft_accepted_tokens;
    }
}

/// One item of a streamed response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCall {
        call: ToolCall,
    },
    Done {
        finish_reason: FinishReason,
        usage: Usage,
    },
}

/// Stream of events for one chat call.
pub type EventStream = std::pin::Pin<
    Box<dyn futures_core::Stream<Item = Result<StreamEvent, crate::CoreError>> + Send>,
>;

/// Where a model's context size comes from (ADR-0020 §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LimitSource {
    /// The context a local model was loaded with.
    Loaded,
    /// The provider or server reported it for this model.
    Provider,
    /// Demios' table of published sizes.
    Published,
    /// The person set it for this model.
    User,
}

/// How much a model reads at once (ADR-0020 §2). `None`: nobody knows, and the runtime
/// fails the turn with a way to set it; it never guesses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ModelLimits {
    /// The whole context in tokens: input and output together.
    pub context_tokens: Option<u32>,
    pub source: Option<LimitSource>,
}

impl ModelLimits {
    /// A known size and where it came from.
    pub fn known(context_tokens: u32, source: LimitSource) -> Self {
        Self {
            context_tokens: Some(context_tokens),
            source: Some(source),
        }
    }
}

/// A boxed future, for the trait's async methods.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Anything that can answer a [`ChatRequest`]: a local backend (`dn-inference`) or a
/// remote/BYOK provider (`dn-providers`). The agent loop (`dn-agent`) only sees this trait.
/// Dropping the stream cancels the call.
pub trait ChatModel: Send + Sync {
    fn chat(&self, req: ChatRequest) -> EventStream;

    /// How much `model` reads at once (ADR-0020 §2). A remote adapter serves several
    /// models, so it's asked per model. The default: unknown.
    fn limits<'a>(&'a self, _model: &'a ModelRef) -> BoxFuture<'a, ModelLimits> {
        Box::pin(async { ModelLimits::default() })
    }

    /// The exact number of input tokens `req` takes with this model's own tokenizer and
    /// template, when the backend can count it (a local model). `None`: it can't, and the
    /// runtime estimates and says so (ADR-0020 §2.1).
    fn count_tokens(&self, _req: &ChatRequest) -> Option<Result<u32, crate::CoreError>> {
        None
    }
}

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
    /// The web pages the model's own search cited for this answer (#369), on an assistant
    /// message. Untrusted: see [`WebCitation`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_citations: Vec<WebCitation>,
}

impl ChatMessage {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_calls: vec![],
            tool_call_id: None,
            web_citations: vec![],
        }
    }
}

/// A web page a model's own search cited (ADR-0020 §6–7, #369): Anthropic's web search,
/// OpenAI's search models, Gemini's grounding.
///
/// **Untrusted.** `title` and `cited_text` come from web pages, and the provider already
/// put the search results into the model's context without passing Demios' untrusted-text
/// format. When either is shown, or given to a model again, it goes through
/// [`crate::untrusted::block`]; `url` is shown as text and opened only through the app's
/// link confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WebCitation {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The passage the answer relied on, when the provider gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_text: Option<String>,
}

/// Lets the model run the provider's own web search during this call (ADR-0020 §6–7,
/// #369). Only set when the person chose "Use <provider>'s search" in Settings; a model or
/// provider that can't search refuses the request with a clear error rather than answering
/// without searching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeWebSearch {
    /// The most searches the model may run in this call.
    pub max_uses: u8,
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
    /// The provider's own web search, when the person chose it (#369). `None`: off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_web_search: Option<NativeWebSearch>,
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
    pub input_tokens: u32,
    pub output_tokens: u32,
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
    /// Pages the provider's own search cited (#369); an adapter sends them when it has them
    /// (with the text, or at the end). Collected onto the assistant message.
    WebCitations {
        citations: Vec<WebCitation>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Messages and requests stored or sent before #369 have no citations and no native
    /// search: they read as none and off, and those fields aren't written when empty.
    #[test]
    fn web_search_fields_default_to_none_and_stay_out_of_old_shapes() {
        let m: ChatMessage =
            serde_json::from_value(serde_json::json!({"role": "assistant", "content": []}))
                .unwrap();
        assert!(m.web_citations.is_empty());
        let v = serde_json::to_value(ChatMessage::text(Role::User, "hi")).unwrap();
        assert!(v.get("web_citations").is_none());

        let r: ChatRequest = serde_json::from_value(serde_json::json!({
            "model": "local:x", "messages": [], "caller": "chat.reply"
        }))
        .unwrap();
        assert_eq!(r.native_web_search, None);
        assert!(
            serde_json::to_value(&r)
                .unwrap()
                .get("native_web_search")
                .is_none()
        );

        let e = StreamEvent::WebCitations {
            citations: vec![WebCitation {
                url: "https://example.org/".into(),
                title: Some("Example".into()),
                cited_text: None,
            }],
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "web_citations");
        assert_eq!(serde_json::from_value::<StreamEvent>(v).unwrap(), e);
    }
}

//! Anthropic Messages API (streaming).

use crate::adapter::{Inner, ModelInfo, ProviderAdapter, ProviderKind, text_of};
use crate::http::{self, Call, OptionalParam, bad_response, stream_error};
use crate::policy::is_reasoning_model;
use crate::util::{BoxFuture, Emit, event_stream};
use crate::{emit, finish_tool_args, send_done};
use dn_core::CoreError;
use dn_core::chat::{
    ChatModel, ChatRequest, EventStream, FinishReason, Role, StreamEvent, ToolCall, Usage,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct Anthropic(pub Arc<Inner>);

const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8192;

const OPTIONAL: &[OptionalParam] = &[
    OptionalParam::new("temperature"),
    OptionalParam {
        path: "thinking",
        aliases: &["budget_tokens"],
        rename_to: None,
    },
];

fn headers(inner: &Inner, accept: &str) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("accept", accept.to_string()),
        ("anthropic-version", API_VERSION.to_string()),
    ];
    if let Some(k) = inner.key() {
        h.push(("x-api-key", k.to_string()));
    }
    h
}

pub(crate) fn body(inner: &Inner, model: &str, req: &ChatRequest) -> Value {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    // Anthropic wants alternating user/assistant turns; tool results are user content.
    let mut push = |role: &str, blocks: Vec<Value>| {
        if let Some(last) = messages.last_mut()
            && last["role"] == role
        {
            last["content"]
                .as_array_mut()
                .expect("array")
                .extend(blocks);
            return;
        }
        messages.push(json!({"role": role, "content": blocks}));
    };
    for m in &req.messages {
        let text = text_of(&m.content);
        match m.role {
            Role::System => system.push(text),
            Role::User => push("user", vec![json!({"type": "text", "text": text})]),
            Role::Assistant => {
                let mut blocks = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for c in &m.tool_calls {
                    blocks.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments}));
                }
                push("assistant", blocks);
            }
            Role::Tool => push(
                "user",
                vec![json!({"type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": text})],
            ),
        }
    }
    if let Some(schema) = &req.response_schema {
        system.push(format!(
            "Reply with only a JSON value (no prose, no code fences) that matches this JSON Schema:\n{schema}"
        ));
    }
    // Thinking blocks carry signatures that dn-core messages can't hold yet, so a
    // tool-result continuation runs without thinking (Anthropic accepts that).
    let continuing_tools = req.messages.last().is_some_and(|m| m.role == Role::Tool);
    let thinking =
        is_reasoning_model(model) && inner.cfg.reasoning.thinking(req) && !continuing_tools;
    let budget = inner.cfg.thinking_budget_tokens.max(1024);
    let mut max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    if thinking && max_tokens <= budget {
        max_tokens = budget + req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    }
    let mut b = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": true,
    });
    let o = b.as_object_mut().expect("object");
    if !system.is_empty() {
        o.insert("system".into(), json!(system.join("\n\n")));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
            .collect();
        o.insert("tools".into(), json!(tools));
    }
    if thinking {
        o.insert(
            "thinking".into(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
    } else if let Some(t) = req.temperature {
        // Thinking requires the default temperature.
        o.insert("temperature".into(), crate::f32_json(t));
    }
    b
}

enum Block {
    Text,
    Thinking,
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

async fn run(inner: Arc<Inner>, req: ChatRequest, tx: Emit) {
    let id = inner.id().to_string();
    let result: Result<(), CoreError> = async {
        let model = inner.model_id(&req)?;
        let resp = http::send(Call {
            client: &inner.client,
            provider_id: &id,
            method: reqwest::Method::POST,
            url: format!("{}/messages", inner.base),
            headers: headers(&inner, "text/event-stream"),
            body: Some(body(&inner, &model, &req)),
            optional: OPTIONAL.to_vec(),
            secret: inner.key(),
        })
        .await?;
        let mut blocks: HashMap<u64, Block> = HashMap::new();
        let mut usage = Usage::default();
        let mut finish = FinishReason::Stop;
        let mut any_call = false;
        let mut sse = http::SseReader::new(&id, resp);
        while let Some(ev) = sse.next().await? {
            let v: Value = serde_json::from_str(&ev.data).map_err(|e| bad_response(&id, e))?;
            match v["type"].as_str().unwrap_or(ev.event.as_str()) {
                "message_start" => {
                    let u = &v["message"]["usage"];
                    usage.input_tokens = (u["input_tokens"].as_u64().unwrap_or(0)
                        + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
                        + u["cache_creation_input_tokens"].as_u64().unwrap_or(0))
                        as u32;
                    usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0) as u32;
                }
                "content_block_start" => {
                    let idx = v["index"].as_u64().unwrap_or(0);
                    let cb = &v["content_block"];
                    let block = match cb["type"].as_str() {
                        Some("tool_use") => Block::Tool {
                            id: cb["id"].as_str().unwrap_or_default().into(),
                            name: cb["name"].as_str().unwrap_or_default().into(),
                            json: String::new(),
                        },
                        Some("thinking") | Some("redacted_thinking") => Block::Thinking,
                        _ => {
                            if let Some(t) = cb["text"].as_str().filter(|t| !t.is_empty()) {
                                emit(&tx, StreamEvent::TextDelta { text: t.into() }).await?;
                            }
                            Block::Text
                        }
                    };
                    blocks.insert(idx, block);
                }
                "content_block_delta" => {
                    let idx = v["index"].as_u64().unwrap_or(0);
                    let d = &v["delta"];
                    match d["type"].as_str() {
                        Some("text_delta") => {
                            let text = d["text"].as_str().unwrap_or_default().into();
                            emit(&tx, StreamEvent::TextDelta { text }).await?;
                        }
                        Some("thinking_delta") => {
                            let text = d["thinking"].as_str().unwrap_or_default().into();
                            emit(&tx, StreamEvent::ReasoningDelta { text }).await?;
                        }
                        Some("input_json_delta") => {
                            if let Some(Block::Tool { json, .. }) = blocks.get_mut(&idx) {
                                json.push_str(d["partial_json"].as_str().unwrap_or_default());
                            }
                        }
                        _ => {} // signature_delta, citations
                    }
                }
                "content_block_stop" => {
                    let idx = v["index"].as_u64().unwrap_or(0);
                    if let Some(Block::Tool { id, name, json }) = blocks.remove(&idx) {
                        any_call = true;
                        let call = ToolCall {
                            id,
                            name,
                            arguments: finish_tool_args(&json),
                        };
                        emit(&tx, StreamEvent::ToolCall { call }).await?;
                    }
                }
                "message_delta" => {
                    if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                        usage.output_tokens = n as u32;
                    }
                    finish = match v["delta"]["stop_reason"].as_str() {
                        Some("max_tokens") => FinishReason::Length,
                        Some("tool_use") => FinishReason::ToolCalls,
                        _ => FinishReason::Stop,
                    };
                }
                "message_stop" => break,
                "error" => {
                    return Err(stream_error(&id, &v["error"].to_string(), inner.key()));
                }
                _ => {} // ping
            }
        }
        if any_call {
            finish = FinishReason::ToolCalls;
        }
        send_done(&tx, finish, usage).await;
        Ok(())
    }
    .await;
    if let Err(e) = result
        && !matches!(e, CoreError::Cancelled)
    {
        let _ = tx.send(Err(e)).await;
    }
}

impl ChatModel for Anthropic {
    fn chat(&self, req: ChatRequest) -> EventStream {
        let inner = self.0.clone();
        event_stream(move |tx| run(inner, req, tx))
    }
}

impl ProviderAdapter for Anthropic {
    fn provider_id(&self) -> &str {
        self.0.id()
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, CoreError>> {
        Box::pin(async move {
            let id = self.0.id();
            let resp = http::send(Call {
                client: &self.0.client,
                provider_id: id,
                method: reqwest::Method::GET,
                url: format!("{}/models?limit=1000", self.0.base),
                headers: headers(&self.0, "application/json"),
                body: None,
                optional: vec![],
                secret: self.0.key(),
            })
            .await?;
            let v = http::json(id, resp).await?;
            let data = v["data"]
                .as_array()
                .ok_or_else(|| bad_response(id, "models: missing `data`"))?;
            Ok(data
                .iter()
                .filter_map(|m| {
                    Some(ModelInfo {
                        id: m["id"].as_str()?.into(),
                        display_name: m["display_name"].as_str().map(Into::into),
                        context_window: None,
                        methods: vec![],
                    })
                })
                .collect())
        })
    }
}

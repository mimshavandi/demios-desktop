//! OpenAI Chat Completions, used for OpenAI itself and every OpenAI-compatible server.

use crate::adapter::{Inner, ModelInfo, ProviderAdapter, ProviderKind, text_of};
use crate::http::{self, Call, OptionalParam, bad_response, stream_error};
use crate::policy::is_reasoning_model;
use crate::util::{BoxFuture, Emit, event_stream};
use crate::{emit, finish_tool_args, new_call_id, send_done};
use dn_core::CoreError;
use dn_core::chat::{
    ChatMessage, ChatModel, ChatRequest, EventStream, FinishReason, Role, StreamEvent, ToolCall,
    Usage,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct OpenAi(pub Arc<Inner>);

const OPTIONAL: &[OptionalParam] = &[
    OptionalParam::new("temperature"),
    OptionalParam::new("reasoning_effort"),
    OptionalParam {
        rename_to: Some("max_completion_tokens"),
        ..OptionalParam::new("max_tokens")
    },
    OptionalParam::new("max_completion_tokens"),
    OptionalParam::new("stream_options"),
    // Never dropped silently: the schema moves into the system prompt instead.
    OptionalParam {
        aliases: &["json_schema"],
        fallback: Some(schema_to_system_message),
        ..OptionalParam::new("response_format")
    },
];

/// `response_format` rejected → same schema as a system instruction.
fn schema_to_system_message(body: &mut Value, removed: Value) {
    let instruction = http::schema_instruction(&removed["json_schema"]["schema"]);
    let Some(msgs) = body["messages"].as_array_mut() else {
        return;
    };
    // Extend the leading system message (some servers allow only one), else add one.
    if let Some(first) = msgs.first_mut()
        && first["role"] == "system"
        && let Some(text) = first["content"].as_str()
    {
        first["content"] = json!(format!("{text}\n\n{instruction}"));
    } else {
        msgs.insert(0, json!({"role": "system", "content": instruction}));
    }
}

impl OpenAi {
    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("accept", "text/event-stream".to_string())];
        if let Some(k) = self.0.key() {
            h.push(("authorization", format!("Bearer {k}")));
        }
        h
    }
}

pub(crate) fn body(inner: &Inner, model: &str, req: &ChatRequest) -> Value {
    let official = inner.cfg.kind == ProviderKind::OpenAi;
    let reasoning = is_reasoning_model(model);
    let mut b = json!({
        "model": model,
        "messages": req.messages.iter().map(message).collect::<Vec<_>>(),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    let o = b.as_object_mut().expect("object");
    if let Some(n) = req.max_tokens {
        let key = if official {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        o.insert(key.into(), json!(n));
    }
    // OpenAI reasoning models only accept the default temperature.
    if let Some(t) = req.temperature
        && !(official && reasoning)
    {
        o.insert("temperature".into(), crate::f32_json(t));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.input_schema}})
            })
            .collect();
        o.insert("tools".into(), json!(tools));
    }
    if let Some(schema) = &req.response_schema {
        o.insert(
            "response_format".into(),
            json!({"type": "json_schema", "json_schema": {"name": "response", "schema": schema}}),
        );
    }
    if reasoning {
        let on = inner.cfg.reasoning.thinking(req);
        let effort = match (on, official) {
            (true, _) => "medium",
            (false, true) if model.starts_with("gpt-5") => "minimal",
            (false, true) => "low",
            (false, false) => "none",
        };
        o.insert("reasoning_effort".into(), json!(effort));
    }
    b
}

fn message(m: &ChatMessage) -> Value {
    match m.role {
        Role::System => json!({"role": "system", "content": text_of(&m.content)}),
        Role::User => json!({"role": "user", "content": text_of(&m.content)}),
        Role::Assistant => {
            let text = text_of(&m.content);
            let mut v = json!({"role": "assistant",
                "content": if text.is_empty() { Value::Null } else { json!(text) }});
            if !m.tool_calls.is_empty() {
                v["tool_calls"] = m
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({"id": c.id, "type": "function",
                            "function": {"name": c.name, "arguments": c.arguments.to_string()}})
                    })
                    .collect();
            }
            v
        }
        Role::Tool => json!({"role": "tool",
            "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
            "content": text_of(&m.content)}),
    }
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

/// Parses an OpenAI-style SSE body into stream events.
pub(crate) async fn run(inner: Arc<Inner>, req: ChatRequest, tx: Emit) {
    let id = inner.id().to_string();
    let result = async {
        let model = inner.model_id(&req)?;
        let resp = http::send(Call {
            client: &inner.client,
            provider_id: &id,
            method: reqwest::Method::POST,
            url: format!("{}/chat/completions", inner.base),
            headers: OpenAi(inner.clone()).headers(),
            body: Some(body(&inner, &model, &req)),
            optional: OPTIONAL.to_vec(),
            secret: inner.key(),
        })
        .await?;
        let mut calls: BTreeMap<u64, PartialCall> = BTreeMap::new();
        let mut finish = None;
        // `[DONE]` or a `finish_reason` ends a complete stream; EOF before either is a cut-off.
        let mut terminal = false;
        let mut usage = Usage::default();
        let mut sse = http::SseReader::new(&id, resp);
        while let Some(ev) = sse.next().await? {
            if ev.data.trim() == "[DONE]" {
                terminal = true;
                break;
            }
            let v: Value = serde_json::from_str(&ev.data).map_err(|e| bad_response(&id, e))?;
            if let Some(err) = v.get("error") {
                return Err(stream_error(&id, &err.to_string(), inner.key()));
            }
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                usage.input_tokens = u["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                usage.output_tokens = u["completion_tokens"].as_u64().unwrap_or(0) as u32;
            }
            let Some(choice) = v["choices"].get(0) else {
                continue;
            };
            let delta = &choice["delta"];
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta[key].as_str().filter(|t| !t.is_empty()) {
                    emit(&tx, StreamEvent::ReasoningDelta { text: t.into() }).await?;
                }
            }
            if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
                emit(&tx, StreamEvent::TextDelta { text: t.into() }).await?;
            }
            for tc in delta["tool_calls"].as_array().into_iter().flatten() {
                let idx = tc["index"].as_u64().unwrap_or(0);
                let p = calls.entry(idx).or_default();
                if let Some(s) = tc["id"].as_str() {
                    p.id = s.into();
                }
                if let Some(s) = tc["function"]["name"].as_str() {
                    p.name.push_str(s);
                }
                if let Some(s) = tc["function"]["arguments"].as_str() {
                    p.args.push_str(s);
                }
            }
            if let Some(f) = choice["finish_reason"].as_str() {
                terminal = true;
                finish = Some(match f {
                    "length" => FinishReason::Length,
                    "tool_calls" | "function_call" => FinishReason::ToolCalls,
                    _ => FinishReason::Stop,
                });
            }
        }
        if !terminal {
            return Err(http::truncated(&id, "`[DONE]` or a finish_reason"));
        }
        // Hitting the token limit can cut tool-call arguments off mid-JSON: keep `Length`
        // and drop the calls instead of running half-formed ones.
        if finish == Some(FinishReason::Length) {
            send_done(&tx, FinishReason::Length, usage).await;
            return Ok(());
        }
        let any_call = !calls.is_empty();
        for p in calls.into_values() {
            let call = ToolCall {
                id: if p.id.is_empty() {
                    new_call_id("call")
                } else {
                    p.id
                },
                name: p.name,
                arguments: finish_tool_args(&p.args),
            };
            emit(&tx, StreamEvent::ToolCall { call }).await?;
        }
        let finish = match (any_call, finish) {
            (true, _) => FinishReason::ToolCalls,
            (false, Some(f)) => f,
            (false, None) => FinishReason::Stop,
        };
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

impl ChatModel for OpenAi {
    fn chat(&self, req: ChatRequest) -> EventStream {
        let inner = self.0.clone();
        event_stream(move |tx| run(inner, req, tx))
    }
}

impl ProviderAdapter for OpenAi {
    fn provider_id(&self) -> &str {
        self.0.id()
    }

    fn kind(&self) -> ProviderKind {
        self.0.cfg.kind
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, CoreError>> {
        Box::pin(async move {
            let id = self.0.id();
            let mut headers = self.headers();
            headers[0].1 = "application/json".into();
            let resp = http::send(Call {
                client: &self.0.client,
                provider_id: id,
                method: reqwest::Method::GET,
                url: format!("{}/models", self.0.base),
                headers,
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
                .filter_map(|m| m["id"].as_str())
                .map(|m| ModelInfo {
                    id: m.into(),
                    display_name: None,
                    context_window: None,
                    methods: vec![],
                })
                .collect())
        })
    }
}

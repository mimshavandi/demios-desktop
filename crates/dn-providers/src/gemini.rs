//! Google Gemini API (`streamGenerateContent?alt=sse`).

use crate::adapter::{Inner, ModelInfo, ProviderAdapter, ProviderKind, text_of};
use crate::http::{self, Call, OptionalParam, bad_response, stream_error};
use crate::policy::is_reasoning_model;
use crate::util::{BoxFuture, Emit, event_stream};
use crate::{emit, new_call_id, send_done};
use dn_core::CoreError;
use dn_core::chat::{
    ChatModel, ChatRequest, EventStream, FinishReason, Role, StreamEvent, ToolCall, Usage,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct Gemini(pub Arc<Inner>);

/// Separator between our call number and Gemini's thought signature inside a tool-call id.
/// Gemini thinking models require the signature back with the function call; dn-core has
/// no per-provider field for it, so it rides in the opaque id.
const SIG_SEP: char = '~';

const OPTIONAL: &[OptionalParam] = &[
    OptionalParam {
        aliases: &["thinking", "thinking_budget"],
        ..OptionalParam::new("generationConfig.thinkingConfig")
    },
    OptionalParam::new("generationConfig.temperature"),
    // Never dropped silently: the schema moves into the system instruction instead.
    OptionalParam {
        aliases: &["response_schema", "responseSchema"],
        fallback: Some(schema_to_system_instruction),
        ..OptionalParam::new("generationConfig.responseJsonSchema")
    },
];

/// `responseJsonSchema` rejected → same schema as a system instruction.
fn schema_to_system_instruction(body: &mut Value, removed: Value) {
    let part = json!({"text": crate::http::schema_instruction(&removed)});
    match body["systemInstruction"]["parts"].as_array_mut() {
        Some(parts) => parts.push(part),
        None => body["systemInstruction"] = json!({"parts": [part]}),
    }
}

fn headers(inner: &Inner, accept: &str) -> Vec<(&'static str, String)> {
    let mut h = vec![("accept", accept.to_string())];
    if let Some(k) = inner.key() {
        h.push(("x-goog-api-key", k.to_string()));
    }
    h
}

fn split_id(id: &str) -> (&str, Option<&str>) {
    match id.split_once(SIG_SEP) {
        Some((a, sig)) => (a, Some(sig)),
        None => (id, None),
    }
}

pub(crate) fn body(inner: &Inner, model: &str, req: &ChatRequest) -> Value {
    let mut system = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut push = |role: &str, parts: Vec<Value>| {
        if let Some(last) = contents.last_mut()
            && last["role"] == role
        {
            last["parts"].as_array_mut().expect("array").extend(parts);
            return;
        }
        contents.push(json!({"role": role, "parts": parts}));
    };
    for m in &req.messages {
        let text = text_of(&m.content);
        match m.role {
            Role::System => system.push(text),
            Role::User => push("user", vec![json!({"text": text})]),
            Role::Assistant => {
                let mut parts = Vec::new();
                if !text.is_empty() {
                    parts.push(json!({"text": text}));
                }
                for c in &m.tool_calls {
                    names.insert(c.id.as_str(), c.name.as_str());
                    let mut p = json!({"functionCall": {"name": c.name, "args": c.arguments}});
                    if let (_, Some(sig)) = split_id(&c.id) {
                        p["thoughtSignature"] = json!(sig);
                    }
                    parts.push(p);
                }
                push("model", parts);
            }
            Role::Tool => {
                let call_id = m.tool_call_id.as_deref().unwrap_or_default();
                let name = names.get(call_id).copied().unwrap_or(call_id);
                push(
                    "user",
                    vec![
                        json!({"functionResponse": {"name": name, "response": {"content": text}}}),
                    ],
                );
            }
        }
    }
    let mut gc = serde_json::Map::new();
    if let Some(n) = req.max_tokens {
        gc.insert("maxOutputTokens".into(), json!(n));
    }
    if let Some(t) = req.temperature {
        gc.insert("temperature".into(), crate::f32_json(t));
    }
    if let Some(schema) = &req.response_schema {
        gc.insert("responseMimeType".into(), json!("application/json"));
        gc.insert("responseJsonSchema".into(), schema.clone());
    }
    if is_reasoning_model(model) {
        let cfg = if inner.cfg.reasoning.thinking(req) {
            json!({"includeThoughts": true, "thinkingBudget": -1})
        } else {
            json!({"thinkingBudget": 0})
        };
        gc.insert("thinkingConfig".into(), cfg);
    }
    let mut b = json!({"contents": contents});
    let o = b.as_object_mut().expect("object");
    if !system.is_empty() {
        o.insert(
            "systemInstruction".into(),
            json!({"parts": [{"text": system.join("\n\n")}]}),
        );
    }
    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "parametersJsonSchema": t.input_schema}))
            .collect();
        o.insert("tools".into(), json!([{"functionDeclarations": decls}]));
    }
    if !gc.is_empty() {
        o.insert("generationConfig".into(), Value::Object(gc));
    }
    b
}

async fn run(inner: Arc<Inner>, req: ChatRequest, tx: Emit) {
    let id = inner.id().to_string();
    let result: Result<(), CoreError> = async {
        let model = inner.model_id(&req)?;
        let resp = http::send(Call {
            client: &inner.client,
            provider_id: &id,
            method: reqwest::Method::POST,
            url: format!(
                "{}/models/{model}:streamGenerateContent?alt=sse",
                inner.base
            ),
            headers: headers(&inner, "text/event-stream"),
            body: Some(body(&inner, &model, &req)),
            optional: OPTIONAL.to_vec(),
            secret: inner.key(),
            first_byte: inner.cfg.first_byte_timeout,
        })
        .await?;
        let mut usage = Usage::default();
        let mut finish = FinishReason::Stop;
        let mut n_calls = 0u32;
        let mut finished = false;
        let mut sse = http::SseReader::new(&id, resp, &inner.cfg);
        while let Some(ev) = sse.next().await? {
            let v: Value = serde_json::from_str(&ev.data).map_err(|e| bad_response(&id, e))?;
            if let Some(err) = v.get("error") {
                return Err(stream_error(&id, &err.to_string(), inner.key()));
            }
            if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
                return Err(stream_error(
                    &id,
                    &format!("prompt blocked: {reason}"),
                    None,
                ));
            }
            if let Some(u) = v.get("usageMetadata") {
                usage.input_tokens = u["promptTokenCount"].as_u64().unwrap_or(0) as u32;
                usage.output_tokens = (u["candidatesTokenCount"].as_u64().unwrap_or(0)
                    + u["thoughtsTokenCount"].as_u64().unwrap_or(0))
                    as u32;
            }
            let Some(cand) = v["candidates"].get(0) else {
                continue;
            };
            for part in cand["content"]["parts"].as_array().into_iter().flatten() {
                if let Some(fc) = part.get("functionCall") {
                    let mut call_id = fc["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .unwrap_or_else(|| new_call_id("gm"));
                    if let Some(sig) = part["thoughtSignature"].as_str() {
                        call_id = format!("{call_id}{SIG_SEP}{sig}");
                    }
                    n_calls += 1;
                    let call = ToolCall {
                        id: call_id,
                        name: fc["name"].as_str().unwrap_or_default().into(),
                        arguments: fc.get("args").cloned().unwrap_or_else(|| json!({})),
                    };
                    emit(&tx, StreamEvent::ToolCall { call }).await?;
                } else if let Some(t) = part["text"].as_str().filter(|t| !t.is_empty()) {
                    let ev = if part["thought"].as_bool() == Some(true) {
                        StreamEvent::ReasoningDelta { text: t.into() }
                    } else {
                        StreamEvent::TextDelta { text: t.into() }
                    };
                    emit(&tx, ev).await?;
                }
            }
            if let Some(f) = cand["finishReason"].as_str() {
                finished = true;
                finish = match f {
                    "MAX_TOKENS" => FinishReason::Length,
                    "STOP" | "FINISH_REASON_UNSPECIFIED" => FinishReason::Stop,
                    "SAFETY"
                    | "RECITATION"
                    | "BLOCKLIST"
                    | "PROHIBITED_CONTENT"
                    | "SPII"
                    | "MALFORMED_FUNCTION_CALL" => FinishReason::Error,
                    _ => FinishReason::Stop,
                };
            }
        }
        if !finished {
            return Err(http::truncated(&id, "a finishReason"));
        }
        if n_calls > 0 && finish == FinishReason::Stop {
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

impl ChatModel for Gemini {
    fn chat(&self, req: ChatRequest) -> EventStream {
        let inner = self.0.clone();
        event_stream(move |tx| run(inner, req, tx))
    }
}

impl ProviderAdapter for Gemini {
    fn provider_id(&self) -> &str {
        self.0.id()
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, CoreError>> {
        Box::pin(async move {
            let id = self.0.id();
            let resp = http::send(Call {
                client: &self.0.client,
                provider_id: id,
                method: reqwest::Method::GET,
                url: format!("{}/models?pageSize=1000", self.0.base),
                headers: headers(&self.0, "application/json"),
                body: None,
                optional: vec![],
                secret: self.0.key(),
                first_byte: self.0.cfg.first_byte_timeout,
            })
            .await?;
            let v = http::json(id, resp).await?;
            let data = v["models"]
                .as_array()
                .ok_or_else(|| bad_response(id, "models: missing `models`"))?;
            Ok(data
                .iter()
                .filter_map(|m| {
                    let name = m["name"].as_str()?;
                    Some(ModelInfo {
                        id: name.strip_prefix("models/").unwrap_or(name).into(),
                        display_name: m["displayName"].as_str().map(Into::into),
                        context_window: m["inputTokenLimit"].as_u64().map(|n| n as u32),
                        methods: m["supportedGenerationMethods"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|x| x.as_str().map(Into::into))
                            .collect(),
                        tool_calls: None,
                        embedding: None,
                    })
                })
                .collect())
        })
    }
}

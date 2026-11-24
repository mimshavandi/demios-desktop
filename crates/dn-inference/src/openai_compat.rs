//! Backend for any OpenAI-compatible server: Ollama, LM Studio, vLLM, llama-server,
//! FreeToken, a team inference box. Streams `/chat/completions` over SSE.

use crate::backend::{TokenStream, normalize};
use crate::stream::{ChannelStream, Item};
use crate::tags::TagParser;
use crate::{
    BackendCaps, InferenceBackend, InferenceError, LoadOptions, ModelHandle, ModelLocation,
    ModelSpec, Result,
};
use dn_core::chat::{
    ChatMessage, ChatRequest, ContentPart, FinishReason, Role, StreamEvent, ToolCall, Usage,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone)]
struct Endpoint {
    base_url: String,
    model: String,
    api_key: Option<String>,
}

pub struct OpenAiCompatBackend {
    client: reqwest::Client,
    endpoints: Mutex<HashMap<u64, Endpoint>>,
    next_id: AtomicU64,
    probe_on_load: bool,
}

impl Default for OpenAiCompatBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiCompatBackend {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            endpoints: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            probe_on_load: true,
        }
    }

    /// Skip the `GET /models` reachability check in `load` (for servers that lack it).
    pub fn without_probe(mut self) -> Self {
        self.probe_on_load = false;
        self
    }

    fn get(&self, h: &ModelHandle) -> Result<Endpoint> {
        self.endpoints
            .lock()
            .map_err(|_| InferenceError::Runtime("endpoint table poisoned".into()))?
            .get(&h.id)
            .cloned()
            .ok_or(InferenceError::UnknownHandle(h.id))
    }

    fn post(&self, ep: &Endpoint, path: &str, body: &Value) -> reqwest::RequestBuilder {
        let url = format!("{}/{path}", ep.base_url.trim_end_matches('/'));
        let mut rb = self.client.post(url).json(body);
        if let Some(k) = &ep.api_key {
            rb = rb.bearer_auth(k);
        }
        rb
    }
}

fn http(e: reqwest::Error) -> InferenceError {
    InferenceError::Http(e.without_url().to_string())
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status().as_u16();
    let mut body = resp.text().await.unwrap_or_default();
    body.truncate(500);
    Err(InferenceError::Server { status, body })
}

fn text_of(m: &ChatMessage) -> String {
    m.content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The request body for `/chat/completions`.
pub fn request_body(model: &str, req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let mut v = json!({"role": role, "content": text_of(m)});
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
            if let Some(id) = &m.tool_call_id {
                v["tool_call_id"] = json!(id);
            }
            v
        })
        .collect();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function":
                    {"name": t.name, "description": t.description, "parameters": t.input_schema}})
            })
            .collect();
    }
    if let Some(n) = req.max_tokens {
        body["max_tokens"] = json!(n);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(s) = &req.response_schema {
        body["response_format"] =
            json!({"type": "json_schema", "json_schema": {"name": "response", "schema": s}});
    }
    body
}

/// Splits an SSE byte stream into `data:` payloads.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = vec![];
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(d) = line.strip_prefix("data:") {
                self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            }
        }
        out
    }

    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let line = String::from_utf8_lossy(&rest);
        if let Some(d) = line.trim_end().strip_prefix("data:") {
            self.data.push(d.trim_start().to_string());
        }
        (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"))
    }
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

/// Turns OpenAI stream chunks into [`StreamEvent`]s.
#[derive(Default)]
pub struct ChunkAssembler {
    tags: TagParser,
    calls: BTreeMap<u64, PartialCall>,
    finish: Option<FinishReason>,
    usage: Usage,
    done: bool,
}

impl ChunkAssembler {
    /// Feed one `data:` payload. Returns events plus whether the stream signalled `[DONE]`.
    pub fn push(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(vec![]);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| InferenceError::Runtime(format!("bad stream chunk: {e}")))?;
        if let Some(err) = v.get("error") {
            return Err(InferenceError::Server {
                status: 200,
                body: err.to_string(),
            });
        }
        let mut out = vec![];
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
            };
        }
        let Some(choice) = v["choices"].get(0) else {
            return Ok(out);
        };
        let delta = &choice["delta"];
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = delta[key].as_str().filter(|t| !t.is_empty()) {
                out.push(StreamEvent::ReasoningDelta { text: t.into() });
            }
        }
        if let Some(t) = delta["content"].as_str() {
            out.extend(self.tags.push(t));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for (n, c) in calls.iter().enumerate() {
                let idx = c["index"].as_u64().unwrap_or(n as u64);
                let p = self.calls.entry(idx).or_default();
                if let Some(id) = c["id"].as_str() {
                    p.id = id.into();
                }
                if let Some(name) = c["function"]["name"].as_str() {
                    p.name.push_str(name);
                }
                if let Some(a) = c["function"]["arguments"].as_str() {
                    p.args.push_str(a);
                }
            }
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(match f {
                "length" => FinishReason::Length,
                "tool_calls" | "function_call" => FinishReason::ToolCalls,
                "stop" => FinishReason::Stop,
                _ => FinishReason::Stop,
            });
        }
        Ok(out)
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Flush buffered text and tool calls, then the final `Done`.
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = self.tags.finish();
        let in_band_calls = self.tags.tool_calls();
        let calls = std::mem::take(&mut self.calls);
        let n_calls = calls.len() as u32 + in_band_calls;
        for (i, (_, p)) in calls.into_iter().enumerate() {
            let arguments = if p.args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&p.args).unwrap_or(Value::String(p.args))
            };
            out.push(StreamEvent::ToolCall {
                call: ToolCall {
                    id: if p.id.is_empty() {
                        format!("call_{}", i + 1)
                    } else {
                        p.id
                    },
                    name: p.name,
                    arguments,
                },
            });
        }
        let finish = match self.finish {
            Some(f) => f,
            None if n_calls > 0 => FinishReason::ToolCalls,
            None => FinishReason::Stop,
        };
        out.push(StreamEvent::Done {
            finish_reason: if n_calls > 0 && finish == FinishReason::Stop {
                FinishReason::ToolCalls
            } else {
                finish
            },
            usage: self.usage,
        });
        out
    }
}

async fn pump(mut resp: reqwest::Response, tx: mpsc::Sender<Item>) {
    let mut sse = SseParser::default();
    let mut asm = ChunkAssembler::default();
    let send_all = |evs: Vec<StreamEvent>, tx: &mpsc::Sender<Item>| {
        let tx = tx.clone();
        async move {
            for ev in evs {
                if tx.send(Ok(ev)).await.is_err() {
                    return false;
                }
            }
            true
        }
    };
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => {
                let _ = tx.send(Err(http(e).into())).await;
                return;
            }
        };
        for data in sse.push(&chunk) {
            match asm.push(&data) {
                Ok(evs) => {
                    if !send_all(evs, &tx).await {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.into())).await;
                    return;
                }
            }
        }
        if asm.is_done() {
            break;
        }
    }
    if let Some(data) = sse.finish() {
        if let Ok(evs) = asm.push(&data) {
            if !send_all(evs, &tx).await {
                return;
            }
        }
    }
    send_all(asm.finish(), &tx).await;
}

impl InferenceBackend for OpenAiCompatBackend {
    fn name(&self) -> &'static str {
        "openai_compat"
    }

    fn capabilities(&self) -> BackendCaps {
        BackendCaps {
            chat: true,
            embed: true,
            tool_calls: true,
            grammar: true,
            ..Default::default()
        }
    }

    async fn load(&self, spec: &ModelSpec, _opts: &LoadOptions) -> Result<ModelHandle> {
        let ModelLocation::Endpoint {
            base_url,
            model,
            api_key,
        } = &spec.location
        else {
            return Err(InferenceError::Unsupported {
                backend: "openai_compat",
                what: "local GGUF files (use llamacpp)".into(),
            });
        };
        let ep = Endpoint {
            base_url: base_url.clone(),
            model: model.clone(),
            api_key: api_key.clone(),
        };
        if self.probe_on_load {
            let url = format!("{}/models", ep.base_url.trim_end_matches('/'));
            let mut rb = self.client.get(url).timeout(Duration::from_secs(5));
            if let Some(k) = &ep.api_key {
                rb = rb.bearer_auth(k);
            }
            // Only "can't connect" fails the load; some servers have no /models route.
            rb.send().await.map_err(http)?;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.endpoints
            .lock()
            .map_err(|_| InferenceError::Runtime("endpoint table poisoned".into()))?
            .insert(id, ep);
        Ok(ModelHandle::new(id, spec.model_id.clone()))
    }

    async fn chat(&self, h: &ModelHandle, req: ChatRequest) -> Result<TokenStream> {
        let ep = self.get(h)?;
        let body = request_body(&ep.model, &req);
        let resp = self
            .post(&ep, "chat/completions", &body)
            .header("accept", "text/event-stream")
            .send()
            .await
            .map_err(http)?;
        let resp = check(resp).await?;
        let (tx, rx) = mpsc::channel(64);
        let task = tokio::spawn(pump(resp, tx));
        Ok(Box::pin(ChannelStream::with_task(rx, task)))
    }

    async fn embed(&self, h: &ModelHandle, input: &[String]) -> Result<Vec<Vec<f32>>> {
        let ep = self.get(h)?;
        let body = json!({"model": ep.model, "input": input});
        let resp = check(
            self.post(&ep, "embeddings", &body)
                .send()
                .await
                .map_err(http)?,
        )
        .await?;
        let v: Value = resp.json().await.map_err(http)?;
        let mut items: Vec<(u64, Vec<f32>)> = v["data"]
            .as_array()
            .ok_or_else(|| InferenceError::Runtime("embeddings: no data".into()))?
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let e = d["embedding"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_f64())
                            .map(|x| x as f32)
                            .collect()
                    })
                    .unwrap_or_default();
                (d["index"].as_u64().unwrap_or(i as u64), e)
            })
            .collect();
        items.sort_by_key(|(i, _)| *i);
        Ok(items
            .into_iter()
            .map(|(_, mut e)| {
                normalize(&mut e);
                e
            })
            .collect())
    }

    async fn unload(&self, h: ModelHandle) -> Result<()> {
        self.endpoints
            .lock()
            .map_err(|_| InferenceError::Runtime("endpoint table poisoned".into()))?
            .remove(&h.id)
            .map(|_| ())
            .ok_or(InferenceError::UnknownHandle(h.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::next_event;
    use dn_core::chat::{ChatMessage, ToolSpec};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Recorded from an OpenAI-compatible server (shape shared by Ollama / vLLM / LM Studio).
    const TEXT_STREAM: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\r\n\r\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo!\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3}}\n\n\
data: [DONE]\n\n";

    const TOOL_STREAM: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Oslo\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";

    fn assemble(stream: &str, chunk: usize) -> Vec<StreamEvent> {
        let mut sse = SseParser::default();
        let mut asm = ChunkAssembler::default();
        let mut out = vec![];
        for c in stream.as_bytes().chunks(chunk) {
            for d in sse.push(c) {
                out.extend(asm.push(&d).unwrap());
            }
        }
        out.extend(asm.finish());
        out
    }

    #[test]
    fn text_stream_any_chunking() {
        for chunk in [1, 7, 4096] {
            let ev = assemble(TEXT_STREAM, chunk);
            let text: String = ev
                .iter()
                .filter_map(|e| match e {
                    StreamEvent::TextDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(text, "Hello!");
            assert!(ev.contains(&StreamEvent::ReasoningDelta { text: "hmm".into() }));
            assert_eq!(
                ev.last(),
                Some(&StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: Usage {
                        input_tokens: 9,
                        output_tokens: 3
                    }
                })
            );
        }
    }

    #[test]
    fn tool_call_fragments_assembled() {
        let ev = assemble(TOOL_STREAM, 5);
        assert_eq!(
            ev,
            vec![
                StreamEvent::ToolCall {
                    call: ToolCall {
                        id: "call_a".into(),
                        name: "get_weather".into(),
                        arguments: json!({"city": "Oslo"})
                    }
                },
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: Usage::default()
                }
            ]
        );
    }

    #[test]
    fn body_shape() {
        let mut req = ChatRequest {
            model: "remote:ollama:qwen3".parse().unwrap(),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![ToolSpec {
                name: "t".into(),
                description: "d".into(),
                input_schema: json!({"type":"object"}),
            }],
            max_tokens: Some(5),
            temperature: Some(0.0),
            response_schema: None,
            reasoning: Default::default(),
            caller: "test".into(),
        };
        let mut a = ChatMessage::text(Role::Assistant, "");
        a.tool_calls.push(ToolCall {
            id: "c".into(),
            name: "t".into(),
            arguments: json!({"x":1}),
        });
        req.messages.push(a);
        let b = request_body("qwen3", &req);
        assert_eq!(b["model"], "qwen3");
        assert_eq!(b["stream"], true);
        assert_eq!(b["tools"][0]["function"]["name"], "t");
        assert_eq!(
            b["messages"][1]["tool_calls"][0]["function"]["arguments"],
            "{\"x\":1}"
        );
        assert_eq!(b["max_tokens"], 5);
    }

    /// Minimal HTTP server replaying a canned SSE body: exercises the real reqwest path
    /// without any external service.
    async fn serve_once(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 16384];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn streams_over_http() {
        let base = serve_once(TEXT_STREAM).await;
        let b = OpenAiCompatBackend::new();
        let h = b
            .load(
                &ModelSpec::endpoint(base, "m", Some("k".into())),
                &LoadOptions::default(),
            )
            .await
            .unwrap();
        let req = ChatRequest {
            model: "remote:local:m".parse().unwrap(),
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            response_schema: None,
            reasoning: Default::default(),
            caller: "test".into(),
        };
        let mut s = b.chat(&h, req).await.unwrap();
        let mut text = String::new();
        let mut done = false;
        while let Some(ev) = next_event(&mut s).await {
            match ev.unwrap() {
                StreamEvent::TextDelta { text: t } => text.push_str(&t),
                StreamEvent::Done { .. } => done = true,
                _ => {}
            }
        }
        assert_eq!(text, "Hello!");
        assert!(done);
    }

    #[tokio::test]
    async fn unreachable_server_fails_load() {
        let b = OpenAiCompatBackend::new();
        let e = b
            .load(
                &ModelSpec::endpoint("http://127.0.0.1:9/v1", "m", None),
                &LoadOptions::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, InferenceError::Http(_)));
    }
}

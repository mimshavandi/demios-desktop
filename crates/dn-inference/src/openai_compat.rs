//! Backend for any OpenAI-compatible server (Ollama, LM Studio, vLLM, llama-server,
//! FreeToken, a team inference box).
//!
//! A thin adapter over `dn-providers`' OpenAI-compatible provider, so there is one SSE
//! parser, one set of timeouts and one tool-call assembler in the workspace. This type only
//! maps [`ModelSpec::endpoint`] + [`ModelHandle`] onto a provider and rewrites the request's
//! model ref to it. Embeddings (not offered by dn-providers) are a small direct
//! `POST /embeddings` here.

use crate::backend::{TokenStream, normalize};
use crate::{
    BackendCaps, InferenceBackend, InferenceError, LoadOptions, ModelHandle, ModelLocation,
    ModelSpec, Result,
};
use dn_core::chat::ChatRequest;
use dn_core::model::ModelRef;
use dn_providers::{ProviderAdapter, ProviderConfig, ProviderKind};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
struct Endpoint {
    provider: Arc<dyn ProviderAdapter>,
    provider_id: String,
    model: String,
    base_url: String,
    api_key: Option<String>,
}

pub struct OpenAiCompatBackend {
    endpoints: Mutex<HashMap<u64, Endpoint>>,
    next_id: AtomicU64,
    /// For `/embeddings` only; chat goes through dn-providers.
    http: reqwest::Client,
}

impl Default for OpenAiCompatBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiCompatBackend {
    pub fn new() -> Self {
        Self {
            endpoints: Mutex::default(),
            next_id: AtomicU64::new(0),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
        }
    }

    fn get(&self, h: &ModelHandle) -> Result<Endpoint> {
        self.endpoints
            .lock()
            .map_err(|_| InferenceError::Runtime("endpoint table poisoned".into()))?
            .get(&h.id)
            .cloned()
            .ok_or(InferenceError::UnknownHandle(h.id))
    }
}

fn provider_err(e: dn_core::CoreError) -> InferenceError {
    InferenceError::Http(e.to_string())
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
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let provider_id = format!("dn-inference-{id}");
        let mut cfg = ProviderConfig::new(&provider_id, ProviderKind::OpenAiCompatible)
            .with_base_url(base_url.clone());
        if let Some(k) = api_key {
            cfg = cfg.with_key(k.clone().into());
        }
        // Which local server this is decides how thinking is switched on/off (T-PRV-04).
        let family = dn_providers::detect_server_family(&cfg).await;
        let provider = dn_providers::build(cfg.with_server_family(family)).map_err(provider_err)?;
        self.endpoints
            .lock()
            .map_err(|_| InferenceError::Runtime("endpoint table poisoned".into()))?
            .insert(
                id,
                Endpoint {
                    provider,
                    provider_id,
                    model: model.clone(),
                    base_url: base_url.trim_end_matches('/').to_string(),
                    api_key: api_key.clone(),
                },
            );
        Ok(ModelHandle::new(id, spec.model_id.clone()))
    }

    async fn chat(&self, h: &ModelHandle, mut req: ChatRequest) -> Result<TokenStream> {
        let ep = self.get(h)?;
        req.model = ModelRef::Remote {
            endpoint_id: ep.provider_id.clone(),
            model_id: ep.model.clone(),
        };
        Ok(ep.provider.chat(req))
    }

    async fn embed(&self, h: &ModelHandle, input: &[String]) -> Result<Vec<Vec<f32>>> {
        let ep = self.get(h)?;
        let http = |e: reqwest::Error| InferenceError::Http(e.without_url().to_string());
        let mut rb = self
            .http
            .post(format!("{}/embeddings", ep.base_url))
            .header("content-type", "application/json")
            .body(json!({"model": ep.model, "input": input}).to_string());
        if let Some(k) = &ep.api_key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb.send().await.map_err(http)?;
        let status = resp.status();
        let text = resp.text().await.map_err(http)?;
        if !status.is_success() {
            let mut body = text;
            body.truncate(500);
            return Err(InferenceError::Server {
                status: status.as_u16(),
                body,
            });
        }
        parse_embeddings(&text, input.len())
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

/// `{"data": [{"index": i, "embedding": [...]}, ...]}` → vectors in input order, L2-normalised.
fn parse_embeddings(body: &str, n: usize) -> Result<Vec<Vec<f32>>> {
    let bad = |m: &str| InferenceError::Runtime(format!("embeddings: {m}"));
    let v: Value = serde_json::from_str(body).map_err(|e| bad(&e.to_string()))?;
    let data = v["data"].as_array().ok_or_else(|| bad("no data"))?;
    if data.len() != n {
        return Err(bad(&format!("{} vectors for {n} inputs", data.len())));
    }
    let mut out = vec![None; n];
    for (i, d) in data.iter().enumerate() {
        let idx = d["index"].as_u64().map_or(i, |x| x as usize);
        let mut e: Vec<f32> = d["embedding"]
            .as_array()
            .ok_or_else(|| bad("embedding is not an array"))?
            .iter()
            .map(|x| {
                x.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| bad("non-number"))
            })
            .collect::<Result<_>>()?;
        normalize(&mut e);
        *out.get_mut(idx).ok_or_else(|| bad("index out of range"))? = Some(e);
    }
    out.into_iter()
        .map(|e| e.ok_or_else(|| bad("missing index")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LoadedModel, next_event};
    use dn_core::chat::{ChatMessage, ChatModel, Role, StreamEvent};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// SSE server answering "hi" to the chat request (recorded); other requests (the
    /// server-family probes on load) get a 404.
    async fn server() -> (String, tokio::task::JoinHandle<String>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 16384];
                let mut req = String::new();
                loop {
                    let n = s.read(&mut buf).await.unwrap();
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some(i) = req.find("\r\n\r\n") {
                        let len: usize = req[..i]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        if req.len() >= i + 4 + len {
                            break;
                        }
                    }
                }
                if !req.contains("/chat/completions") {
                    let _ = s
                    .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
                    continue;
                }
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                        data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                        data: [DONE]\n\n";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(resp.as_bytes()).await.unwrap();
                return req;
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn streams_through_dn_providers() {
        let (url, task) = server().await;
        let m = LoadedModel::load(
            Arc::new(OpenAiCompatBackend::new()),
            &ModelSpec::endpoint(url, "qwen3:8b", None),
            &LoadOptions::default(),
        )
        .await
        .unwrap();
        let mut s = m.chat(ChatRequest {
            model: "remote:cli:qwen3:8b".parse().unwrap(),
            messages: vec![ChatMessage::text(Role::User, "hello")],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            response_schema: None,
            reasoning: Default::default(),
            caller: "test".into(),
            native_web_search: None,
        });
        let mut text = String::new();
        let mut done = false;
        while let Some(ev) = next_event(&mut s).await {
            match ev.unwrap() {
                StreamEvent::TextDelta { text: t } => text.push_str(&t),
                StreamEvent::Done { .. } => done = true,
                _ => {}
            }
        }
        assert_eq!(text, "hi");
        assert!(done);
        let req = task.await.unwrap();
        assert!(req.contains("\"model\":\"qwen3:8b\""), "{req}");
    }

    #[test]
    fn embeddings_reordered_and_normalised() {
        let body = r#"{"data":[{"index":1,"embedding":[0,2]},{"index":0,"embedding":[3,4]}]}"#;
        let v = parse_embeddings(body, 2).unwrap();
        assert_eq!(v, vec![vec![0.6, 0.8], vec![0.0, 1.0]]);
        assert!(parse_embeddings(body, 3).is_err());
        assert!(parse_embeddings(r#"{"data":[{"index":5,"embedding":[1]}]}"#, 1).is_err());
    }

    #[tokio::test]
    async fn gguf_spec_rejected() {
        let b = OpenAiCompatBackend::new();
        assert!(matches!(
            b.load(&ModelSpec::gguf("x.gguf"), &LoadOptions::default())
                .await,
            Err(InferenceError::Unsupported { .. })
        ));
    }
}

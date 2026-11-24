//! Backend for any OpenAI-compatible server (Ollama, LM Studio, vLLM, llama-server,
//! FreeToken, a team inference box).
//!
//! A thin adapter over `dn-providers`' OpenAI-compatible provider, so there is one SSE
//! parser, one set of timeouts and one tool-call assembler in the workspace. This type only
//! maps [`ModelSpec::endpoint`] + [`ModelHandle`] onto a provider and rewrites the request's
//! model ref to it. Embeddings are not offered through providers yet.

use crate::backend::TokenStream;
use crate::{
    BackendCaps, InferenceBackend, InferenceError, LoadOptions, ModelHandle, ModelLocation,
    ModelSpec, Result,
};
use dn_core::chat::ChatRequest;
use dn_core::model::ModelRef;
use dn_providers::{ProviderAdapter, ProviderConfig, ProviderKind};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Endpoint {
    provider: Arc<dyn ProviderAdapter>,
    provider_id: String,
    model: String,
}

#[derive(Default)]
pub struct OpenAiCompatBackend {
    endpoints: Mutex<HashMap<u64, Endpoint>>,
    next_id: AtomicU64,
}

impl OpenAiCompatBackend {
    pub fn new() -> Self {
        Self::default()
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
        let provider = dn_providers::build(cfg).map_err(provider_err)?;
        self.endpoints
            .lock()
            .map_err(|_| InferenceError::Runtime("endpoint table poisoned".into()))?
            .insert(
                id,
                Endpoint {
                    provider,
                    provider_id,
                    model: model.clone(),
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

    async fn embed(&self, _h: &ModelHandle, _input: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(InferenceError::Unsupported {
            backend: "openai_compat",
            what: "embeddings (not offered by dn-providers yet)".into(),
        })
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
    use crate::{LoadedModel, next_event};
    use dn_core::chat::{ChatMessage, ChatModel, Role, StreamEvent};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One-shot SSE server answering "hi" and recording the request body.
    async fn server() -> (String, tokio::task::JoinHandle<String>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        let task = tokio::spawn(async move {
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
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                        data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                        data: [DONE]\n\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            req
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

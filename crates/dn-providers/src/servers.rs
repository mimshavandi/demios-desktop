//! What an OpenAI-compatible server says about its models (T-PRV-05). `/v1/models` lists
//! only ids, so tool-call and embedding support come from each server's own API:
//!
//! - LM Studio: `GET /api/v0/models` → `type` and `capabilities` (`tool_use`);
//! - Ollama: `POST /api/show` per model → `capabilities` (`tools`, `embedding`);
//! - llama.cpp `llama-server`: `GET /props` → `chat_template_caps.supports_tool_calls`.
//!
//! A server that answers none of these leaves `ModelInfo::tool_calls` at `None` ("doesn't
//! say"). Nothing is guessed from the model name, and no chat request is made to find out.

use crate::adapter::{Inner, ModelInfo, ServerFamily};
use serde_json::Value;
use std::time::Duration;

/// Per request; the whole annotation stays well under the UI's patience.
const TIMEOUT: Duration = Duration::from_secs(4);
/// Ollama needs one `/api/show` per model; more than this many are left unannotated.
const MAX_SHOW: usize = 64;

/// Fills `tool_calls` and `embedding` from the server's own API. `owned_by` is the first
/// `owned_by` of `/v1/models` (it names the server family). Errors leave fields at `None`.
pub(crate) async fn annotate(inner: &Inner, models: &mut [ModelInfo], owned_by: Option<&str>) {
    let family = owned_by
        .map(ServerFamily::from_owned_by)
        .unwrap_or_default();
    let root = inner
        .base
        .strip_suffix("/v1")
        .unwrap_or(&inner.base)
        .to_string();
    match family {
        ServerFamily::LmStudio => {
            lm_studio(inner, &root, models).await;
        }
        ServerFamily::Ollama => ollama(inner, &root, models).await,
        ServerFamily::LlamaCpp => {
            llama_cpp(inner, &root, models).await;
        }
        ServerFamily::Vllm => {}
        ServerFamily::Unknown => {
            if lm_studio(inner, &root, models).await {
                return;
            }
            if get_json(inner, &format!("{root}/api/version"))
                .await
                .is_some_and(|v| v["version"].is_string())
            {
                ollama(inner, &root, models).await;
                return;
            }
            llama_cpp(inner, &root, models).await;
        }
    }
}

/// `true` if the server answered as LM Studio.
async fn lm_studio(inner: &Inner, root: &str, models: &mut [ModelInfo]) -> bool {
    let Some(v) = get_json(inner, &format!("{root}/api/v0/models")).await else {
        return false;
    };
    let Some(data) = v["data"].as_array() else {
        return false;
    };
    for m in models.iter_mut() {
        let Some(e) = data.iter().find(|e| e["id"].as_str() == Some(&m.id)) else {
            continue;
        };
        if let Some(caps) = e["capabilities"].as_array() {
            m.tool_calls = Some(caps.iter().any(|c| c.as_str() == Some("tool_use")));
        }
        if let Some(t) = e["type"].as_str() {
            m.embedding = Some(t == "embeddings");
        }
        if m.context_window.is_none() {
            m.context_window = e["max_context_length"].as_u64().map(|n| n as u32);
        }
    }
    true
}

async fn ollama(inner: &Inner, root: &str, models: &mut [ModelInfo]) {
    for m in models.iter_mut().take(MAX_SHOW) {
        let body = serde_json::json!({ "model": m.id }).to_string();
        let rb = inner
            .client
            .post(format!("{root}/api/show"))
            .header("content-type", "application/json")
            .body(body);
        let Some(v) = send_json(inner, rb).await else {
            continue;
        };
        if let Some(caps) = v["capabilities"].as_array() {
            let has = |c: &str| caps.iter().any(|x| x.as_str() == Some(c));
            m.tool_calls = Some(has("tools"));
            m.embedding = Some(has("embedding"));
        }
    }
}

/// `llama-server` serves one model, so its template caps apply to every listed id.
async fn llama_cpp(inner: &Inner, root: &str, models: &mut [ModelInfo]) -> bool {
    let Some(v) = get_json(inner, &format!("{root}/props")).await else {
        return false;
    };
    let caps = &v["chat_template_caps"];
    let Some(tools) = caps["supports_tool_calls"]
        .as_bool()
        .or_else(|| caps["supports_tools"].as_bool())
    else {
        return false;
    };
    for m in models.iter_mut() {
        m.tool_calls = Some(tools);
    }
    true
}

async fn get_json(inner: &Inner, url: &str) -> Option<Value> {
    send_json(inner, inner.client.get(url)).await
}

async fn send_json(inner: &Inner, mut rb: reqwest::RequestBuilder) -> Option<Value> {
    if let Some(k) = inner.key() {
        rb = rb.bearer_auth(k);
    }
    let r = rb.timeout(TIMEOUT).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let bytes = tokio::time::timeout(TIMEOUT, r.bytes()).await.ok()?.ok()?;
    serde_json::from_slice(&bytes).ok()
}

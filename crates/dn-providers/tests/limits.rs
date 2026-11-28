//! A remote model's context size (ADR-0020 §2.1): from the server or provider, or the
//! published table; never guessed, and cached once known.

mod common;

use common::{Reply, serve};
use dn_core::chat::{LimitSource, ModelLimits};
use dn_core::model::ModelRef;
use dn_providers::{ProviderConfig, ProviderKind, ServerFamily, build};
use dn_secrets::Secret;

fn json(v: serde_json::Value) -> Reply {
    Reply::inline(200, "application/json", &v.to_string())
}

fn remote(endpoint: &str, model: &str) -> ModelRef {
    ModelRef::Remote {
        endpoint_id: endpoint.into(),
        model_id: model.into(),
    }
}

fn server(base: &str, family: ServerFamily) -> ProviderConfig {
    ProviderConfig::new("srv", ProviderKind::OpenAiCompatible)
        .with_base_url(base)
        .with_server_family(family)
}

#[tokio::test]
async fn llama_server_reports_the_context_it_loaded_and_its_cached() {
    let srv = serve(vec![json(serde_json::json!({
        "default_generation_settings": {"n_ctx": 16384}
    }))])
    .await;
    let a = build(server(&srv.base, ServerFamily::LlamaCpp)).unwrap();
    let m = remote("srv", "qwen3-8b");
    assert_eq!(
        a.limits(&m).await,
        ModelLimits::known(16384, LimitSource::Provider)
    );
    // Cached: no second request (the fixture server has no second reply).
    assert_eq!(
        a.limits(&m).await,
        ModelLimits::known(16384, LimitSource::Provider)
    );
    assert_eq!(srv.requests().len(), 1);
    assert_eq!(srv.requests()[0].path, "/props");
}

#[tokio::test]
async fn lm_studio_uses_the_loaded_context_not_the_maximum() {
    let srv = serve(vec![
        json(serde_json::json!({"id": "qwen", "max_context_length": 131072, "loaded_context_length": 8192})),
        json(serde_json::json!({"id": "gemma", "max_context_length": 131072})),
    ])
    .await;
    let a = build(server(&srv.base, ServerFamily::LmStudio)).unwrap();
    assert_eq!(
        a.limits(&remote("srv", "qwen")).await,
        ModelLimits::known(8192, LimitSource::Provider)
    );
    // Not loaded: only the maximum is known, so the size is unknown.
    assert_eq!(
        a.limits(&remote("srv", "gemma")).await,
        ModelLimits::default()
    );
    let reqs = srv.requests();
    assert_eq!(reqs[0].path, "/api/v0/models/qwen");
}

#[tokio::test]
async fn vllm_and_ollama_report_theirs() {
    let srv = serve(vec![json(serde_json::json!({"data": [
        {"id": "meta-llama/Llama-3.1-8B-Instruct", "max_model_len": 32768}
    ]}))])
    .await;
    let a = build(server(&srv.base, ServerFamily::Vllm)).unwrap();
    assert_eq!(
        a.limits(&remote("srv", "meta-llama/Llama-3.1-8B-Instruct"))
            .await,
        ModelLimits::known(32768, LimitSource::Provider)
    );
    let srv = serve(vec![json(serde_json::json!({"models": [
        {"name": "qwen3:8b", "model": "qwen3:8b", "context_length": 4096}
    ]}))])
    .await;
    let a = build(server(&srv.base, ServerFamily::Ollama)).unwrap();
    assert_eq!(
        a.limits(&remote("srv", "qwen3:8b")).await,
        ModelLimits::known(4096, LimitSource::Provider)
    );
    assert_eq!(srv.requests()[0].path, "/api/ps");
}

#[tokio::test]
async fn an_ollama_model_not_yet_loaded_uses_its_modelfile_num_ctx_and_nothing_else() {
    // Not in /api/ps; /api/show has num_ctx in its parameters.
    let srv = serve(vec![
        json(serde_json::json!({"models": []})),
        json(serde_json::json!({
            "parameters": "num_keep 24\nnum_ctx 8192\nstop \"<|im_end|>\"",
            "model_info": {"qwen3.context_length": 40960}
        })),
    ])
    .await;
    let a = build(server(&srv.base, ServerFamily::Ollama)).unwrap();
    let m = remote("srv", "qwen3:8b");
    assert_eq!(
        a.limits(&m).await,
        ModelLimits::known(8192, LimitSource::Provider)
    );
    // Known values are cached: no further requests.
    assert_eq!(
        a.limits(&m).await,
        ModelLimits::known(8192, LimitSource::Provider)
    );
    let reqs = srv.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        (reqs[0].path.as_str(), reqs[1].path.as_str()),
        ("/api/ps", "/api/show")
    );
    assert_eq!(reqs[1].method, "POST");
    assert_eq!(reqs[1].body["model"], "qwen3:8b");

    // No num_ctx: unknown, never the model's maximum (model_info) or a guessed default, and
    // not cached, so a later load is found.
    let srv = serve(vec![
        json(serde_json::json!({"models": []})),
        json(serde_json::json!({
            "parameters": "stop \"<|im_end|>\"",
            "model_info": {"qwen3.context_length": 40960}
        })),
        json(serde_json::json!({"models": [
            {"name": "qwen3:8b", "model": "qwen3:8b", "context_length": 4096}
        ]})),
    ])
    .await;
    let a = build(server(&srv.base, ServerFamily::Ollama)).unwrap();
    assert_eq!(a.limits(&m).await, ModelLimits::default());
    assert_eq!(
        a.limits(&m).await,
        ModelLimits::known(4096, LimitSource::Provider)
    );
}

#[tokio::test]
async fn an_unknown_server_or_failed_request_is_unknown() {
    let a = build(server("http://127.0.0.1:9", ServerFamily::Unknown)).unwrap();
    assert_eq!(a.limits(&remote("srv", "x")).await, ModelLimits::default());
    let srv = serve(vec![Reply::inline(500, "text/plain", "boom")]).await;
    let a = build(server(&srv.base, ServerFamily::LlamaCpp)).unwrap();
    assert_eq!(a.limits(&remote("srv", "x")).await, ModelLimits::default());
}

#[tokio::test]
async fn gemini_adds_input_and_output_limits() {
    let srv = serve(vec![json(serde_json::json!({
        "name": "models/gemini-2.5-flash", "inputTokenLimit": 1048576, "outputTokenLimit": 65536
    }))])
    .await;
    let cfg = ProviderConfig::new("gemini", ProviderKind::Gemini)
        .with_base_url(&srv.base)
        .with_key(Secret::new("test-key-redacted"));
    let a = build(cfg).unwrap();
    let m = ModelRef::Byok {
        provider_id: "gemini".into(),
        model_id: "gemini-2.5-flash".into(),
    };
    assert_eq!(
        a.limits(&m).await,
        ModelLimits::known(1_114_112, LimitSource::Provider)
    );
    assert_eq!(srv.requests()[0].path, "/models/gemini-2.5-flash");
}

#[tokio::test]
async fn anthropic_and_openai_use_the_published_table_without_a_request() {
    let cfg = ProviderConfig::new("anthropic", ProviderKind::Anthropic)
        .with_base_url("http://127.0.0.1:9")
        .with_key(Secret::new("test-key-redacted"));
    let a = build(cfg).unwrap();
    let m = |id: &str| ModelRef::Byok {
        provider_id: "anthropic".into(),
        model_id: id.into(),
    };
    assert_eq!(
        a.limits(&m("claude-sonnet-4-5-20250929")).await,
        ModelLimits::known(200_000, LimitSource::Published)
    );
    assert_eq!(
        a.limits(&m("claude-next-preview")).await,
        ModelLimits::default()
    );
    // Another provider's model: unknown here, not looked up.
    let other = ModelRef::Byok {
        provider_id: "openai".into(),
        model_id: "gpt-4o".into(),
    };
    assert_eq!(a.limits(&other).await, ModelLimits::default());
}

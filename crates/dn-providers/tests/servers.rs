//! OpenAI-compatible servers (T-PRV-05): per-model tool-call support from each server's own
//! API, the URL policy, and same-origin redirects.

mod common;

use common::{Reply, serve};
use dn_providers::{CoreError, ProviderConfig, ProviderKind, build, check_server_url};

fn compat(base: &str) -> ProviderConfig {
    ProviderConfig::new("srv", ProviderKind::OpenAiCompatible).with_base_url(base)
}

fn models(owned_by: &str, ids: &[&str]) -> Reply {
    let data: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({"id": id, "object": "model", "owned_by": owned_by}))
        .collect();
    Reply::inline(
        200,
        "application/json",
        &serde_json::json!({"object": "list", "data": data}).to_string(),
    )
}

fn json(v: serde_json::Value) -> Reply {
    Reply::inline(200, "application/json", &v.to_string())
}

fn not_found() -> Reply {
    Reply::inline(404, "text/plain", "404 page not found")
}

#[tokio::test]
async fn ollama_reports_tools_and_embeddings_per_model() {
    let srv = serve(vec![
        models("library", &["qwen3:8b", "nomic-embed-text:latest"]),
        json(serde_json::json!({"capabilities": ["completion", "tools", "thinking"]})),
        json(serde_json::json!({"capabilities": ["embedding"]})),
    ])
    .await;
    let a = build(compat(&srv.base)).unwrap();
    let list = a.list_models().await.unwrap();
    assert_eq!(list[0].tool_calls, Some(true));
    assert_eq!(list[0].embedding, Some(false));
    assert_eq!(list[1].tool_calls, Some(false));
    assert_eq!(list[1].embedding, Some(true));
    let reqs = srv.requests();
    assert_eq!(reqs[1].method, "POST");
    assert_eq!(reqs[1].path, "/api/show");
    assert_eq!(reqs[1].body["model"], "qwen3:8b");
    assert_eq!(reqs[2].body["model"], "nomic-embed-text:latest");
}

#[tokio::test]
async fn lm_studio_reports_tool_use_from_api_v0() {
    let srv = serve(vec![
        models("organization_owner", &["qwen2.5-7b-instruct", "gemma-3-4b", "text-embedding-nomic"]),
        json(serde_json::json!({"object": "list", "data": [
            {"id": "qwen2.5-7b-instruct", "type": "llm", "capabilities": ["tool_use"], "max_context_length": 32768},
            {"id": "gemma-3-4b", "type": "vlm", "capabilities": []},
            {"id": "text-embedding-nomic", "type": "embeddings"}
        ]})),
    ])
    .await;
    let list = build(compat(&srv.base))
        .unwrap()
        .list_models()
        .await
        .unwrap();
    assert_eq!(list[0].tool_calls, Some(true));
    assert_eq!(list[0].context_window, Some(32768));
    assert_eq!(list[1].tool_calls, Some(false));
    assert_eq!(
        list[2].tool_calls, None,
        "no capabilities field: the server doesn't say"
    );
    assert_eq!(list[2].embedding, Some(true));
    assert_eq!(srv.requests()[1].path, "/api/v0/models");
}

#[tokio::test]
async fn llama_server_reports_its_template_caps() {
    let srv = serve(vec![
        models("llamacpp", &["gpt-oss-20b"]),
        json(serde_json::json!({"chat_template_caps": {"supports_tool_calls": true}})),
    ])
    .await;
    let list = build(compat(&srv.base))
        .unwrap()
        .list_models()
        .await
        .unwrap();
    assert_eq!(list[0].tool_calls, Some(true));
    assert_eq!(srv.requests()[1].path, "/props");
}

#[tokio::test]
async fn a_server_that_says_nothing_leaves_tool_calls_unknown() {
    // Unknown family: LM Studio, Ollama and llama.cpp endpoints are each tried once.
    let srv = serve(vec![
        models("acme", &["mystery-model"]),
        not_found(),
        not_found(),
        not_found(),
    ])
    .await;
    let a = build(compat(&srv.base)).unwrap();
    let list = a.list_models().await.unwrap();
    assert_eq!(list[0].tool_calls, None);
    let paths: Vec<_> = srv.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        ["/v1/models", "/api/v0/models", "/api/version", "/props"]
    );
}

#[tokio::test]
async fn probe_capabilities_does_not_assume_tools_for_a_compatible_server() {
    let srv = serve(vec![
        models("acme", &["m"]),
        not_found(),
        not_found(),
        not_found(),
    ])
    .await;
    let caps = build(compat(&srv.base))
        .unwrap()
        .probe_capabilities("m")
        .await
        .unwrap();
    assert!(caps.chat);
    assert!(!caps.tools, "a server that doesn't say gets no tool calls");
}

#[tokio::test]
async fn redirects_stay_on_the_same_origin() {
    // Same origin: followed.
    let srv = serve(vec![
        Reply::redirect("/v1/models-moved"),
        models("acme", &["m"]),
    ])
    .await;
    // The annotation requests after the list find nothing (server closed): that's fine.
    let list = build(compat(&srv.base))
        .unwrap()
        .list_models()
        .await
        .unwrap();
    assert_eq!(list[0].id, "m");
    assert_eq!(srv.requests()[1].path, "/v1/models-moved");

    // Another origin (`localhost` vs `127.0.0.1`): refused, and the other server sees nothing.
    let other = serve(vec![models("acme", &["stolen"])]).await;
    let elsewhere = other.base.replace("127.0.0.1", "localhost");
    let srv = serve(vec![Reply::redirect(&format!("{elsewhere}/models"))]).await;
    let a = build(compat(&srv.base).with_key(dn_secrets::Secret::new("sk-test-not-real"))).unwrap();
    let err = a.list_models().await.unwrap_err();
    assert!(matches!(err, CoreError::ModelUnavailable { .. }), "{err:?}");
    assert!(
        other.requests().is_empty(),
        "the key must not reach another host"
    );
}

#[test]
fn remote_http_servers_are_refused_at_build() {
    let err = build(compat("http://api.example.com/v1")).err().unwrap();
    match err {
        CoreError::ModelUnavailable { user_message, .. } => {
            assert!(user_message.contains("needs https://"), "{user_message}")
        }
        other => panic!("{other:?}"),
    }
    assert!(build(compat("https://api.example.com/v1")).is_ok());
    assert!(build(compat("http://192.168.1.9:11434")).is_ok());
    assert_eq!(
        check_server_url("http://localhost:11434").unwrap().as_str(),
        "http://localhost:11434/v1"
    );
}

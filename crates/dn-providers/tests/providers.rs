//! End-to-end adapter tests against the fixture server (no network, no keys).

mod common;

use common::{Reply, serve};
use dn_core::chat::{
    ChatMessage, ChatRequest, FinishReason, Reasoning, Role, StreamEvent, ToolCall, ToolSpec, Usage,
};
use dn_core::{CoreError, RecoveryAction};
use dn_providers::{ProviderConfig, ProviderKind, build, next_event};
use dn_secrets::{MemoryStore, Secret, SecretId};
use serde_json::json;
use std::sync::Arc;

const FAKE_KEY: &str = "test-key-not-real";

fn cfg(id: &str, kind: ProviderKind, base: &str) -> ProviderConfig {
    ProviderConfig::new(id, kind)
        .with_base_url(base)
        .with_key(Secret::new(FAKE_KEY))
}

fn request(model: &str, caller: &str) -> ChatRequest {
    ChatRequest {
        model: model.parse().unwrap(),
        messages: vec![
            ChatMessage::text(Role::System, "Be brief."),
            ChatMessage::text(Role::User, "Say hello."),
        ],
        tools: vec![],
        max_tokens: Some(256),
        temperature: Some(0.2),
        response_schema: None,
        reasoning: Reasoning::Auto,
        caller: caller.into(),
    }
}

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: "get_weather".into(),
        description: "Current weather for a city".into(),
        input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
    }
}

/// A conversation that already ran one tool, so the adapters must map tool calls and
/// tool results back into the provider's format.
fn with_tool_history(mut req: ChatRequest, call_id: &str) -> ChatRequest {
    req.tools = vec![weather_tool()];
    req.messages.push(ChatMessage {
        role: Role::Assistant,
        content: vec![],
        tool_calls: vec![ToolCall {
            id: call_id.into(),
            name: "get_weather".into(),
            arguments: json!({"city": "Paris"}),
        }],
        tool_call_id: None,
    });
    req.messages.push(ChatMessage {
        role: Role::Tool,
        content: vec![dn_core::chat::ContentPart::Text {
            text: "18°C, cloudy".into(),
        }],
        tool_calls: vec![],
        tool_call_id: Some(call_id.into()),
    });
    req
}

async fn collect(
    model: &dyn dn_core::chat::ChatModel,
    req: ChatRequest,
) -> Result<Vec<StreamEvent>, CoreError> {
    let mut s = model.chat(req);
    let mut out = Vec::new();
    while let Some(ev) = next_event(&mut s).await {
        out.push(ev?);
    }
    Ok(out)
}

fn text(evs: &[StreamEvent]) -> String {
    evs.iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn reasoning(evs: &[StreamEvent]) -> String {
    evs.iter()
        .filter_map(|e| match e {
            StreamEvent::ReasoningDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn calls(evs: &[StreamEvent]) -> Vec<ToolCall> {
    evs.iter()
        .filter_map(|e| match e {
            StreamEvent::ToolCall { call } => Some(call.clone()),
            _ => None,
        })
        .collect()
}

fn done(evs: &[StreamEvent]) -> (FinishReason, Usage) {
    match evs.last() {
        Some(StreamEvent::Done {
            finish_reason,
            usage,
        }) => (*finish_reason, *usage),
        other => panic!("last event not Done: {other:?}"),
    }
}

fn text_deltas(evs: &[StreamEvent]) -> usize {
    evs.iter()
        .filter(|e| matches!(e, StreamEvent::TextDelta { .. }))
        .count()
}

// ---------- OpenAI ----------

#[tokio::test]
async fn openai_streams_text() {
    let srv = serve(vec![Reply::sse("openai/text.sse")]).await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let evs = collect(&*p, request("byok:openai:gpt-4o-mini", "chat.reply"))
        .await
        .unwrap();
    assert_eq!(text(&evs), "Hello from OpenAI.");
    assert_eq!(text_deltas(&evs), 3);
    assert_eq!(
        done(&evs),
        (
            FinishReason::Stop,
            Usage {
                input_tokens: 12,
                output_tokens: 4
            }
        )
    );
    let r = &srv.requests()[0];
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("POST", "/chat/completions")
    );
    assert_eq!(
        r.header("authorization"),
        Some(&*format!("Bearer {FAKE_KEY}"))
    );
    assert_eq!(r.body["model"], "gpt-4o-mini");
    assert_eq!(r.body["stream"], true);
    assert_eq!(r.body["max_completion_tokens"], 256);
    assert_eq!(
        r.body["messages"][0],
        json!({"role": "system", "content": "Be brief."})
    );
    // Not a reasoning model: no reasoning_effort.
    assert!(r.body.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn openai_tool_call_round_trip() {
    let srv = serve(vec![Reply::sse("openai/tool_call.sse")]).await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let req = with_tool_history(
        request("byok:openai:gpt-4o-mini", "chat.reply"),
        "call_prev",
    );
    let evs = collect(&*p, req).await.unwrap();
    assert_eq!(
        calls(&evs),
        vec![ToolCall {
            id: "call_REDACTED01".into(),
            name: "get_weather".into(),
            arguments: json!({"city": "Berlin"})
        }]
    );
    assert_eq!(done(&evs).0, FinishReason::ToolCalls);
    let b = &srv.requests()[0].body;
    assert_eq!(b["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(b["messages"][2]["tool_calls"][0]["id"], "call_prev");
    assert_eq!(
        b["messages"][2]["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"Paris"}"#
    );
    assert_eq!(
        b["messages"][3],
        json!({"role": "tool", "tool_call_id": "call_prev", "content": "18°C, cloudy"})
    );
}

#[tokio::test]
async fn openai_reasoning_model_policy() {
    let srv = serve(vec![
        Reply::sse("openai/text.sse"),
        Reply::sse("openai/text.sse"),
        Reply::sse("openai/text.sse"),
    ])
    .await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    // Prose caller → thinking on; temperature dropped for reasoning models.
    collect(&*p, request("byok:openai:gpt-5-mini", "chat.reply"))
        .await
        .unwrap();
    // Non-prose caller → off.
    collect(&*p, request("byok:openai:gpt-5-mini", "chat.title"))
        .await
        .unwrap();
    // Structured output → off even for a prose caller.
    let mut r = request("byok:openai:gpt-5-mini", "chat.reply");
    r.response_schema = Some(json!({"type": "object"}));
    collect(&*p, r).await.unwrap();
    let reqs = srv.requests();
    assert_eq!(reqs[0].body["reasoning_effort"], "medium");
    assert!(reqs[0].body.get("temperature").is_none());
    assert_eq!(reqs[1].body["reasoning_effort"], "minimal");
    assert_eq!(reqs[2].body["reasoning_effort"], "minimal");
    assert_eq!(reqs[2].body["response_format"]["type"], "json_schema");
}

#[tokio::test]
async fn openai_bad_key_is_model_unavailable_with_add_key_and_no_key_echo() {
    let srv = serve(vec![Reply::json(401, "openai/unauthorized.json")]).await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let err = collect(&*p, request("byok:openai:gpt-4o-mini", "chat.reply"))
        .await
        .unwrap_err();
    match err {
        CoreError::ModelUnavailable {
            recovery,
            developer_message,
            ..
        } => {
            assert_eq!(recovery, vec![RecoveryAction::AddProviderKey]);
            assert!(!developer_message.contains(FAKE_KEY));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn openai_list_models_and_validate() {
    let srv = serve(vec![
        Reply::json(200, "openai/models.json"),
        Reply::json(200, "openai/models.json"),
        Reply::json(401, "openai/unauthorized.json"),
    ])
    .await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let models = p.list_models().await.unwrap();
    assert_eq!(models.len(), 3);
    let caps = p.probe_capabilities("gpt-5-mini").await.unwrap();
    assert!(caps.chat && caps.tools && caps.reasoning);
    assert!(matches!(
        p.validate().await,
        Err(CoreError::ModelUnavailable { .. })
    ));
    assert_eq!(srv.requests()[0].path, "/models");
}

#[tokio::test]
async fn stream_is_incremental_and_drop_cancels() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut reply = Reply::sse("openai/text.sse");
    reply.gate = Some(gate.clone());
    let srv = serve(vec![reply]).await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let mut s = p.chat(request("byok:openai:gpt-4o-mini", "chat.reply"));
    // The server holds the rest of the body until `gate` fires, so getting an event now
    // proves we parse while the response is still open.
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), next_event(&mut s))
        .await
        .expect("first event before body end");
    assert_eq!(
        first,
        Some(Ok(StreamEvent::TextDelta {
            text: "Hello".into()
        }))
    );
    drop(s); // cancels the HTTP call
    gate.notify_one();
}

// ---------- OpenAI-compatible ----------

#[tokio::test]
async fn compatible_without_key_streams_reasoning_and_drops_unsupported_param_once() {
    let srv = serve(vec![
        Reply::json(400, "openai_compat/unsupported_param.json"),
        Reply::sse("openai_compat/reasoning_text.sse"),
    ])
    .await;
    let p = build(
        ProviderConfig::new("ollama", ProviderKind::OpenAiCompatible)
            .with_base_url(format!("{}/v1", srv.base)),
    )
    .unwrap();
    let evs = collect(&*p, request("remote:ollama:qwen3:8b", "chat.reply"))
        .await
        .unwrap();
    assert_eq!(text(&evs), "Hi there!");
    assert_eq!(reasoning(&evs), "The user greets me.");
    assert_eq!(done(&evs).1.output_tokens, 11);
    let reqs = srv.requests();
    assert_eq!(reqs.len(), 2, "retried exactly once");
    assert_eq!(reqs[0].path, "/v1/chat/completions");
    assert!(reqs[0].header("authorization").is_none());
    assert_eq!(reqs[0].body["reasoning_effort"], "medium");
    assert!(reqs[1].body.get("reasoning_effort").is_none());
    assert_eq!(reqs[1].body["max_tokens"], 256);
    assert_eq!(reqs[1].body["temperature"], 0.2);
}

#[tokio::test]
async fn second_unsupported_param_is_not_retried_again() {
    let srv = serve(vec![
        Reply::json(400, "openai_compat/unsupported_param.json"),
        Reply::json(400, "openai_compat/unsupported_param.json"),
    ])
    .await;
    let p = build(
        ProviderConfig::new("ollama", ProviderKind::OpenAiCompatible).with_base_url(&srv.base),
    )
    .unwrap();
    let err = collect(&*p, request("remote:ollama:qwen3:8b", "chat.reply"))
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::ModelUnavailable { .. }));
    assert_eq!(srv.requests().len(), 2);
}

#[tokio::test]
async fn request_for_another_provider_is_rejected_not_rerouted() {
    let srv = serve(vec![]).await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let err = collect(
        &*p,
        request("byok:anthropic:claude-sonnet-4-5", "chat.reply"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CoreError::ModelUnavailable { .. }));
    assert!(srv.requests().is_empty());
}

// ---------- Anthropic ----------

#[tokio::test]
async fn anthropic_streams_text_and_thinking() {
    let srv = serve(vec![Reply::sse("anthropic/text.sse")]).await;
    let p = build(cfg("anthropic", ProviderKind::Anthropic, &srv.base)).unwrap();
    let evs = collect(
        &*p,
        request("byok:anthropic:claude-sonnet-4-5", "chat.reply"),
    )
    .await
    .unwrap();
    assert_eq!(text(&evs), "Hello from Claude.");
    assert_eq!(reasoning(&evs), "A short greeting is enough.");
    assert_eq!(
        done(&evs),
        (
            FinishReason::Stop,
            Usage {
                input_tokens: 21,
                output_tokens: 18
            }
        )
    );
    let r = &srv.requests()[0];
    assert_eq!(r.path, "/messages");
    assert_eq!(r.header("x-api-key"), Some(FAKE_KEY));
    assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(r.body["system"], "Be brief.");
    assert_eq!(r.body["messages"][0]["role"], "user");
    assert_eq!(r.body["thinking"]["type"], "enabled");
    assert!(
        r.body["max_tokens"].as_u64().unwrap()
            > r.body["thinking"]["budget_tokens"].as_u64().unwrap()
    );
    // Thinking requires default temperature.
    assert!(r.body.get("temperature").is_none());
}

#[tokio::test]
async fn anthropic_tool_call_round_trip() {
    let srv = serve(vec![Reply::sse("anthropic/tool_call.sse")]).await;
    let p = build(cfg("anthropic", ProviderKind::Anthropic, &srv.base)).unwrap();
    let req = with_tool_history(
        request("byok:anthropic:claude-sonnet-4-5", "chat.reply"),
        "toolu_prev",
    );
    let evs = collect(&*p, req).await.unwrap();
    assert_eq!(text(&evs), "Let me check.");
    assert_eq!(
        calls(&evs),
        vec![ToolCall {
            id: "toolu_REDACTED01".into(),
            name: "get_weather".into(),
            arguments: json!({"city": "Berlin"})
        }]
    );
    assert_eq!(
        done(&evs),
        (
            FinishReason::ToolCalls,
            Usage {
                input_tokens: 402,
                output_tokens: 54
            }
        )
    );
    let b = &srv.requests()[0].body;
    assert_eq!(b["tools"][0]["input_schema"]["required"][0], "city");
    assert_eq!(
        b["messages"][1],
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_prev", "name": "get_weather", "input": {"city": "Paris"}}]})
    );
    assert_eq!(
        b["messages"][2],
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_prev", "content": "18°C, cloudy"}]})
    );
    // Tool continuation runs without thinking (no signatures to send back).
    assert!(b.get("thinking").is_none());
}

#[tokio::test]
async fn anthropic_stream_error_is_retryable_model_unavailable() {
    let srv = serve(vec![Reply::sse("anthropic/overloaded.sse")]).await;
    let p = build(cfg("anthropic", ProviderKind::Anthropic, &srv.base)).unwrap();
    let err = collect(
        &*p,
        request("byok:anthropic:claude-3-5-haiku-latest", "chat.title"),
    )
    .await
    .unwrap_err();
    match err {
        CoreError::ModelUnavailable { recovery, .. } => {
            assert!(matches!(recovery[0], RecoveryAction::Retry { .. }))
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn anthropic_list_models() {
    let srv = serve(vec![Reply::json(200, "anthropic/models.json")]).await;
    let p = build(cfg("anthropic", ProviderKind::Anthropic, &srv.base)).unwrap();
    let m = p.list_models().await.unwrap();
    assert_eq!(m[0].display_name.as_deref(), Some("Claude Sonnet 4.5"));
    assert_eq!(srv.requests()[0].path, "/models?limit=1000");
}

// ---------- Gemini ----------

#[tokio::test]
async fn gemini_streams_text_and_thoughts() {
    let srv = serve(vec![Reply::sse("gemini/text.sse")]).await;
    let p = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    let evs = collect(&*p, request("byok:gemini:gemini-2.5-flash", "chat.reply"))
        .await
        .unwrap();
    assert_eq!(text(&evs), "Hello from Gemini.");
    assert_eq!(reasoning(&evs), "Planning a short reply.");
    assert_eq!(
        done(&evs),
        (
            FinishReason::Stop,
            Usage {
                input_tokens: 8,
                output_tokens: 24
            }
        )
    );
    let r = &srv.requests()[0];
    assert_eq!(
        r.path,
        "/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
    assert_eq!(r.header("x-goog-api-key"), Some(FAKE_KEY));
    assert!(!r.path.contains(FAKE_KEY), "key must not be in the URL");
    assert_eq!(r.body["systemInstruction"]["parts"][0]["text"], "Be brief.");
    assert_eq!(
        r.body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        -1
    );
    assert_eq!(r.body["generationConfig"]["maxOutputTokens"], 256);
}

#[tokio::test]
async fn gemini_tool_call_keeps_thought_signature_through_the_id() {
    let srv = serve(vec![
        Reply::sse("gemini/tool_call.sse"),
        Reply::sse("gemini/text.sse"),
    ])
    .await;
    let p = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    let mut req = request("byok:gemini:gemini-2.5-flash", "chat.title");
    req.tools = vec![weather_tool()];
    let evs = collect(&*p, req.clone()).await.unwrap();
    let c = calls(&evs);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].name, "get_weather");
    assert_eq!(c[0].arguments, json!({"city": "Berlin"}));
    assert_eq!(done(&evs).0, FinishReason::ToolCalls);
    // Feed the call and its result back.
    let req2 = with_tool_history(req, &c[0].id);
    collect(&*p, req2).await.unwrap();
    let reqs = srv.requests();
    assert_eq!(
        reqs[0].body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        0
    );
    assert_eq!(
        reqs[0].body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
        "object"
    );
    let contents = &reqs[1].body["contents"];
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(
        contents[1]["parts"][0]["functionCall"]["name"],
        "get_weather"
    );
    assert_eq!(
        contents[1]["parts"][0]["thoughtSignature"],
        "c2lnLVJFREFDVEVE"
    );
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"],
        json!({"name": "get_weather", "response": {"content": "18°C, cloudy"}})
    );
}

#[tokio::test]
async fn gemini_list_models_and_caps() {
    let srv = serve(vec![
        Reply::json(200, "gemini/models.json"),
        Reply::json(200, "gemini/models.json"),
    ])
    .await;
    let p = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    let m = p.list_models().await.unwrap();
    assert_eq!(m[0].id, "gemini-2.5-flash");
    assert_eq!(m[0].context_window, Some(1_048_576));
    let caps = p.probe_capabilities("gemini-embedding-001").await.unwrap();
    assert!(caps.embedding && !caps.chat);
}

// ---------- config ----------

#[tokio::test]
async fn key_from_async_store() {
    let store = dn_secrets::AsyncSecrets::new(Arc::new(MemoryStore::with([(
        SecretId::provider("gemini"),
        Secret::new(FAKE_KEY),
    )])));
    let c = ProviderConfig::new("gemini", ProviderKind::Gemini)
        .with_key_from_async(&store)
        .await
        .unwrap();
    assert_eq!(c.api_key.unwrap().expose(), FAKE_KEY);
    let missing = ProviderConfig::new("openai", ProviderKind::OpenAi)
        .with_key_from_async(&store)
        .await;
    assert!(matches!(missing, Err(CoreError::ModelUnavailable { .. })));
}

#[test]
fn hosted_provider_without_key_needs_add_provider_key() {
    let store = MemoryStore::new();
    let err = ProviderConfig::new("openai", ProviderKind::OpenAi)
        .with_key_from(&store)
        .unwrap_err();
    assert!(
        matches!(err, CoreError::ModelUnavailable { ref recovery, .. } if recovery == &vec![RecoveryAction::AddProviderKey])
    );
    assert!(build(ProviderConfig::new("openai", ProviderKind::OpenAi)).is_err());
    // Key from the store works; compatible endpoints don't need one.
    let store = MemoryStore::with([(SecretId::provider("openai"), Secret::new(FAKE_KEY))]);
    let c = ProviderConfig::new("openai", ProviderKind::OpenAi)
        .with_key_from(&store)
        .unwrap();
    assert_eq!(c.api_key.unwrap().expose(), FAKE_KEY);
    assert!(
        ProviderConfig::new("lmstudio", ProviderKind::OpenAiCompatible)
            .with_key_from(&MemoryStore::new())
            .is_ok()
    );
    assert!(
        build(ProviderConfig::new(
            "lmstudio",
            ProviderKind::OpenAiCompatible
        ))
        .is_err(),
        "needs base_url"
    );
}

// ---------- review: robustness ----------

fn assert_retryable_unavailable(err: CoreError) {
    match err {
        CoreError::ModelUnavailable { recovery, .. } => {
            assert!(
                matches!(recovery[0], RecoveryAction::Retry { .. }),
                "{recovery:?}"
            )
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn openai_parallel_tool_calls_keep_order_and_ids() {
    let srv = serve(vec![Reply::sse("openai/parallel_tool_calls.sse")]).await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let mut req = request("byok:openai:gpt-4o-mini", "chat.reply");
    req.tools = vec![weather_tool()];
    let evs = collect(&*p, req).await.unwrap();
    let c = calls(&evs);
    let got: Vec<(&str, &serde_json::Value)> =
        c.iter().map(|c| (c.id.as_str(), &c.arguments)).collect();
    assert_eq!(
        got,
        [
            ("call_REDACTED_A", &json!({"city": "Berlin"})),
            ("call_REDACTED_B", &json!({"city": "Paris"}))
        ]
    );
    assert_eq!(done(&evs).0, FinishReason::ToolCalls);
}

#[tokio::test]
async fn missing_call_ids_get_unique_ids_across_calls() {
    let srv = serve(vec![
        Reply::sse("openai_compat/no_ids_tool_calls.sse"),
        Reply::sse("openai_compat/no_ids_tool_calls.sse"),
        Reply::sse("gemini/parallel_tool_calls.sse"),
        Reply::sse("gemini/parallel_tool_calls.sse"),
    ])
    .await;
    let compat = build(
        ProviderConfig::new("ollama", ProviderKind::OpenAiCompatible).with_base_url(&srv.base),
    )
    .unwrap();
    let gem = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let evs = collect(&*compat, request("remote:ollama:qwen3:8b", "chat.reply"))
            .await
            .unwrap();
        ids.extend(calls(&evs).into_iter().map(|c| c.id));
    }
    for _ in 0..2 {
        let evs = collect(&*gem, request("byok:gemini:gemini-2.0-flash", "chat.reply"))
            .await
            .unwrap();
        let c = calls(&evs);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].arguments, json!({"city": "Paris"}));
        ids.extend(c.into_iter().map(|c| c.id));
    }
    assert_eq!(ids.len(), 6);
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 6, "ids repeat: {ids:?}");
    assert!(
        ids[0].starts_with("call_") && ids[2].starts_with("gm_"),
        "{ids:?}"
    );
}

#[tokio::test]
async fn mid_stream_errors_surface_after_partial_text() {
    let srv = serve(vec![
        Reply::sse("openai/midstream_error.sse"),
        Reply::sse("gemini/midstream_error.sse"),
    ])
    .await;
    let oa = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let gm = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    for (p, model) in [
        (&oa, "byok:openai:gpt-4o-mini"),
        (&gm, "byok:gemini:gemini-2.5-flash"),
    ] {
        let mut s = p.chat(request(model, "chat.reply"));
        assert_eq!(
            next_event(&mut s).await.unwrap().unwrap(),
            StreamEvent::TextDelta { text: "Hel".into() }
        );
        assert_retryable_unavailable(next_event(&mut s).await.unwrap().unwrap_err());
        assert!(next_event(&mut s).await.is_none());
    }
}

#[tokio::test]
async fn clean_eof_without_end_marker_is_an_error_for_every_provider() {
    let srv = serve(vec![
        Reply::sse("openai/truncated.sse"),
        Reply::sse("anthropic/truncated.sse"),
        Reply::sse("gemini/truncated.sse"),
    ])
    .await;
    for (id, kind, model) in [
        ("openai", ProviderKind::OpenAi, "byok:openai:gpt-4o-mini"),
        (
            "anthropic",
            ProviderKind::Anthropic,
            "byok:anthropic:claude-3-5-haiku-latest",
        ),
        (
            "gemini",
            ProviderKind::Gemini,
            "byok:gemini:gemini-2.5-flash",
        ),
    ] {
        let p = build(cfg(id, kind, &srv.base)).unwrap();
        let mut s = p.chat(request(model, "chat.title"));
        let mut got_text = false;
        let err = loop {
            match next_event(&mut s).await {
                Some(Ok(StreamEvent::TextDelta { .. })) => got_text = true,
                Some(Ok(StreamEvent::Done { .. })) => {
                    panic!("{id}: truncated stream reported Done")
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => break e,
                None => panic!("{id}: stream ended without error"),
            }
        };
        assert!(got_text, "{id}");
        assert_retryable_unavailable(err);
    }
}

#[tokio::test]
async fn length_finish_keeps_length_and_drops_partial_tool_calls() {
    let srv = serve(vec![Reply::sse("openai/length_tool_call.sse")]).await;
    let p = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let evs = collect(&*p, request("byok:openai:gpt-4o-mini", "chat.reply"))
        .await
        .unwrap();
    assert!(calls(&evs).is_empty());
    assert_eq!(done(&evs).0, FinishReason::Length);
}

#[tokio::test]
async fn gemini_malformed_function_call_is_error_finish() {
    let srv = serve(vec![Reply::sse("gemini/malformed_call.sse")]).await;
    let p = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    let evs = collect(&*p, request("byok:gemini:gemini-2.5-flash", "chat.reply"))
        .await
        .unwrap();
    assert_eq!(done(&evs).0, FinishReason::Error);
}

#[tokio::test]
async fn rejected_response_schema_moves_into_the_prompt_instead_of_vanishing() {
    let srv = serve(vec![
        Reply::json(400, "openai/schema_unsupported.json"),
        Reply::sse("openai/text.sse"),
        Reply::json(400, "gemini/schema_unsupported.json"),
        Reply::sse("gemini/text.sse"),
    ])
    .await;
    let schema = json!({"type": "object", "properties": {"title": {"type": "string"}}});
    let oa = build(cfg("openai", ProviderKind::OpenAi, &srv.base)).unwrap();
    let mut r = request("byok:openai:gpt-4o-mini", "chat.title");
    r.response_schema = Some(schema.clone());
    collect(&*oa, r).await.unwrap();
    let gm = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    let mut r = request("byok:gemini:gemini-2.5-flash", "chat.title");
    r.response_schema = Some(schema.clone());
    collect(&*gm, r).await.unwrap();
    let reqs = srv.requests();
    let schema_text = schema.to_string();
    // OpenAI: retried without response_format, schema appended to the system message.
    assert!(reqs[0].body.get("response_format").is_some());
    assert!(reqs[1].body.get("response_format").is_none());
    let sys = reqs[1].body["messages"][0]["content"].as_str().unwrap();
    assert!(
        sys.starts_with("Be brief.") && sys.contains(&schema_text),
        "{sys}"
    );
    // Gemini: same, in systemInstruction.
    assert!(
        reqs[3].body["generationConfig"]
            .get("responseJsonSchema")
            .is_none()
    );
    let parts = &reqs[3].body["systemInstruction"]["parts"];
    assert!(
        parts[1]["text"].as_str().unwrap().contains(&schema_text),
        "{parts}"
    );
}

#[tokio::test]
async fn reasoning_on_with_schema_does_not_think() {
    let srv = serve(vec![Reply::sse("gemini/text.sse")]).await;
    let p = build(cfg("gemini", ProviderKind::Gemini, &srv.base)).unwrap();
    let mut r = request("byok:gemini:gemini-2.5-flash", "chat.reply");
    r.reasoning = Reasoning::On;
    r.response_schema = Some(json!({"type": "object"}));
    collect(&*p, r).await.unwrap();
    assert_eq!(
        srv.requests()[0].body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        0
    );
}

// ---------- Stream timeouts (T-PRV-04) ----------

fn compat(srv: &common::Server) -> ProviderConfig {
    ProviderConfig::new("local", ProviderKind::OpenAiCompatible).with_base_url(&srv.base)
}

fn retryable_unavailable(e: &CoreError, needle: &str) {
    match e {
        CoreError::ModelUnavailable {
            developer_message,
            recovery,
            ..
        } => {
            assert!(developer_message.contains(needle), "{developer_message}");
            assert!(
                recovery
                    .iter()
                    .any(|r| matches!(r, RecoveryAction::Retry { .. })),
                "{recovery:?}"
            );
        }
        other => panic!("expected ModelUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn stream_that_goes_silent_ends_with_retryable_error() {
    use std::time::Duration;
    let mut r = Reply::sse("openai_compat/reasoning_text.sse");
    // Two events, then silence forever.
    r.gate = Some(Arc::new(tokio::sync::Notify::new()));
    let srv = serve(vec![r]).await;
    let p = build(
        compat(&srv).with_stream_timeouts(Duration::from_secs(5), Duration::from_millis(200)),
    )
    .unwrap();
    let t0 = std::time::Instant::now();
    let err = collect(&*p, request("remote:local:qwen3:8b", "chat.reply"))
        .await
        .unwrap_err();
    retryable_unavailable(&err, "idle timeout");
    assert!(t0.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn no_first_byte_within_first_byte_timeout_is_retryable() {
    use std::time::Duration;
    let mut r = Reply::sse("openai_compat/reasoning_text.sse");
    r.hold = Some(Duration::from_secs(5));
    let srv = serve(vec![r]).await;
    let p = build(
        compat(&srv).with_stream_timeouts(Duration::from_millis(200), Duration::from_secs(5)),
    )
    .unwrap();
    let err = collect(&*p, request("remote:local:qwen3:8b", "chat.reply"))
        .await
        .unwrap_err();
    retryable_unavailable(&err, "first-byte timeout");
}

#[tokio::test]
async fn slow_first_token_is_allowed_longer_than_idle() {
    use std::time::Duration;
    // A reasoning model that thinks for 400 ms before its first byte: fine with a 2 s
    // first-byte allowance even though the idle timeout is only 100 ms.
    let mut r = Reply::sse("openai_compat/reasoning_text.sse");
    r.hold = Some(Duration::from_millis(400));
    let srv = serve(vec![r]).await;
    let p = build(
        compat(&srv).with_stream_timeouts(Duration::from_secs(2), Duration::from_millis(100)),
    )
    .unwrap();
    let evs = collect(&*p, request("remote:local:qwen3:8b", "chat.reply"))
        .await
        .unwrap();
    assert_eq!(text(&evs), "Hi there!");
}

// ---------- Thinking switch per local server family (T-PRV-04) ----------

async fn body_sent(family: dn_providers::ServerFamily, caller: &str) -> serde_json::Value {
    let srv = serve(vec![Reply::sse("openai_compat/reasoning_text.sse")]).await;
    let p = build(compat(&srv).with_server_family(family)).unwrap();
    collect(&*p, request("remote:local:qwen3:8b", caller))
        .await
        .unwrap();
    srv.requests()[0].body.clone()
}

#[tokio::test]
async fn thinking_switch_follows_policy_per_server_family() {
    use dn_providers::ServerFamily::*;
    // `chat.reply` is a prose caller (thinking on under Auto); `extract.fields` is not.
    for fam in [LlamaCpp, Vllm] {
        let on = body_sent(fam, "chat.reply").await;
        assert_eq!(
            on["chat_template_kwargs"],
            json!({"enable_thinking": true}),
            "{fam:?}"
        );
        assert!(on.get("reasoning_effort").is_none() && on.get("think").is_none());
        let off = body_sent(fam, "extract.fields").await;
        assert_eq!(
            off["chat_template_kwargs"],
            json!({"enable_thinking": false}),
            "{fam:?}"
        );
    }
    let on = body_sent(Ollama, "chat.reply").await;
    assert_eq!(on["think"], true);
    assert!(on.get("chat_template_kwargs").is_none() && on.get("reasoning_effort").is_none());
    assert_eq!(body_sent(Ollama, "extract.fields").await["think"], false);
    // Unknown servers and LM Studio get nothing server-specific.
    for fam in [Unknown, LmStudio] {
        let b = body_sent(fam, "chat.reply").await;
        assert!(
            b.get("chat_template_kwargs").is_none() && b.get("think").is_none(),
            "{fam:?}"
        );
    }
}

#[tokio::test]
async fn response_schema_turns_thinking_off_on_local_servers() {
    let srv = serve(vec![Reply::sse("openai_compat/reasoning_text.sse")]).await;
    let p = build(compat(&srv).with_server_family(dn_providers::ServerFamily::LlamaCpp)).unwrap();
    let mut req = request("remote:local:qwen3:8b", "chat.reply");
    req.response_schema = Some(json!({"type": "object"}));
    collect(&*p, req).await.unwrap();
    assert_eq!(
        srv.requests()[0].body["chat_template_kwargs"]["enable_thinking"],
        false
    );
}

#[tokio::test]
async fn ollama_model_without_thinking_drops_the_switch_and_retries() {
    let srv = serve(vec![
        Reply::json(400, "openai_compat/think_unsupported.json"),
        Reply::sse("openai_compat/reasoning_text.sse"),
    ])
    .await;
    let p = build(compat(&srv).with_server_family(dn_providers::ServerFamily::Ollama)).unwrap();
    collect(&*p, request("remote:local:llama3.2", "chat.reply"))
        .await
        .unwrap();
    let reqs = srv.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].body["think"], true);
    assert!(reqs[1].body.get("think").is_none());
}

#[tokio::test]
async fn detects_server_family() {
    use dn_providers::{ServerFamily, detect_server_family};
    let models = |owner: &str| {
        Reply::inline(
            200,
            "application/json",
            &format!(
                r#"{{"object":"list","data":[{{"id":"m","object":"model","owned_by":"{owner}"}}]}}"#
            ),
        )
    };
    let srv = serve(vec![models("llamacpp")]).await;
    assert_eq!(
        detect_server_family(&compat(&srv)).await,
        ServerFamily::LlamaCpp
    );
    let srv = serve(vec![models("vllm")]).await;
    assert_eq!(
        detect_server_family(&compat(&srv)).await,
        ServerFamily::Vllm
    );
    // Ollama: unhelpful owner, but its native /api/version answers.
    let srv = serve(vec![
        models("someone"),
        Reply::inline(200, "application/json", r#"{"version":"0.12.0"}"#),
    ])
    .await;
    let cfg = ProviderConfig::new("o", ProviderKind::OpenAiCompatible)
        .with_base_url(format!("{}/v1", srv.base));
    assert_eq!(detect_server_family(&cfg).await, ServerFamily::Ollama);
    assert_eq!(srv.requests()[1].path, "/api/version");
    let srv = serve(vec![
        models("someone"),
        Reply::inline(404, "application/json", "{}"),
    ])
    .await;
    assert_eq!(
        detect_server_family(&compat(&srv)).await,
        ServerFamily::Unknown
    );
}

#[tokio::test]
async fn server_withholding_headers_hits_first_byte_timeout() {
    use std::time::Duration;
    // Ollama-style: nothing at all (not even the status line) until the first token.
    let mut r = Reply::sse("openai_compat/reasoning_text.sse");
    r.hold_head = Some(Duration::from_secs(5));
    let srv = serve(vec![r]).await;
    let p = build(
        compat(&srv).with_stream_timeouts(Duration::from_millis(200), Duration::from_secs(5)),
    )
    .unwrap();
    let t0 = std::time::Instant::now();
    let err = collect(&*p, request("remote:local:qwen3:8b", "chat.reply"))
        .await
        .unwrap_err();
    retryable_unavailable(&err, "first-byte timeout");
    assert!(t0.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn idle_timer_resets_on_every_chunk() {
    use std::time::Duration;
    // Pieces 80 ms apart for well over 150 ms in total, with a 150 ms idle timeout.
    let mut r = Reply::sse("openai_compat/reasoning_text.sse");
    r.pace = Some(Duration::from_millis(80));
    let srv = serve(vec![r]).await;
    let p = build(
        compat(&srv).with_stream_timeouts(Duration::from_secs(2), Duration::from_millis(150)),
    )
    .unwrap();
    let t0 = std::time::Instant::now();
    let evs = collect(&*p, request("remote:local:qwen3:8b", "chat.reply"))
        .await
        .unwrap();
    assert_eq!(text(&evs), "Hi there!");
    assert!(
        t0.elapsed() > Duration::from_millis(150),
        "stream must outlast one idle period"
    );
}

#[tokio::test]
async fn official_openai_never_gets_a_local_thinking_switch() {
    let srv = serve(vec![Reply::sse("openai/text.sse")]).await;
    let p = build(
        cfg("openai", ProviderKind::OpenAi, &srv.base)
            .with_server_family(dn_providers::ServerFamily::LlamaCpp),
    )
    .unwrap();
    collect(&*p, request("byok:openai:gpt-4o-mini", "chat.reply"))
        .await
        .unwrap();
    let b = &srv.requests()[0].body;
    assert!(b.get("chat_template_kwargs").is_none(), "{b}");
    assert!(b.get("think").is_none(), "{b}");
}

#[tokio::test]
async fn detection_skips_non_compatible_providers() {
    use dn_providers::{ServerFamily, detect_server_family};
    let srv = serve(vec![]).await;
    let c = cfg("openai", ProviderKind::OpenAi, &srv.base);
    assert_eq!(detect_server_family(&c).await, ServerFamily::Unknown);
    assert!(srv.requests().is_empty());
}

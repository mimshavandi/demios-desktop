//! Tests against a real GGUF. Ignored by default so CI never downloads a model.
//!
//! Run locally with a small chat model (e.g. `dn models pull qwen2.5-0.5b-instruct-q4_k_m`):
//!
//! ```sh
//! DN_TEST_GGUF=/path/to/qwen2.5-0.5b-instruct-q4_k_m.gguf \
//!   cargo test -p dn-inference --release -- --ignored --test-threads=1
//! ```
#![cfg(feature = "llamacpp")]

use dn_core::chat::{
    ChatMessage, ChatModel, ChatRequest, FinishReason, Role, StreamEvent, ToolSpec,
};
use dn_inference::llamacpp::active_generations;
use dn_inference::{LlamaCppBackend, LoadOptions, LoadedModel, ModelSpec, next_event};
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn model() -> LoadedModel<LlamaCppBackend> {
    let path = std::env::var("DN_TEST_GGUF").expect("set DN_TEST_GGUF to a chat GGUF");
    let opts = LoadOptions {
        n_ctx: Some(4096),
        ..Default::default()
    };
    LoadedModel::load(
        Arc::new(LlamaCppBackend::new()),
        &ModelSpec::gguf(path),
        &opts,
    )
    .await
    .expect("load")
}

fn req(prompt: &str, tools: Vec<ToolSpec>) -> ChatRequest {
    ChatRequest {
        model: "local:test".parse().unwrap(),
        messages: vec![ChatMessage::text(Role::User, prompt)],
        tools,
        max_tokens: Some(64),
        temperature: Some(0.0),
        response_schema: None,
        reasoning: Default::default(),
        caller: "test".into(),
        native_web_search: None,
    }
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF"]
async fn streams_tokens_then_done() {
    let m = model().await;
    let mut s = m.chat(req("Say hello in five words.", vec![]));
    let (mut deltas, mut done) = (0, None);
    while let Some(ev) = next_event(&mut s).await {
        match ev.unwrap() {
            StreamEvent::TextDelta { .. } | StreamEvent::ReasoningDelta { .. } => deltas += 1,
            StreamEvent::Done {
                finish_reason,
                usage,
            } => {
                assert!(usage.input_tokens > 0 && usage.output_tokens > 0);
                done = Some(finish_reason);
            }
            StreamEvent::ToolCall { .. } | StreamEvent::WebCitations { .. } => {}
        }
    }
    assert!(deltas > 1, "expected several deltas, got {deltas}");
    assert!(matches!(
        done,
        Some(FinishReason::Stop | FinishReason::Length)
    ));
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF"]
async fn dropping_the_stream_stops_generation() {
    let m = model().await;
    let mut r = req("Count from 1 to 500, one number per line.", vec![]);
    r.max_tokens = Some(2000);
    let mut s = m.chat(r);
    for _ in 0..3 {
        next_event(&mut s).await.unwrap().unwrap();
    }
    assert_eq!(active_generations(), 1);
    drop(s);
    let t = Instant::now();
    while active_generations() > 0 {
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "generation kept running"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF"]
async fn emits_a_tool_call() {
    let m = model().await;
    let tool = ToolSpec {
        name: "get_weather".into(),
        description: "Current weather for a city".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
    };
    let mut s = m.chat(req("What is the weather in Paris right now?", vec![tool]));
    let mut call = None;
    let mut finish = None;
    while let Some(ev) = next_event(&mut s).await {
        match ev.unwrap() {
            StreamEvent::ToolCall { call: c } => call = Some(c),
            StreamEvent::Done { finish_reason, .. } => finish = Some(finish_reason),
            _ => {}
        }
    }
    let call = call.expect("model should call get_weather");
    assert_eq!(call.name, "get_weather");
    assert!(call.arguments["city"].as_str().unwrap().contains("Paris"));
    assert_eq!(finish, Some(FinishReason::ToolCalls));
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF"]
async fn embeds_normalised_vectors() {
    let m = model().await;
    let v = m.embed(&["hello".into(), "world".into()]).await.unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].len(), v[1].len());
    let norm: f32 = v[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3);
}

/// ADR-0020 §2.1: the count before a call is what the model then reports it read, with and
/// without tools; the context is the one it was loaded with.
#[tokio::test]
#[ignore = "needs DN_TEST_GGUF"]
async fn counted_tokens_match_what_the_model_reads() {
    use dn_core::chat::{LimitSource, ModelLimits};
    let m = model().await;
    let model_ref = "local:test".parse().unwrap();
    assert_eq!(
        m.limits(&model_ref).await,
        ModelLimits::known(4096, LimitSource::Loaded)
    );
    for tools in [
        vec![],
        vec![ToolSpec {
            name: "get_current_time".into(),
            description: "The current time.".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }],
    ] {
        let r = req("Say hello in five words.", tools);
        let counted = m.count_tokens(&r).expect("llama.cpp counts").unwrap();
        let mut s = m.chat(r);
        let mut read = None;
        while let Some(ev) = next_event(&mut s).await {
            if let StreamEvent::Done { usage, .. } = ev.unwrap() {
                read = Some(usage.input_tokens);
            }
        }
        assert_eq!(Some(counted), read);
    }
}

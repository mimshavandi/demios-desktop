//! Tests against a real GGUF. Ignored by default so CI never downloads a model.
//!
//! Run locally with a small chat model (e.g. `dn models pull qwen2.5-0.5b-instruct-q4_k_m`):
//!
//! ```sh
//! DN_TEST_GGUF=/path/to/qwen2.5-0.5b-instruct-q4_k_m.gguf \
//!   cargo test -p dn-inference --release -- --ignored --test-threads=1
//! ```
//!
//! The speculative-decoding tests also need `DN_TEST_DRAFT_GGUF` (a smaller model with the
//! same vocabulary, e.g. Qwen3-0.6B for Qwen3-8B) and `DN_TEST_OTHER_GGUF` (a model with
//! another vocabulary).
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
            StreamEvent::ToolCall { .. } => {}
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

/// Runs `r` to the end: (text, usage).
async fn run(m: &LoadedModel<LlamaCppBackend>, r: ChatRequest) -> (String, dn_core::chat::Usage) {
    let mut s = m.chat(r);
    let (mut text, mut usage) = (String::new(), None);
    while let Some(ev) = next_event(&mut s).await {
        match ev.unwrap() {
            StreamEvent::TextDelta { text: t } | StreamEvent::ReasoningDelta { text: t } => {
                text.push_str(&t)
            }
            StreamEvent::Done { usage: u, .. } => usage = Some(u),
            StreamEvent::ToolCall { .. } => {}
        }
    }
    (text, usage.expect("done"))
}

fn chat(turns: &[(Role, String)]) -> ChatRequest {
    let mut r = req("", vec![]);
    r.messages = turns
        .iter()
        .map(|(role, t)| ChatMessage::text(*role, t.as_str()))
        .collect();
    r
}

async fn model_with(prompt_cache: bool) -> LoadedModel<LlamaCppBackend> {
    let path = std::env::var("DN_TEST_GGUF").expect("set DN_TEST_GGUF to a chat GGUF");
    let opts = LoadOptions {
        n_ctx: Some(4096),
        prompt_cache: if prompt_cache {
            dn_inference::PromptCache::on()
        } else {
            Default::default()
        },
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

/// A long first message, so the cached part is worth measuring.
fn long_question(topic: &str) -> String {
    format!(
        "{}\nIn one sentence: what is {topic}?",
        "Background notes for the question below. ".repeat(60)
    )
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF"]
async fn the_next_turn_reuses_the_cached_prefix_and_answers_the_same() {
    let cached = model_with(true).await;
    let fresh = model_with(false).await;
    let q1 = long_question("a river");
    let (a1, u1) = run(&cached, chat(&[(Role::User, q1.clone())])).await;
    assert_eq!(u1.cached_input_tokens, 0, "nothing cached yet");
    let turn2 = [
        (Role::User, q1.clone()),
        (Role::Assistant, a1.clone()),
        (Role::User, "And a lake?".to_string()),
    ];
    let (b2, u2) = run(&cached, chat(&turn2)).await;
    assert!(
        u2.cached_input_tokens as usize >= 400,
        "the first turn is reused: {u2:?}"
    );
    assert!(u2.cached_input_tokens < u2.input_tokens);
    // Without the cache, the same turn gives the same answer at temperature 0.
    let (_, f1) = run(&fresh, chat(&[(Role::User, q1)])).await;
    let (f2, fu2) = run(&fresh, chat(&turn2)).await;
    assert_eq!(
        f1.cached_input_tokens + fu2.cached_input_tokens,
        0,
        "off means off"
    );
    assert_eq!(b2, f2, "reuse doesn't change the answer");
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF"]
async fn switching_chats_restores_the_other_chats_prefix() {
    let m = model_with(true).await;
    // Two chats that differ from their first line, as two real chats do.
    let (qa, qb) = (
        format!("About rivers. {}", long_question("a river")),
        format!("About mountains. {}", long_question("a mountain")),
    );
    let (aa, _) = run(&m, chat(&[(Role::User, qa.clone())])).await;
    let (_, ub) = run(&m, chat(&[(Role::User, qb.clone())])).await;
    // Chat B shares only the template's header with chat A.
    assert!(ub.cached_input_tokens < 100, "{ub:?}");
    let back = [
        (Role::User, qa.clone()),
        (Role::Assistant, aa),
        (Role::User, "And a lake?".to_string()),
    ];
    let (_, ua) = run(&m, chat(&back)).await;
    assert!(
        ua.cached_input_tokens >= 400,
        "chat A came back from RAM: {ua:?}"
    );
    let stats = m.backend().cache_stats(m.handle()).await.unwrap();
    assert!(stats.saved >= 1, "{stats:?}");
}

fn env_path(var: &str) -> std::path::PathBuf {
    std::env::var(var)
        .unwrap_or_else(|_| panic!("set {var}"))
        .into()
}

async fn model_with_draft(
    draft: std::path::PathBuf,
) -> dn_inference::Result<LoadedModel<LlamaCppBackend>> {
    let opts = LoadOptions {
        n_ctx: Some(4096),
        draft: Some(dn_inference::DraftModel::new(draft)),
        ..Default::default()
    };
    LoadedModel::load(
        Arc::new(LlamaCppBackend::new()),
        &ModelSpec::gguf(env_path("DN_TEST_GGUF")),
        &opts,
    )
    .await
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF and DN_TEST_DRAFT_GGUF"]
async fn a_draft_model_speeds_up_without_changing_the_answer() {
    let mut r = req(
        "Write the numbers from one to forty in words, separated by commas.",
        vec![],
    );
    r.max_tokens = Some(200);
    let (plain, pu) = run(&model_with(false).await, r.clone()).await;
    let spec = model_with_draft(env_path("DN_TEST_DRAFT_GGUF"))
        .await
        .expect("load");
    let (drafted, du) = run(&spec, r).await;
    assert_eq!(drafted, plain, "the target decides every token");
    assert_eq!(du.output_tokens, pu.output_tokens);
    assert!(du.draft_tokens > 0, "{du:?}");
    assert!(
        du.draft_accepted_tokens > 0,
        "a counting task is easy to draft: {du:?}"
    );
    assert!(du.draft_accepted_tokens <= du.draft_tokens);
    assert_eq!(pu.draft_tokens, 0);
}

#[tokio::test]
#[ignore = "needs DN_TEST_GGUF and DN_TEST_OTHER_GGUF"]
async fn a_draft_with_another_vocabulary_is_refused() {
    let Err(e) = model_with_draft(env_path("DN_TEST_OTHER_GGUF")).await else {
        panic!("a draft with another vocabulary loaded");
    };
    let msg = e.to_string();
    assert!(msg.contains("draft model"), "{msg}");
}

//! Live smoke tests against real providers. Ignored by default and never run in CI.
//!
//! ```bash
//! OPENAI_API_KEY=... ANTHROPIC_API_KEY=... GEMINI_API_KEY=... \
//!   cargo test -p dn-providers --test live -- --ignored --nocapture
//! ```
//! A provider whose env var is unset is skipped. For an OpenAI-compatible server set
//! `DN_COMPAT_BASE_URL` (e.g. `http://localhost:11434/v1`) and `DN_COMPAT_MODEL`.

use dn_core::chat::{ChatMessage, ChatRequest, Reasoning, Role, StreamEvent};
use dn_providers::{ProviderConfig, ProviderKind, build, next_event};
use dn_secrets::Secret;

async fn smoke(cfg: ProviderConfig, model: &str) {
    let p = build(cfg).expect("build");
    p.validate().await.expect("validate");
    let mut s = p.chat(ChatRequest {
        model: model.parse().unwrap(),
        messages: vec![ChatMessage::text(
            Role::User,
            "Reply with the single word: pong",
        )],
        tools: vec![],
        max_tokens: Some(64),
        temperature: None,
        response_schema: None,
        reasoning: Reasoning::Auto,
        caller: "chat.title".into(),
    });
    let mut text = String::new();
    let mut done = false;
    while let Some(ev) = next_event(&mut s).await {
        match ev.expect("stream") {
            StreamEvent::TextDelta { text: t } => text.push_str(&t),
            StreamEvent::Done { usage, .. } => {
                done = true;
                println!("{model}: {text:?} {usage:?}");
            }
            _ => {}
        }
    }
    assert!(done && !text.trim().is_empty(), "{model}: no answer");
}

fn key(var: &str) -> Option<Secret> {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .map(Secret::new)
}

#[tokio::test]
#[ignore = "needs OPENAI_API_KEY"]
async fn live_openai() {
    let Some(k) = key("OPENAI_API_KEY") else {
        return;
    };
    smoke(
        ProviderConfig::new("openai", ProviderKind::OpenAi).with_key(k),
        "byok:openai:gpt-4o-mini",
    )
    .await;
}

#[tokio::test]
#[ignore = "needs ANTHROPIC_API_KEY"]
async fn live_anthropic() {
    let Some(k) = key("ANTHROPIC_API_KEY") else {
        return;
    };
    smoke(
        ProviderConfig::new("anthropic", ProviderKind::Anthropic).with_key(k),
        "byok:anthropic:claude-haiku-4-5",
    )
    .await;
}

#[tokio::test]
#[ignore = "needs GEMINI_API_KEY"]
async fn live_gemini() {
    let Some(k) = key("GEMINI_API_KEY") else {
        return;
    };
    smoke(
        ProviderConfig::new("gemini", ProviderKind::Gemini).with_key(k),
        "byok:gemini:gemini-2.5-flash",
    )
    .await;
}

#[tokio::test]
#[ignore = "needs DN_COMPAT_BASE_URL and DN_COMPAT_MODEL"]
async fn live_openai_compatible() {
    let (Ok(base), Ok(model)) = (
        std::env::var("DN_COMPAT_BASE_URL"),
        std::env::var("DN_COMPAT_MODEL"),
    ) else {
        return;
    };
    let mut cfg = ProviderConfig::new("compat", ProviderKind::OpenAiCompatible).with_base_url(base);
    cfg.api_key = key("DN_COMPAT_API_KEY");
    smoke(cfg, &format!("remote:compat:{model}")).await;
}

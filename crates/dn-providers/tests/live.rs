//! Live smoke tests against real providers. Ignored by default and never run in CI.
//!
//! ```bash
//! OPENAI_API_KEY=... ANTHROPIC_API_KEY=... GEMINI_API_KEY=... \
//!   cargo test -p dn-providers --test live -- --ignored --nocapture
//! ```
//! A provider whose env var is unset is skipped. For an OpenAI-compatible server set
//! `DN_COMPAT_BASE_URL` (e.g. `http://localhost:11434/v1`) and `DN_COMPAT_MODEL`.
//!
//! **Recording fixtures:** with `DN_RECORD_FIXTURES=1` the adapter talks to a local proxy
//! that forwards to the real API and saves the chat response to
//! `tests/fixtures/<provider>/text.recorded.sse`, with the key and every id/signature
//! replaced by `REDACTED`. Review the file by hand before committing it.

mod common;

use dn_core::chat::{ChatMessage, ChatRequest, Reasoning, Role, StreamEvent};
use dn_providers::{ProviderConfig, ProviderKind, build, next_event};
use dn_secrets::Secret;

async fn smoke(mut cfg: ProviderConfig, model: &str) {
    if std::env::var("DN_RECORD_FIXTURES").as_deref() == Ok("1") {
        let upstream = cfg
            .base_url
            .clone()
            .or(cfg.kind.default_base_url().map(String::from))
            .expect("base url");
        let key = cfg.api_key.as_ref().map(|k| k.expose().to_string());
        let dir = format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            cfg.provider_id
        );
        cfg.base_url = Some(recording_proxy(upstream, key, dir).await);
    }
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

/// Local HTTP proxy → real API. Saves each streaming (`text/event-stream`) response,
/// scrubbed, and replays it to the client.
async fn recording_proxy(upstream: String, key: Option<String>, dir: String) -> String {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let req = common::read_request(&mut sock).await;
            let mut rb = client.request(
                req.method.parse().unwrap(),
                format!("{upstream}{}", req.path),
            );
            for (k, v) in &req.headers {
                if !["host", "content-length", "connection"].contains(&k.as_str()) {
                    rb = rb.header(k, v);
                }
            }
            if !req.body.is_null() {
                rb = rb.body(serde_json::to_vec(&req.body).unwrap());
            }
            let resp = rb.send().await.expect("upstream");
            let status = resp.status().as_u16();
            let ctype = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();
            let body = resp.bytes().await.expect("upstream body");
            if ctype.starts_with("text/event-stream") {
                std::fs::create_dir_all(&dir).unwrap();
                let clean = scrub(&String::from_utf8_lossy(&body), key.as_deref());
                std::fs::write(format!("{dir}/text.recorded.sse"), clean).unwrap();
            }
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// Removes the key and replaces id-like fields in every SSE `data:` JSON payload.
fn scrub(sse: &str, key: Option<&str>) -> String {
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(o) => {
                for (k, child) in o.iter_mut() {
                    let secretish = [
                        "id",
                        "responseId",
                        "signature",
                        "thoughtSignature",
                        "system_fingerprint",
                    ]
                    .contains(&k.as_str());
                    if secretish && child.is_string() {
                        *child = serde_json::Value::String("REDACTED".into());
                    } else {
                        walk(child);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut out = String::new();
    for line in sse.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        match body
            .strip_prefix("data: ")
            .map(serde_json::from_str::<serde_json::Value>)
        {
            Some(Ok(mut v)) => {
                walk(&mut v);
                out.push_str(&format!("data: {v}{nl}"));
            }
            _ => out.push_str(line),
        }
    }
    match key.filter(|k| k.len() >= 4) {
        Some(k) => out.replace(k, "REDACTED"),
        None => out,
    }
}

#[test]
fn scrub_redacts_ids_signatures_and_key() {
    let raw = "event: x\ndata: {\"id\":\"msg_123\",\"delta\":{\"signature\":\"abc\",\"text\":\"k-SECRET\"}}\n\ndata: [DONE]\n";
    let s = scrub(raw, Some("k-SECRET"));
    assert_eq!(
        s,
        "event: x\ndata: {\"delta\":{\"signature\":\"REDACTED\",\"text\":\"REDACTED\"},\"id\":\"REDACTED\"}\n\ndata: [DONE]\n"
    );
}

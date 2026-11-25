//! MoE expert placement against a real (tiny) MoE GGUF (T-INF-20). Ignored by default; CI
//! runs it with `ggml-org/stories15M_MOE` (Q8_0, 39 MB, MIT), pinned by commit and sha256
//! in `.github/workflows/ci.yml`:
//!
//! ```sh
//! DN_TEST_MOE_GGUF=/path/to/stories15M_MOE-Q8_0.gguf \
//!   cargo test -p dn-inference --test moe_offload -- --ignored --test-threads=1
//! ```
#![cfg(feature = "llamacpp")]

use dn_core::chat::{ChatMessage, ChatModel, ChatRequest, Role, StreamEvent};
use dn_inference::{LlamaCppBackend, LoadOptions, LoadedModel, ModelSpec, MoeOffload, next_event};
use std::sync::Arc;

/// Greedy text for a fixed prompt with experts placed per `moe`.
async fn generate(moe: MoeOffload) -> String {
    let path = std::env::var("DN_TEST_MOE_GGUF").expect("set DN_TEST_MOE_GGUF to a MoE GGUF");
    let opts = LoadOptions {
        n_ctx: Some(256),
        moe_offload: moe,
        ..Default::default()
    };
    let m = LoadedModel::load(
        Arc::new(LlamaCppBackend::new()),
        &ModelSpec::gguf(path),
        &opts,
    )
    .await
    .unwrap_or_else(|e| panic!("load with {moe:?}: {e}"));
    let mut s = m.chat(ChatRequest {
        model: "local:test".parse().unwrap(),
        messages: vec![ChatMessage::text(Role::User, "Once upon a time")],
        tools: vec![],
        max_tokens: Some(24),
        temperature: Some(0.0),
        response_schema: None,
        reasoning: Default::default(),
        caller: "test".into(),
    });
    let mut text = String::new();
    while let Some(ev) = next_event(&mut s).await {
        match ev.unwrap_or_else(|e| panic!("generate with {moe:?}: {e}")) {
            StreamEvent::TextDelta { text: t } | StreamEvent::ReasoningDelta { text: t } => {
                text.push_str(&t)
            }
            _ => {}
        }
    }
    text
}

#[tokio::test]
#[ignore = "needs DN_TEST_MOE_GGUF"]
async fn experts_on_cpu_load_and_generate() {
    let base = generate(MoeOffload::None).await;
    assert!(!base.trim().is_empty(), "no text without offload");
    for moe in [MoeOffload::All, MoeOffload::FirstLayers(1)] {
        let out = generate(moe).await;
        assert!(!out.trim().is_empty(), "no text with {moe:?}");
        // Without a GPU every tensor is on the CPU either way, so greedy output must match.
        // With one, CPU and GPU kernels may round differently; loading and generating is the
        // check there.
        if !LlamaCppBackend::supports_gpu() {
            assert_eq!(
                out, base,
                "{moe:?} changed greedy output on a CPU-only build"
            );
        }
    }
}

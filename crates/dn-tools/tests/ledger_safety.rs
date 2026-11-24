//! Review fixes for PR #7: idempotency-key scoping, drop guard, unknown outcomes,
//! stdio environment allow-list. Runs against the stdio fixture server.

mod common;

use common::{call, ctx_in, executor_with_ledger, fixture_server_path};
use dn_core::{CoreError, Id};
use dn_tools::ledger::{self, Resolution, RunStatus};
use dn_tools::{
    ApprovalDecision, ApprovalRequest, CapabilityOverride, McpClient, McpServerConfig,
    MemorySecretStore, ValueRef,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

fn stdio_cfg() -> McpServerConfig {
    McpServerConfig::stdio("fixture", fixture_server_path().to_string_lossy(), vec![])
}

fn client(cfg: McpServerConfig) -> McpClient {
    McpClient::new(cfg, Arc::new(MemorySecretStore::new())).unwrap()
}

/// Approve every request; returns how many prompts were shown.
fn auto_approve(mut rx: mpsc::Receiver<ApprovalRequest>) -> Arc<AtomicU32> {
    let n = Arc::new(AtomicU32::new(0));
    let n2 = n.clone();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            n2.fetch_add(1, Ordering::SeqCst);
            req.respond(ApprovalDecision::Approve);
        }
    });
    n
}

fn text(r: &Result<dn_tools::ToolOutcome, CoreError>) -> String {
    r.as_ref().unwrap().output.text()
}

#[tokio::test]
async fn call_id_keys_are_scoped_to_the_run() {
    let (ex, rx, _db) = executor_with_ledger();
    let _prompts = auto_approve(rx);
    ex.add_server(client(stdio_cfg())).await.unwrap();
    let run1 = Id::new();
    let run2 = Id::new();
    // Local models reuse `call_0` in every run: a new run must execute, not replay.
    let w = call(
        "call_0",
        "mcp.fixture.write_note",
        serde_json::json!({"note": "n"}),
    );
    let a = ex.execute(&ctx_in("c", run1), &w).await;
    assert!(text(&a).contains("write #1"));
    let b = ex.execute(&ctx_in("c", run2), &w).await;
    assert!(text(&b).contains("write #2"), "{}", text(&b));
    // Same run + same call id → replay.
    let again = ex.execute(&ctx_in("c", run1), &w).await.unwrap();
    assert!(again.replayed);
    assert!(again.output.text().contains("write #1"));
}

#[tokio::test]
async fn field_keys_are_scoped_to_the_conversation() {
    let (ex, rx, _db) = executor_with_ledger();
    let _prompts = auto_approve(rx);
    ex.set_overrides(HashMap::from([(
        "mcp.fixture.write_note".to_owned(),
        CapabilityOverride {
            idempotency_key_field: Some("request_id".into()),
            ..Default::default()
        },
    )]));
    ex.add_server(client(stdio_cfg())).await.unwrap();
    let run = Id::new();
    let args = serde_json::json!({"note": "n", "request_id": "tok-1"});
    let a = ex
        .execute(
            &ctx_in("A", run),
            &call("1", "mcp.fixture.write_note", args.clone()),
        )
        .await;
    assert!(text(&a).contains("write #1"));
    // Conversation B with the same token must not see A's output.
    let b = ex
        .execute(
            &ctx_in("B", run),
            &call("2", "mcp.fixture.write_note", args.clone()),
        )
        .await
        .unwrap();
    assert!(!b.replayed);
    assert!(b.output.text().contains("write #2"));
    // Same token in A (any call id) → replay of A's result.
    let a2 = ex
        .execute(
            &ctx_in("A", run),
            &call("3", "mcp.fixture.write_note", args),
        )
        .await
        .unwrap();
    assert!(a2.replayed);
    assert!(a2.output.text().contains("write #1"));
    // Token reused with different arguments → model-visible input error, not a replay.
    let bad = ex
        .execute(
            &ctx_in("A", run),
            &call(
                "4",
                "mcp.fixture.write_note",
                serde_json::json!({"note": "other", "request_id": "tok-1"}),
            ),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(bad, CoreError::Capability { developer_message, .. } if developer_message.contains("different arguments"))
    );
    // A fresh token in A runs again.
    let fresh = ex
        .execute(
            &ctx_in("A", run),
            &call(
                "5",
                "mcp.fixture.write_note",
                serde_json::json!({"note": "n", "request_id": "tok-2"}),
            ),
        )
        .await;
    assert!(text(&fresh).contains("write #3"));
}

#[tokio::test]
async fn dropped_execute_marks_unknown_and_blocks_until_resolved() {
    let (ex, rx, db) = executor_with_ledger();
    let prompts = auto_approve(rx);
    ex.add_server(client(stdio_cfg())).await.unwrap();
    let ctx = ctx_in("c", Id::new());
    let w = call("w1", "mcp.fixture.slow_write", serde_json::json!({}));
    let ex2 = ex.clone();
    let (ctx2, w2) = (ctx.clone(), w.clone());
    let task = tokio::spawn(async move { ex2.execute(&ctx2, &w2).await });
    // Let it get approved and claimed, then drop the future mid-call.
    tokio::time::sleep(Duration::from_millis(500)).await;
    task.abort();
    let _ = task.await;
    let key = dn_tools::Executor::idempotency_key(
        &ex.resolve("mcp.fixture.slow_write").unwrap(),
        &ctx,
        &w,
    );
    let entry = ledger::get(&db.lock().unwrap(), &key).unwrap().unwrap();
    assert_eq!(entry.status, RunStatus::Unknown, "{entry:?}");
    assert_eq!(ex.unknown_outcomes().unwrap().len(), 1);

    // Re-running the same call is refused without asking.
    let before = prompts.load(Ordering::SeqCst);
    let e = ex.execute(&ctx, &w).await.unwrap_err();
    assert!(
        matches!(e, CoreError::Capability { ref developer_message, .. } if developer_message.contains("unknown outcome")),
        "{e:?}"
    );
    assert_eq!(
        prompts.load(Ordering::SeqCst),
        before,
        "no prompt for a blocked re-run"
    );

    // User says it didn't happen → it may run again (and asks again).
    ex.resolve_unknown(&key, &Resolution::DidNotHappen).unwrap();
    let ex3 = ex.clone();
    let r = tokio::time::timeout(Duration::from_secs(15), async move {
        ex3.execute(&ctx, &w).await
    })
    .await
    .unwrap();
    assert_eq!(text(&r), "slow write done");
    assert_eq!(prompts.load(Ordering::SeqCst), before + 1);
}

#[tokio::test]
async fn cancel_or_timeout_mid_write_is_unknown_not_failed() {
    let (ex, rx, db) = executor_with_ledger();
    let _prompts = auto_approve(rx);
    let mut cfg = stdio_cfg();
    cfg.request_timeout_ms = 300;
    ex.add_server(client(cfg)).await.unwrap();
    let ctx = ctx_in("c", Id::new());
    let cap = ex.resolve("mcp.fixture.slow_write").unwrap();

    // Timeout after the request was sent.
    let w = call("t1", "mcp.fixture.slow_write", serde_json::json!({}));
    let e = ex.execute(&ctx, &w).await.unwrap_err();
    assert!(matches!(e, CoreError::Capability { .. }));
    let key = dn_tools::Executor::idempotency_key(&cap, &ctx, &w);
    let entry = ledger::get(&db.lock().unwrap(), &key).unwrap().unwrap();
    assert_eq!(entry.status, RunStatus::Unknown);

    // Cancel during the call (another conversation: the unknown row above now blocks new
    // calls to this capability in the first one).
    let ctx = ctx_in("other", Id::new());
    let w = call("c1", "mcp.fixture.slow_write", serde_json::json!({}));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let ex2 = ex.clone();
    let (ctx2, w2) = (ctx.clone(), w.clone());
    let task = tokio::spawn(async move {
        ex2.execute_with_cancel(&ctx2, &w2, async {
            let _ = stop_rx.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop_tx.send(()).unwrap();
    assert_eq!(task.await.unwrap().unwrap_err(), CoreError::Cancelled);
    let key = dn_tools::Executor::idempotency_key(&cap, &ctx, &w);
    let entry = ledger::get(&db.lock().unwrap(), &key).unwrap().unwrap();
    assert_eq!(entry.status, RunStatus::Unknown);
}

/// Re-runs this test binary with a planted secret in the *parent* environment (setting env
/// vars in-process needs `unsafe`, which the workspace forbids), then checks the child.
#[tokio::test]
async fn stdio_child_gets_only_allow_listed_env() {
    const PLANTED: &str = "DN_TEST_SECRET";
    if std::env::var_os(PLANTED).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "stdio_child_gets_only_allow_listed_env",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(PLANTED, "planted-value-should-not-leak")
            .status()
            .unwrap();
        assert!(status.success(), "child test run failed");
        return;
    }

    let mut cfg = stdio_cfg();
    if let dn_tools::McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("FIXTURE_FLAG".into(), ValueRef::plain("on"));
    }
    let c = client(cfg);
    let names = c
        .call_tool("env_names", serde_json::json!({}))
        .await
        .unwrap()
        .text();
    let names: Vec<String> = names.split(',').map(str::to_owned).collect();
    let has = |n: &str| names.iter().any(|x| x.eq_ignore_ascii_case(n));
    assert!(has("FIXTURE_FLAG"), "{names:?}");
    // Windows spells it `Path`; env names are case-insensitive there.
    assert!(has("PATH"), "{names:?}");
    assert!(!has(PLANTED), "planted parent secret leaked: {names:?}");
    assert!(!has("CARGO_MANIFEST_DIR"), "{names:?}");
    let allowed: Vec<String> = dn_tools::client::inherited_env()
        .into_iter()
        .filter_map(|(k, _)| k.into_string().ok())
        .collect();
    let unexpected: Vec<&String> = names
        .iter()
        .filter(|n| {
            !n.eq_ignore_ascii_case("FIXTURE_FLAG")
                && !allowed.iter().any(|a| a.eq_ignore_ascii_case(n))
                // set by the OS/loader on some platforms even with an empty environment
                && !n.starts_with("__CF")
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected vars leaked: {unexpected:?}"
    );
}

/// Shutdown stops grandchildren too (process group), as with `npx` → `node`.
#[cfg(unix)]
#[tokio::test]
async fn shutdown_kills_grandchildren() {
    let c = client(stdio_cfg());
    let pid = c
        .call_tool("spawn_grandchild", serde_json::json!({}))
        .await
        .unwrap()
        .text();
    let alive = |pid: &str| {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    assert!(alive(&pid), "grandchild {pid} should be running");
    c.shutdown().await;
    let mut gone = false;
    for _ in 0..40 {
        if !alive(&pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "grandchild {pid} survived shutdown");
}

/// Items A + B of the second review: an "allow for this chat" grant never lets a write with an
/// unknown outcome run again — neither the same call nor a retry with a fresh call id — until
/// the user resolves it. The model only sees the user-facing message.
#[tokio::test]
async fn unknown_outcome_blocks_ask_once_grant_and_fresh_call_ids() {
    use dn_agent::{CancelToken, ToolContext, ToolExecutor};
    let (ex, mut rx, _db) = executor_with_ledger();
    ex.set_overrides(HashMap::from([(
        "mcp.fixture.slow_write".to_owned(),
        CapabilityOverride {
            approval: Some(dn_core::capability::ApprovalPolicy::AskOncePerConversation),
            ..Default::default()
        },
    )]));
    let mut cfg = stdio_cfg();
    cfg.request_timeout_ms = 300;
    ex.add_server(client(cfg)).await.unwrap();
    let conv = Id::new();
    let run = Id::new();
    let ctx = ctx_in(&conv.to_string(), run);

    // First call: user grants "allow for this chat"; the call times out after sending → unknown.
    let ex2 = ex.clone();
    let c2 = ctx.clone();
    let first = tokio::spawn(async move {
        ex2.execute(
            &c2,
            &call("c1", "mcp.fixture.slow_write", serde_json::json!({})),
        )
        .await
    });
    rx.recv()
        .await
        .unwrap()
        .respond(ApprovalDecision::ApproveForConversation);
    assert!(first.await.unwrap().is_err());
    assert_eq!(ex.unknown_outcomes().unwrap().len(), 1);
    assert!(ex.gate().is_satisfied(
        &ctx.scope(),
        "mcp.fixture.slow_write",
        dn_core::capability::ApprovalPolicy::AskOncePerConversation
    ));

    // (A) Same call again: refused despite the grant, no prompt.
    let e = ex
        .execute(
            &ctx,
            &call("c1", "mcp.fixture.slow_write", serde_json::json!({})),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CoreError::Capability { developer_message, .. } if developer_message.contains("unknown outcome")),
        "{e:?}"
    );

    // (B) The model retries with a new call id through the agent contract: still refused,
    // model-visible, telling it to ask the user; no developer detail leaks.
    let tctx = ToolContext {
        agent_id: "assistant.default".into(),
        caller: "chat.reply".into(),
        conversation_id: Some(conv),
        run_id: run,
        step: 1,
        cancel: CancelToken::new(),
    };
    let out = ToolExecutor::execute(
        ex.as_ref(),
        &call("c2", "mcp__fixture__slow_write", serde_json::json!({})),
        &tctx,
    )
    .await
    .unwrap();
    assert!(out.is_error);
    let dn_core::chat::ContentPart::Text { text } = &out.content[0] else {
        panic!("text expected")
    };
    assert!(text.contains("confirm"), "{text}");
    assert!(!text.contains("key `"), "developer detail leaked: {text}");
    assert!(rx.try_recv().is_err(), "no prompt while blocked");
    assert!(
        ex.recent_diagnostics()
            .iter()
            .any(|d| d.developer_message.contains("unresolved unknown outcome"))
    );

    // Resolve → the fresh call goes to the server again (grant still valid, so no prompt);
    // here it times out again, proving it was sent rather than refused.
    let key = ex.unknown_outcomes().unwrap()[0].idempotency_key.clone();
    ex.resolve_unknown(&key, &Resolution::DidNotHappen).unwrap();
    let e = ex
        .execute(
            &ctx,
            &call("c2", "mcp.fixture.slow_write", serde_json::json!({})),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CoreError::Capability { developer_message, .. } if developer_message.contains("did not answer")),
        "{e:?}"
    );
    assert!(rx.try_recv().is_err());
}

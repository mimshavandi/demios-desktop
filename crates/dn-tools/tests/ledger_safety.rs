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

    // Cancel during the call.
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

#[tokio::test]
async fn stdio_child_gets_only_allow_listed_env() {
    // Cargo sets CARGO_MANIFEST_DIR for the test process; it must not reach the server.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
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
    let names: Vec<&str> = names.split(',').collect();
    assert!(names.contains(&"FIXTURE_FLAG"), "{names:?}");
    assert!(names.contains(&"PATH"), "{names:?}");
    assert!(!names.contains(&"CARGO_MANIFEST_DIR"), "{names:?}");
    assert!(
        names.iter().all(|n| dn_tools::client::inherited_env()
            .iter()
            .any(|(k, _)| k.to_str() == Some(n))
            || *n == "FIXTURE_FLAG"
            // set by the OS/loader on some platforms even with an empty environment
            || n.starts_with("__CF")),
        "unexpected vars leaked: {names:?}"
    );
}

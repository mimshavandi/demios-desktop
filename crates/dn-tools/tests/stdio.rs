//! Stdio MCP: connect, list, read auto-runs, write waits for approval, ledger replay,
//! restart after crash, timeouts, error envelope.

mod common;

use common::{call, executor, fixture_server_path};
use dn_core::CoreError;
use dn_core::capability::{ApprovalPolicy, Preflight, RiskTier};
use dn_tools::{
    ApprovalDecision, CallContext, McpClient, McpServerConfig, MemorySecretStore, ValueRef,
};
use std::sync::Arc;
use std::time::Duration;

static RUN: std::sync::LazyLock<dn_core::Id> = std::sync::LazyLock::new(dn_core::Id::new);

fn ctx() -> CallContext {
    CallContext::new(Some("conv-1".into()), *RUN)
}

fn client(cfg: McpServerConfig) -> McpClient {
    McpClient::new(cfg, Arc::new(MemorySecretStore::new())).unwrap()
}

fn stdio_cfg() -> McpServerConfig {
    McpServerConfig::stdio("fixture", fixture_server_path().to_string_lossy(), vec![])
}

#[tokio::test]
async fn lists_tools_and_seeds_risk() {
    let (ex, _rx) = executor();
    let caps = ex.add_server(client(stdio_cfg())).await.unwrap();
    let find = |n: &str| caps.iter().find(|c| c.tool_name == n).unwrap();
    assert_eq!(find("echo").spec.id, "mcp.fixture.echo");
    assert_eq!(find("echo").spec.risk, RiskTier::Read);
    assert_eq!(find("echo").approval, ApprovalPolicy::Auto);
    assert_eq!(find("write_note").spec.risk, RiskTier::LowWrite);
    assert_eq!(find("delete_all").spec.risk, RiskTier::Destructive);
    assert!(
        ex.tool_specs()
            .iter()
            .any(|t| t.name == "mcp__fixture__echo")
    );
}

#[tokio::test]
async fn read_auto_runs_and_write_waits_for_approval() {
    let (ex, mut rx) = executor();
    ex.add_server(client(stdio_cfg())).await.unwrap();

    // Read: no approval request is produced.
    let out = ex
        .execute(
            &ctx(),
            &call(
                "c1",
                "mcp__fixture__echo",
                serde_json::json!({"text": "hi"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(out.output.text(), "hi");
    assert!(rx.try_recv().is_err());

    // Write: preflight says approval required, and execution blocks until the UI answers.
    let write = call(
        "c2",
        "mcp.fixture.write_note",
        serde_json::json!({"note": "n1"}),
    );
    assert_eq!(
        ex.preflight(&ctx(), &write).unwrap(),
        Preflight::ApprovalRequired
    );
    let ex2 = ex.clone();
    let w = write.clone();
    let task = tokio::spawn(async move { ex2.execute(&ctx(), &w).await });
    let req = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.prompt.capability_id, "mcp.fixture.write_note");
    assert_eq!(req.prompt.arguments, serde_json::json!({"note": "n1"}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!task.is_finished(), "write must wait for approval");
    req.respond(ApprovalDecision::Approve);
    let out = task.await.unwrap().unwrap();
    assert!(out.output.text().contains("write #1"));
    assert!(!out.replayed);

    // Ledger replay: same call id → cached output, no new side effect, no approval prompt.
    let again = ex.execute(&ctx(), &write).await.unwrap();
    assert!(again.replayed);
    assert_eq!(again.output, out.output);
    assert!(rx.try_recv().is_err());

    // Deny → CoreError::Capability with user and developer messages.
    let ex3 = ex.clone();
    let task = tokio::spawn(async move {
        ex3.execute(
            &ctx(),
            &call("c3", "mcp.fixture.delete_all", serde_json::json!({})),
        )
        .await
    });
    rx.recv().await.unwrap().respond(ApprovalDecision::Deny);
    match task.await.unwrap() {
        Err(CoreError::Capability {
            capability_id,
            user_message,
            developer_message,
            retryable,
            ..
        }) => {
            assert_eq!(capability_id, "mcp.fixture.delete_all");
            assert!(user_message.contains("declined"), "{user_message}");
            assert!(developer_message.contains("denied"), "{developer_message}");
            assert!(!retryable);
        }
        other => panic!("expected capability error, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_error_and_missing_input_are_capability_errors() {
    let (ex, _rx) = executor();
    ex.add_server(client(stdio_cfg())).await.unwrap();
    let e = ex
        .execute(
            &ctx(),
            &call("c1", "mcp.fixture.fail", serde_json::json!({})),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CoreError::Capability { developer_message, .. } if developer_message.contains("it broke"))
    );
    let e = ex
        .execute(
            &ctx(),
            &call("c2", "mcp.fixture.echo", serde_json::json!({})),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CoreError::Capability { developer_message, .. } if developer_message.contains("text"))
    );
    let e = ex
        .execute(&ctx(), &call("c3", "mcp.nope.echo", serde_json::json!({})))
        .await
        .unwrap_err();
    assert!(matches!(e, CoreError::Capability { .. }));
}

#[tokio::test]
async fn reconnects_after_server_crash() {
    let c = Arc::new(client(stdio_cfg()));
    let pid1 = c
        .call_tool("pid", serde_json::json!({}))
        .await
        .unwrap()
        .text();
    let crash = c.call_tool("crash", serde_json::json!({})).await;
    assert!(crash.is_err(), "crash should surface as an error");
    // Give the transport a moment to notice EOF, then the next call respawns the server.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pid2 = c
        .call_tool("pid", serde_json::json!({}))
        .await
        .unwrap()
        .text();
    assert_ne!(pid1, pid2);
    assert_eq!(c.connect_count(), 2);
    c.restart().await.unwrap();
    assert_eq!(c.connect_count(), 3);
    assert!(
        c.stderr_tail()
            .iter()
            .any(|l| l.contains("fixture server starting"))
    );
}

#[tokio::test]
async fn request_timeout_is_reported() {
    let mut cfg = stdio_cfg();
    cfg.request_timeout_ms = 300;
    let c = client(cfg);
    let e = c
        .call_tool("slow", serde_json::json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.kind, dn_tools::error::ToolErrorKind::Timeout);
    let core: CoreError = e.into();
    assert!(matches!(
        core,
        CoreError::Capability {
            retryable: true,
            ..
        }
    ));
}

#[tokio::test]
async fn spawn_failure_and_missing_secret() {
    let c = client(McpServerConfig::stdio(
        "nope",
        "/definitely/not/here",
        vec![],
    ));
    let e = c.connect().await.unwrap_err();
    assert_eq!(e.kind, dn_tools::error::ToolErrorKind::Connection);

    let mut cfg = stdio_cfg();
    if let dn_tools::McpTransport::Stdio { env, .. } = &mut cfg.transport {
        env.insert("API_TOKEN".into(), ValueRef::secret("mcp.fixture.token"));
    }
    let e = client(cfg).connect().await.unwrap_err();
    assert_eq!(e.kind, dn_tools::error::ToolErrorKind::Config);
    assert!(e.developer_message.contains("mcp.fixture.token"));
}

#[tokio::test]
async fn cancel_during_approval_wait_returns_cancelled() {
    let (ex, mut rx) = executor();
    ex.add_server(client(stdio_cfg())).await.unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let ex2 = ex.clone();
    let task = tokio::spawn(async move {
        ex2.execute_with_cancel(
            &ctx(),
            &call("k1", "mcp.fixture.delete_all", serde_json::json!({})),
            async {
                let _ = stop_rx.await;
            },
        )
        .await
    });
    let _pending = rx.recv().await.unwrap(); // approval is waiting, never answered
    stop_tx.send(()).unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancel must return promptly")
        .unwrap();
    assert_eq!(r.unwrap_err(), CoreError::Cancelled);
}

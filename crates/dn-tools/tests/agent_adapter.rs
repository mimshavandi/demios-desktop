//! `dn_agent::ToolExecutor` contract over the stdio fixture server.

mod common;

use common::{call, executor, fixture_server_path};
use dn_agent::{CancelToken, ToolContext, ToolExecutor};
use dn_core::chat::ContentPart;
use dn_core::{CoreError, Id};
use dn_tools::{ApprovalDecision, McpClient, McpServerConfig, MemorySecretStore};
use std::sync::Arc;
use std::time::Duration;

fn tool_ctx(cancel: CancelToken) -> ToolContext {
    ToolContext {
        agent_id: "assistant.default".into(),
        caller: "chat.reply".into(),
        conversation_id: Some(Id::new()),
        run_id: Id::new(),
        step: 0,
        cancel,
    }
}

#[tokio::test]
async fn trait_contract() {
    let (ex, mut rx) = executor();
    ex.add_server(
        McpClient::new(
            McpServerConfig::stdio("fixture", fixture_server_path().to_string_lossy(), vec![]),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let exec: Arc<dyn ToolExecutor> = ex.clone();
    assert!(exec.tools().iter().any(|t| t.name == "mcp__fixture__echo"));
    let ctx = tool_ctx(CancelToken::new());

    // Read runs and returns text content.
    let out = exec
        .execute(
            &call("1", "mcp__fixture__echo", serde_json::json!({"text": "hi"})),
            &ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(out.content, vec![ContentPart::Text { text: "hi".into() }]);

    // Unknown tool and tool errors are model-visible, not run-ending.
    let out = exec
        .execute(&call("2", "nope", serde_json::json!({})), &ctx)
        .await
        .unwrap();
    assert!(out.is_error);
    let out = exec
        .execute(
            &call("3", "mcp__fixture__fail", serde_json::json!({})),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.is_error);

    // Denied approval → Ok(is_error).
    let e2 = exec.clone();
    let c2 = ctx.clone();
    let task = tokio::spawn(async move {
        e2.execute(
            &call("4", "mcp__fixture__delete_all", serde_json::json!({})),
            &c2,
        )
        .await
    });
    rx.recv().await.unwrap().respond(ApprovalDecision::Deny);
    let out = task.await.unwrap().unwrap();
    assert!(out.is_error);

    // Cancel while waiting for approval → Err(Cancelled), promptly.
    let cancel = CancelToken::new();
    let c3 = tool_ctx(cancel.clone());
    let e3 = exec.clone();
    let task = tokio::spawn(async move {
        e3.execute(
            &call("5", "mcp__fixture__delete_all", serde_json::json!({})),
            &c3,
        )
        .await
    });
    let _pending = rx.recv().await.unwrap();
    cancel.cancel();
    let r = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.unwrap_err(), CoreError::Cancelled);
}

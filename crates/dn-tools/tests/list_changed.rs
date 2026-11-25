//! #178: a server that changes its tools mid-session (`notifications/tools/list_changed`).
//! Its tools are stale at once, left out and refused, until they are listed again and
//! the app has reviewed them ([`Executor::settled`]).

mod common;

use common::{call, ctx_in, executor, fixture_server_path};
use dn_core::Id;
use dn_tools::{McpClient, McpServerConfig, MemorySecretStore};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn a_tool_list_change_marks_the_server_stale_until_it_is_settled() {
    let (ex, _approvals) = executor();
    let mut changes = ex.tool_changes();
    ex.add_server(
        McpClient::new(
            McpServerConfig::stdio("fixture", fixture_server_path().to_string_lossy(), vec![]),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let ctx = ctx_in("conv", Id::new());
    let echo = |id: &str| call(id, "mcp__fixture__echo", serde_json::json!({"text": "hi"}));

    // Before: echo runs, and nothing is stale.
    assert!(ex.execute(&ctx, &echo("1")).await.is_ok());
    assert!(!ex.is_stale("fixture"));

    // The server changes `echo` and adds `added`, then notifies.
    ex.execute(
        &ctx,
        &call("2", "mcp__fixture__change_tools", serde_json::json!({})),
    )
    .await
    .unwrap();
    let id = tokio::time::timeout(Duration::from_secs(10), changes.recv())
        .await
        .expect("the change notice arrived")
        .unwrap();
    assert_eq!(id, "fixture");

    // Stale at once: left out of the model's list, refused with a clear, retryable error.
    assert!(ex.is_stale("fixture"));
    assert!(
        !ex.tool_specs()
            .iter()
            .any(|t| t.name.starts_with("mcp__fixture__")),
        "no tool of a stale server is offered"
    );
    let e = ex.execute(&ctx, &echo("3")).await.unwrap_err();
    match e {
        dn_core::CoreError::Capability {
            user_message,
            retryable,
            ..
        } => {
            assert!(user_message.contains("changed its tools"), "{user_message}");
            assert!(retryable);
        }
        other => panic!("{other:?}"),
    }
    assert!(ex.preflight(&ctx, &echo("4")).is_err());

    // Listing again sees the change but doesn't settle by itself: the app reviews first.
    let caps = ex.refresh("fixture").await.unwrap();
    let echo_cap = caps.iter().find(|c| c.tool_name == "echo").unwrap();
    assert!(echo_cap.spec.description.contains("webhook"));
    assert!(caps.iter().any(|c| c.tool_name == "added"));
    assert!(ex.is_stale("fixture"));
    assert!(ex.execute(&ctx, &echo("5")).await.is_err());

    // Settled: offered and runnable again.
    ex.settled("fixture");
    assert!(
        ex.tool_specs()
            .iter()
            .any(|t| t.name == "mcp__fixture__added")
    );
    assert!(ex.execute(&ctx, &echo("6")).await.is_ok());
}

#[tokio::test]
async fn removing_a_stale_server_clears_it() {
    let (ex, _approvals) = executor();
    let mut changes = ex.tool_changes();
    ex.add_server(
        McpClient::new(
            McpServerConfig::stdio("fixture", fixture_server_path().to_string_lossy(), vec![]),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    ex.execute(
        &ctx_in("c", Id::new()),
        &call("1", "mcp__fixture__change_tools", serde_json::json!({})),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), changes.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(ex.is_stale("fixture"));
    ex.remove_server("fixture").await;
    assert!(!ex.is_stale("fixture"));
}

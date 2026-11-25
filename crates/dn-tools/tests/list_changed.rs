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
    let generation = ex.tool_list_generation("fixture");
    assert!(generation > 0);
    let caps = ex.refresh("fixture").await.unwrap();
    let echo_cap = caps.iter().find(|c| c.tool_name == "echo").unwrap();
    assert!(echo_cap.spec.description.contains("webhook"));
    assert!(caps.iter().any(|c| c.tool_name == "added"));
    assert!(ex.is_stale("fixture"));
    assert!(ex.execute(&ctx, &echo("5")).await.is_err());

    // Settled: offered and runnable again.
    ex.settled("fixture", generation);
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
    let before = ex.tool_list_generation("fixture");
    ex.remove_server("fixture").await;
    assert!(!ex.is_stale("fixture"));
    // A new generation, never back to an old one: a card opened before a reconnect can't
    // pass its check afterwards.
    assert!(ex.tool_list_generation("fixture") > before);
}

/// The rug pull: a card opened before the change is approved after the re-review already
/// finished. The call must not run on the new definition.
#[tokio::test]
async fn an_approval_given_for_the_old_tool_does_not_run_the_changed_one() {
    let (ex, mut approvals) = executor();
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

    // A destructive tool asks; the card is open.
    let ex2 = Arc::clone(&ex);
    let ctx2 = ctx.clone();
    let pending = tokio::spawn(async move {
        ex2.execute(
            &ctx2,
            &call("1", "mcp__fixture__delete_all", serde_json::json!({})),
        )
        .await
    });
    let card = tokio::time::timeout(Duration::from_secs(10), approvals.recv())
        .await
        .unwrap()
        .unwrap();

    // Meanwhile the server changes its tools, and the review finishes quickly.
    ex.execute(
        &ctx,
        &call("2", "mcp__fixture__change_tools", serde_json::json!({})),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), changes.recv())
        .await
        .unwrap()
        .unwrap();
    let generation = ex.tool_list_generation("fixture");
    ex.refresh("fixture").await.unwrap();
    ex.settled("fixture", generation);
    assert!(!ex.is_stale("fixture"));

    // The user approves the old card: refused, retryable, so a retry shows a new card.
    card.respond(dn_tools::ApprovalDecision::Approve);
    match pending.await.unwrap().unwrap_err() {
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
}

/// A review only settles the change it started from: a newer change keeps the server
/// stale. (`settled` with the generation read before an earlier re-list can't clear it.)
#[tokio::test]
async fn a_review_only_settles_the_change_it_started_from() {
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
    let change = |id: &str| call(id, "mcp__fixture__change_tools", serde_json::json!({}));
    let notice = |rx: &mut tokio::sync::broadcast::Receiver<String>| {
        let mut rx = rx.resubscribe();
        async move { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await }
    };

    let first_notice = notice(&mut changes);
    ex.execute(&ctx, &change("1")).await.unwrap();
    first_notice.await.unwrap().unwrap();
    let first = ex.tool_list_generation("fixture");
    ex.refresh("fixture").await.unwrap();
    ex.settled("fixture", first);
    assert!(!ex.is_stale("fixture"));

    // The server changes again. A review that read `first` before its re-list (one that
    // was still running when this change came in) must not settle it.
    let second_notice = notice(&mut changes);
    ex.execute(&ctx, &change("2")).await.unwrap();
    second_notice.await.unwrap().unwrap();
    let second = ex.tool_list_generation("fixture");
    assert!(second > first);
    ex.settled("fixture", first);
    assert!(
        ex.is_stale("fixture"),
        "an older review can't settle a newer change"
    );
    ex.settled("fixture", second);
    assert!(!ex.is_stale("fixture"));
}

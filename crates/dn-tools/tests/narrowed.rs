//! T-SKL-01: a skill's allowlist narrows the tools but never widens what runs without
//! asking. Approvals, risk tiers and per-tool choices from the executor always win.

mod common;

use common::{call, executor, fixture_server_path};
use dn_agent::{CancelToken, ToolContext, ToolExecutor};
use dn_core::Id;
use dn_core::capability::ApprovalPolicy;
use dn_tools::{
    ApprovalDecision, CapabilityOverride, Executor, McpClient, McpServerConfig, MemorySecretStore,
    Narrowed, ToolAllowlist,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn ctx() -> ToolContext {
    ToolContext {
        agent_id: "assistant.default".into(),
        caller: "chat.reply".into(),
        conversation_id: Some(Id::new()),
        run_id: Id::new(),
        step: 0,
        cancel: CancelToken::new(),
        allowed_tools: Vec::new(),
    }
}

async fn with_fixture(ex: &Arc<Executor>) {
    ex.add_server(
        McpClient::new(
            McpServerConfig::stdio("fixture", fixture_server_path().to_string_lossy(), vec![]),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap(),
    )
    .await
    .unwrap();
}

fn narrowed(ex: &Arc<Executor>, patterns: &[&str]) -> Arc<Narrowed> {
    let inner: Arc<dyn ToolExecutor> = ex.clone();
    Arc::new(Narrowed::new(
        inner,
        ToolAllowlist::new(patterns).unwrap(),
        ex.capability_resolver(),
    ))
}

fn names(t: &dyn ToolExecutor) -> Vec<String> {
    let mut v: Vec<String> = t.tools().into_iter().map(|s| s.name).collect();
    v.sort();
    v
}

#[tokio::test]
async fn only_listed_tools_are_offered_and_others_are_refused_without_running() {
    let (ex, mut rx) = executor();
    with_fixture(&ex).await;
    let t = narrowed(&ex, &["mcp.fixture.echo"]);
    assert_eq!(names(&*t), ["mcp__fixture__echo"]);

    // A write the skill doesn't list: refused before the executor, so no approval card.
    let out = t
        .execute(
            &call("1", "mcp__fixture__delete_all", serde_json::json!({})),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(rx.try_recv().is_err(), "no approval was requested");

    // A listed read tool runs.
    let out = t
        .execute(
            &call("2", "mcp__fixture__echo", serde_json::json!({"text": "hi"})),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(!out.is_error);
}

#[tokio::test]
async fn a_listed_write_tool_still_asks_and_a_denial_still_stops_it() {
    let (ex, mut rx) = executor();
    with_fixture(&ex).await;
    let t = narrowed(&ex, &["mcp.fixture.*"]);
    let t2 = Arc::clone(&t);
    let task = tokio::spawn(async move {
        t2.execute(
            &call("1", "mcp__fixture__delete_all", serde_json::json!({})),
            &ctx(),
        )
        .await
    });
    let req = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the approval gate asked")
        .unwrap();
    req.respond(ApprovalDecision::Deny);
    assert!(task.await.unwrap().unwrap().is_error);
}

#[tokio::test]
async fn per_tool_choices_win_over_the_skill() {
    let (ex, mut rx) = executor();
    with_fixture(&ex).await;
    ex.set_overrides(HashMap::from([
        // The user turned echo off: listing it in a skill doesn't bring it back.
        (
            "mcp.fixture.echo".to_string(),
            CapabilityOverride {
                disabled: true,
                ..Default::default()
            },
        ),
        // The user wants to be asked before `fail` (a read tool) runs: the skill keeps that.
        (
            "mcp.fixture.fail".to_string(),
            CapabilityOverride {
                approval: Some(ApprovalPolicy::AlwaysAsk),
                ..Default::default()
            },
        ),
    ]));
    let t = narrowed(&ex, &["mcp.fixture.echo", "mcp.fixture.fail"]);
    assert_eq!(names(&*t), ["mcp__fixture__fail"]);

    let out = t
        .execute(
            &call("1", "mcp__fixture__echo", serde_json::json!({"text": "hi"})),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(out.is_error, "a disabled tool stays off");

    let t2 = Arc::clone(&t);
    let task = tokio::spawn(async move {
        t2.execute(
            &call("2", "mcp__fixture__fail", serde_json::json!({})),
            &ctx(),
        )
        .await
    });
    let req = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("always-ask still asks")
        .unwrap();
    req.respond(ApprovalDecision::Deny);
    assert!(task.await.unwrap().unwrap().is_error);
}

#[tokio::test]
async fn an_empty_allowlist_offers_no_tools() {
    let (ex, _rx) = executor();
    with_fixture(&ex).await;
    let t = narrowed(&ex, &[]);
    assert!(t.tools().is_empty());
}

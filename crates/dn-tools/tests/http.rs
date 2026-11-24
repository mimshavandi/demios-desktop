//! Streamable HTTP MCP against an in-process test server (no third-party network).

mod common;

use common::{call, executor, fixture::Fixture, spawn_http_fixture, spawn_http_router};
use dn_core::capability::RiskTier;
use dn_tools::{
    ApprovalDecision, CallContext, McpClient, McpServerConfig, McpTransport, MemorySecretStore,
    SecretStore, ValueRef,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;

static RUN: std::sync::LazyLock<dn_core::Id> = std::sync::LazyLock::new(dn_core::Id::new);

fn ctx() -> CallContext {
    CallContext::new(Some("conv-http".into()), *RUN)
}

#[tokio::test]
async fn http_read_auto_runs_write_waits_and_replays() {
    let fixture = Fixture::default();
    let writes = fixture.writes.clone();
    let url = spawn_http_fixture(fixture).await;
    let (ex, mut rx) = executor();
    let mut cfg = McpServerConfig::http("remote", url);
    cfg.trust_annotations = Some(true);
    let client = McpClient::new(cfg, Arc::new(MemorySecretStore::new())).unwrap();
    let caps = ex.add_server(client).await.unwrap();
    assert!(
        caps.iter()
            .any(|c| c.spec.id == "mcp.remote.echo" && c.spec.risk == RiskTier::Read)
    );

    let out = ex
        .execute(
            &ctx(),
            &call(
                "1",
                "mcp.remote.echo",
                serde_json::json!({"text": "over http"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(out.output.text(), "over http");
    assert!(rx.try_recv().is_err());

    let write = call(
        "2",
        "mcp__remote__write_note",
        serde_json::json!({"note": "x"}),
    );
    let ex2 = ex.clone();
    let w = write.clone();
    let task = tokio::spawn(async move { ex2.execute(&ctx(), &w).await });
    let req = rx.recv().await.unwrap();
    assert_eq!(
        writes.load(Ordering::SeqCst),
        0,
        "no side effect before approval"
    );
    req.respond(ApprovalDecision::ApproveForConversation);
    let first = task.await.unwrap().unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 1);

    let replay = ex.execute(&ctx(), &write).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.output, first.output);
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "replay must not re-run the tool"
    );

    // LowWrite = ask_once_per_conversation: a new call in the same chat runs without asking.
    let out = ex
        .execute(
            &ctx(),
            &call(
                "3",
                "mcp.remote.write_note",
                serde_json::json!({"note": "y"}),
            ),
        )
        .await
        .unwrap();
    assert!(!out.replayed);
    assert_eq!(writes.load(Ordering::SeqCst), 2);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn http_headers_come_from_secret_refs() {
    use axum::http::StatusCode;
    let url = spawn_http_router(
        |router| {
            router.layer(axum::middleware::from_fn(
                |req: axum::extract::Request, next: axum::middleware::Next| async move {
                    if req.headers().get("x-api-key").and_then(|v| v.to_str().ok())
                        == Some("sekret")
                    {
                        Ok(next.run(req).await)
                    } else {
                        Err(StatusCode::FORBIDDEN)
                    }
                },
            ))
        },
        Fixture::default(),
    )
    .await;

    let secrets = Arc::new(MemorySecretStore::new());
    let mut cfg = McpServerConfig::http("keyed", url);
    if let McpTransport::StreamableHttp { headers, .. } = &mut cfg.transport {
        headers.insert("X-Api-Key".into(), ValueRef::secret("mcp.keyed.api_key"));
    }
    // Missing secret → config error, nothing sent.
    let c = McpClient::new(cfg.clone(), secrets.clone()).unwrap();
    assert!(c.connect().await.is_err());

    secrets.set("mcp.keyed.api_key", "sekret").unwrap();
    let c = McpClient::new(cfg, secrets).unwrap();
    let tools = c.list_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "echo"));
}

#[tokio::test]
async fn http_annotations_untrusted_by_default() {
    let url = spawn_http_fixture(Fixture::default()).await;
    let (ex, mut rx) = executor();
    let client = McpClient::new(
        McpServerConfig::http("remote", url),
        Arc::new(MemorySecretStore::new()),
    )
    .unwrap();
    let caps = ex.add_server(client).await.unwrap();
    let echo = caps.iter().find(|c| c.tool_name == "echo").unwrap();
    // readOnlyHint from an untrusted (HTTP) server can't make a tool auto-run.
    assert_eq!(echo.spec.risk, RiskTier::LowWrite);
    let delete = caps.iter().find(|c| c.tool_name == "delete_all").unwrap();
    assert_eq!(delete.spec.risk, RiskTier::Destructive);

    let ex2 = ex.clone();
    let task = tokio::spawn(async move {
        ex2.execute(
            &ctx(),
            &call("u1", "mcp.remote.echo", serde_json::json!({"text": "t"})),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert_eq!(req.prompt.capability_id, "mcp.remote.echo");
    req.respond(ApprovalDecision::Approve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn colliding_llm_names_get_distinct_suffixes() {
    // `a.b` and `a/b` sanitise to the same `a_b`; a third tool is literally named like the
    // first one's suffixed name. All three must stay distinct and resolvable.
    let lossy = dn_tools::capability::llm_tool_name("remote", "a.b");
    let literal = lossy.trim_start_matches("mcp__remote__").to_owned();
    let fixture = Fixture {
        extra_tools: Arc::new(vec![
            serde_json::json!({"name": "a.b", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}),
            serde_json::json!({"name": "a/b", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}),
            serde_json::json!({"name": literal, "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}),
        ]),
        ..Fixture::default()
    };
    let url = spawn_http_fixture(fixture).await;
    let (ex, _rx) = executor();
    let mut cfg = McpServerConfig::http("remote", url);
    cfg.trust_annotations = Some(true);
    ex.add_server(McpClient::new(cfg, Arc::new(MemorySecretStore::new())).unwrap())
        .await
        .unwrap();
    let specs = ex.tool_specs();
    let mut names: Vec<&str> = specs.iter().map(|t| t.name.as_str()).collect();
    let total = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), total, "duplicate LLM tool names: {names:?}");
    for tool in ["a.b", "a/b", literal.as_str()] {
        let cap = ex
            .capabilities()
            .into_iter()
            .find(|c| c.tool_name == tool)
            .unwrap();
        assert!(cap.llm_name.len() <= 64);
        let out = ex
            .execute(&ctx(), &call(tool, &cap.llm_name, serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(
            out.output.text(),
            tool,
            "{} resolved to the wrong tool",
            cap.llm_name
        );
    }
}

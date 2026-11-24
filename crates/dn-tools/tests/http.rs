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

fn ctx() -> CallContext {
    CallContext {
        conversation_id: "conv-http".into(),
    }
}

#[tokio::test]
async fn http_read_auto_runs_write_waits_and_replays() {
    let fixture = Fixture::default();
    let writes = fixture.writes.clone();
    let url = spawn_http_fixture(fixture).await;
    let (ex, mut rx) = executor();
    let client = McpClient::new(
        McpServerConfig::http("remote", url),
        Arc::new(MemorySecretStore::new()),
    )
    .unwrap();
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

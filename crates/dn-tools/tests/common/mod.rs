#![allow(dead_code)]

pub mod fixture;

use dn_tools::{ApprovalGate, ApprovalRequest, Executor, ledger};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Path of the stdio fixture server built from `examples/fixture_server.rs`
/// (`cargo test` builds examples). Override with `DN_TOOLS_FIXTURE_SERVER`.
pub fn fixture_server_path() -> PathBuf {
    if let Ok(p) = std::env::var("DN_TOOLS_FIXTURE_SERVER") {
        return PathBuf::from(p);
    }
    let name = format!("fixture_server{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().expect("current exe");
    // Old layout: target/<profile>/deps/<test>; new build-dir layout:
    // target/<profile>/build/<pkg>/<hash>/out/<test>. Examples land in target/<profile>/examples.
    exe.ancestors()
        .map(|dir| dir.join("examples").join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            panic!(
                "examples/{name} not found above {}; run via `cargo test` or set \
                 DN_TOOLS_FIXTURE_SERVER",
                exe.display()
            )
        })
}

pub fn ledger_db() -> Arc<Mutex<rusqlite::Connection>> {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for m in ledger::migrations() {
        conn.execute_batch(m).unwrap();
    }
    Arc::new(Mutex::new(conn))
}

pub fn executor() -> (Arc<Executor>, mpsc::Receiver<ApprovalRequest>) {
    let (ex, rx, _db) = executor_with_ledger();
    (ex, rx)
}

pub fn executor_with_ledger() -> (
    Arc<Executor>,
    mpsc::Receiver<ApprovalRequest>,
    Arc<Mutex<rusqlite::Connection>>,
) {
    let (gate, rx) = ApprovalGate::new(8);
    let db = ledger_db();
    (Arc::new(Executor::new(Arc::new(gate), db.clone())), rx, db)
}

/// Context for conversation `conv` in a fixed run (one run per test unless a test says so).
pub fn ctx_in(conv: &str, run: dn_core::Id) -> dn_tools::CallContext {
    dn_tools::CallContext::new(Some(conv.to_owned()), run)
}

pub fn call(id: &str, name: &str, args: serde_json::Value) -> dn_core::chat::ToolCall {
    dn_core::chat::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

/// Serve the fixture over Streamable HTTP on 127.0.0.1; returns the `/mcp` URL.
pub async fn spawn_http_fixture(fixture: fixture::Fixture) -> String {
    spawn_http_router(|router| router, fixture).await
}

pub async fn spawn_http_router(
    extend: impl FnOnce(axum::Router) -> axum::Router,
    fixture: fixture::Fixture,
) -> String {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    let service: StreamableHttpService<fixture::Fixture, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(fixture.clone()),
            Default::default(),
            StreamableHttpServerConfig::default(),
        );
    let router = extend(axum::Router::new().nest_service("/mcp", service));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}/mcp")
}

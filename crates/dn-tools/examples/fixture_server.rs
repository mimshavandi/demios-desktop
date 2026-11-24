//! Stdio MCP test server for dn-tools' integration tests (`cargo test` builds examples).
//! Not a product example; see `tests/common/fixture.rs`.

#[path = "../tests/common/fixture.rs"]
mod fixture;

use rmcp::ServiceExt;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    eprintln!("fixture server starting");
    let service = fixture::Fixture::default()
        .serve(rmcp::transport::stdio())
        .await
        .expect("serve");
    let _ = service.waiting().await;
}

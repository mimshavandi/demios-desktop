//! Done-check against the **reference MCP filesystem server**
//! (`@modelcontextprotocol/server-filesystem`, Node). Ignored by default: CI has no Node
//! and must not reach the npm registry.
//!
//! Run locally (needs Node ≥ 18; the package must already be installed or fetchable by npx):
//!
//! ```sh
//! npm install -g @modelcontextprotocol/server-filesystem   # once
//! cargo test -p dn-tools --test filesystem_reference -- --ignored --nocapture
//! ```
//!
//! Set `DN_MCP_FS_COMMAND` to override the launcher (default: `npx -y
//! @modelcontextprotocol/server-filesystem`; on Windows `npx.cmd`).

mod common;

use common::{call, executor};
use dn_core::capability::RiskTier;
use dn_tools::{ApprovalDecision, CallContext, McpClient, McpServerConfig, MemorySecretStore};
use std::sync::Arc;

fn launcher(root: &str) -> McpServerConfig {
    let (cmd, mut args) = match std::env::var("DN_MCP_FS_COMMAND") {
        Ok(s) => {
            let mut parts = s.split_whitespace().map(str::to_owned);
            (parts.next().unwrap(), parts.collect::<Vec<_>>())
        }
        Err(_) => (
            if cfg!(windows) { "npx.cmd" } else { "npx" }.to_owned(),
            vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
            ],
        ),
    };
    args.push(root.to_owned());
    let mut cfg = McpServerConfig::stdio("filesystem", cmd, args);
    cfg.connect_timeout_ms = 120_000;
    cfg
}

#[tokio::test]
#[ignore = "needs Node + @modelcontextprotocol/server-filesystem; see file header"]
async fn reference_filesystem_server() {
    let root = std::env::temp_dir().join(format!("dn-tools-fs-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(root.join("hello.txt"), "hello from dn-tools").unwrap();
    let root_s = root.to_string_lossy().to_string();

    let (ex, mut rx) = executor();
    let client = McpClient::new(launcher(&root_s), Arc::new(MemorySecretStore::new())).unwrap();
    let caps = ex.add_server(client).await.unwrap();
    for c in &caps {
        println!("{} -> {:?} / {:?}", c.spec.id, c.spec.risk, c.approval);
    }
    let read = caps
        .iter()
        .find(|c| c.tool_name == "read_text_file" || c.tool_name == "read_file")
        .expect("read tool");
    assert_eq!(read.spec.risk, RiskTier::Read);
    let write = caps
        .iter()
        .find(|c| c.tool_name == "write_file")
        .expect("write tool");
    assert_ne!(write.spec.risk, RiskTier::Read);

    let ctx = CallContext {
        conversation_id: "fs".into(),
    };
    let path = root.join("hello.txt").to_string_lossy().to_string();
    let out = ex
        .execute(
            &ctx,
            &call("r1", &read.spec.id, serde_json::json!({"path": path})),
        )
        .await
        .unwrap();
    assert!(out.output.text().contains("hello from dn-tools"));
    assert!(rx.try_recv().is_err(), "read must not ask");

    let target = root.join("out.txt").to_string_lossy().to_string();
    let w = call(
        "w1",
        &write.spec.id,
        serde_json::json!({"path": target, "content": "written"}),
    );
    let ex2 = ex.clone();
    let ctx2 = ctx.clone();
    let w2 = w.clone();
    let task = tokio::spawn(async move { ex2.execute(&ctx2, &w2).await });
    let req = rx.recv().await.unwrap();
    assert!(
        !root.join("out.txt").exists(),
        "nothing written before approval"
    );
    req.respond(ApprovalDecision::Approve);
    task.await.unwrap().unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("out.txt")).unwrap(),
        "written"
    );

    let replay = ex.execute(&ctx, &w).await.unwrap();
    assert!(replay.replayed);
    let _ = std::fs::remove_dir_all(&root);
}

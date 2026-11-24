//! Test MCP server used by the integration tests (over stdio via `examples/fixture_server.rs`,
//! and over Streamable HTTP in-process). Not shipped.
#![allow(dead_code)]

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone, Default)]
pub struct Fixture {
    /// Number of side effects performed by `write_note` (lets tests detect double execution).
    pub writes: Arc<AtomicU32>,
    /// Calls to `flaky` that fail before it succeeds.
    pub flaky_failures: Arc<AtomicU32>,
}

fn tool(v: serde_json::Value) -> Tool {
    serde_json::from_value(v).expect("tool json")
}

pub fn tools() -> Vec<Tool> {
    vec![
        tool(serde_json::json!({
            "name": "echo",
            "description": "Echo text back",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
            "annotations": {"readOnlyHint": true}
        })),
        tool(serde_json::json!({
            "name": "write_note",
            "description": "Store a note (side effect)",
            "inputSchema": {"type": "object", "properties": {"note": {"type": "string"}, "request_id": {"type": "string"}}, "required": ["note"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
        })),
        tool(serde_json::json!({
            "name": "delete_all",
            "description": "Delete everything",
            "inputSchema": {"type": "object"},
            "annotations": {"destructiveHint": true}
        })),
        tool(serde_json::json!({
            "name": "fail",
            "description": "Always reports an error",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true}
        })),
        tool(serde_json::json!({
            "name": "slow",
            "description": "Sleeps for 5 seconds",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true}
        })),
        tool(serde_json::json!({
            "name": "crash",
            "description": "Exits the server process (stdio only)",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true}
        })),
        tool(serde_json::json!({
            "name": "pid",
            "description": "Returns the server process id",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true}
        })),
    ]
}

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(serde_json::from_value(serde_json::json!({ "tools": tools() })).expect("list"))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let text = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let result = match request.name.as_ref() {
            "echo" => CallToolResult::success(vec![ContentBlock::text(text("text"))]),
            "write_note" => {
                let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
                CallToolResult::success(vec![ContentBlock::text(format!(
                    "stored `{}` (write #{n})",
                    text("note")
                ))])
            }
            "delete_all" => CallToolResult::success(vec![ContentBlock::text("deleted")]),
            "fail" => CallToolResult::error(vec![ContentBlock::text("it broke")]),
            "slow" => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                CallToolResult::success(vec![ContentBlock::text("done")])
            }
            "crash" => std::process::exit(3),
            "pid" => {
                CallToolResult::success(vec![ContentBlock::text(std::process::id().to_string())])
            }
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool {other}"),
                    None,
                ));
            }
        };
        Ok(result.into())
    }
}

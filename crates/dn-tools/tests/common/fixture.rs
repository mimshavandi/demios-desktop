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
    /// Extra tools (JSON) advertised in addition to [`tools`]; they echo their name.
    pub extra_tools: Arc<Vec<serde_json::Value>>,
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
            "name": "slow_write",
            "description": "A write that takes 5 seconds (no annotations: external write)",
            "inputSchema": {"type": "object"}
        })),
        tool(serde_json::json!({
            "name": "env_names",
            "description": "Comma-separated names of the server's environment variables",
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
        let mut all = tools();
        all.extend(self.extra_tools.iter().cloned().map(tool));
        Ok(serde_json::from_value(serde_json::json!({ "tools": all })).expect("list"))
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
            "slow_write" => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                self.writes.fetch_add(1, Ordering::SeqCst);
                CallToolResult::success(vec![ContentBlock::text("slow write done")])
            }
            "env_names" => {
                let mut names: Vec<String> = std::env::vars_os()
                    .filter_map(|(k, _)| k.into_string().ok())
                    .collect();
                names.sort();
                CallToolResult::success(vec![ContentBlock::text(names.join(","))])
            }
            "pid" => {
                CallToolResult::success(vec![ContentBlock::text(std::process::id().to_string())])
            }
            other if self.extra_tools.iter().any(|t| t["name"] == other) => {
                CallToolResult::success(vec![ContentBlock::text(other.to_owned())])
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

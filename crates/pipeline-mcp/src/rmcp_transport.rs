//! `rmcp` transport · standards-compliant MCP server using the official Rust SDK.
//!
//! Replaces the Day-2 hand-rolled JSON-RPC 2.0 implementation with `rmcp`'s
//! protocol layer. The 19 super tool surface stays unchanged · `ServerHandler`
//! delegates `tools/list` to our `registry()` and `tools/call` to our existing
//! `dispatch::call_tool`.
//!
//! Selected via `PIPELINE_TRANSPORT=rmcp` env var; default remains hand-rolled
//! until the rmcp implementation ships green for ≥ 1 milestone.

// Several rmcp model structs are #[non_exhaustive] · we cannot use
// struct-update syntax across the crate boundary, so the only option is
// Default::default() followed by field assignment. Allow the lint at
// module level rather than littering each constructor.
#![allow(clippy::doc_markdown, clippy::field_reassign_with_default)]

use crate::dispatch;
use crate::registry::registry;
use crate::server::ServerState;
use crate::tools::ToolRequest;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ErrorData, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool, ToolsCapability,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::stdio;
use std::borrow::Cow;
use std::sync::Arc;

/// `ServerHandler` wired against the existing dispatcher.
#[derive(Clone)]
pub struct PipelineRmcpHandler {
    state: Arc<ServerState>,
}

impl PipelineRmcpHandler {
    pub fn new(state: Arc<ServerState>) -> Self {
        Self { state }
    }
}

impl ServerHandler for PipelineRmcpHandler {
    fn get_info(&self) -> ServerInfo {
        // Build via Default + field assignment · ServerInfo is non-exhaustive.
        let mut info = ServerInfo::default();
        let mut caps = ServerCapabilities::default();
        let mut tools_cap = ToolsCapability::default();
        tools_cap.list_changed = Some(false);
        caps.tools = Some(tools_cap);
        info.capabilities = caps;

        let mut server_info = Implementation::default();
        server_info.name = "pipeline-mcp".into();
        server_info.version = crate::VERSION.into();
        server_info.website_url = Some("https://github.com/azzindani/Pipeline".into());
        info.server_info = server_info;
        info.instructions = Some(
            "Local-first CI/CD + MCP for any coding agent. \
             Each tool dispatches by `action` parameter; see the action \
             list embedded in each tool description."
                .into(),
        );
        info
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools: Vec<Tool> = registry()
            .into_iter()
            .map(|t| {
                let mut tool = Tool::new(
                    Cow::Owned(t.name.as_str().to_owned()),
                    Cow::Owned(t.describe()),
                    Arc::new(t.input_schema()),
                );
                tool.description = Some(Cow::Owned(t.describe()));
                tool
            })
            .collect();
        let mut result = ListToolsResult::default();
        result.tools = tools;
        Ok(result)
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let arguments = params.arguments.unwrap_or_default();
        let action = arguments
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let inner_args = arguments
            .get("args")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let req = ToolRequest {
            action,
            args: inner_args,
        };
        let resp = dispatch::call_tool(&params.name, req, self.state.clone()).await;
        let is_error = !resp.ok;
        let payload = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());

        let mut result = CallToolResult::default();
        result.content = vec![Content::text(payload)];
        result.is_error = Some(is_error);
        Ok(result)
    }
}

/// Run the rmcp-backed MCP server on stdio · blocks until stdin closes.
pub async fn serve_stdio_rmcp() -> Result<(), crate::McpError> {
    let state = Arc::new(ServerState::new());
    let handler = PipelineRmcpHandler::new(state);
    let service = handler
        .serve(stdio())
        .await
        .map_err(|e| crate::McpError::Transport(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| crate::McpError::Transport(e.to_string()))?;
    Ok(())
}

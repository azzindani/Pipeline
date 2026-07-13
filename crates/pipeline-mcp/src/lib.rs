//! Pipeline MCP server · 19 super tools dispatching by `action`.
//!
//! Hand-rolled JSON-RPC 2.0 over stdio. Implements `initialize` ·
//! `tools/list` · `tools/call`. Tool calls deserialize into `ToolRequest`
//! and route through `dispatch::call_tool` which fans out to per-tool
//! handlers in `handlers/`.
//!
//! Surface design rationale lives in `PLAN.md` §3.

#![allow(clippy::doc_markdown, clippy::manual_let_else)] // domain prose · early-return readability

mod auth;
mod browse;
mod dispatch;
mod fsops;
mod handlers;
#[cfg(test)]
mod http_tests;
mod http_transport;
mod library;
mod oauth;
mod ratelimit;
mod registry;
mod rmcp_transport;
mod server;
pub mod templates;
mod tools;

pub use dispatch::call_tool;
pub use registry::{ToolDescriptor, registry};
pub use server::ServerState;
pub use tools::{ToolName, ToolRequest, ToolResponse};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Start the MCP server on stdio transport · blocks until stdin closes.
///
/// Transport selection (for stdio only · use `serve_http` for HTTP mode):
/// - default: hand-rolled JSON-RPC 2.0 (Day-2 implementation)
/// - `PIPELINE_TRANSPORT=rmcp`: official `rmcp` crate (Day-8c)
///
/// Both wrap the same `dispatch::call_tool` so the 19-tool surface is
/// identical from the agent's perspective. The env var lets the rmcp
/// path bake before becoming the default.
pub async fn serve_stdio() -> Result<(), McpError> {
    match std::env::var("PIPELINE_TRANSPORT").as_deref() {
        Ok("rmcp") => rmcp_transport::serve_stdio_rmcp().await,
        _ => server::run_stdio().await,
    }
}

/// Start the MCP server on HTTP transport · blocks until process exit.
///
/// Streamable HTTP-style: POST `/mcp` with a JSON-RPC envelope, GET `/health`.
/// Bearer auth via `PIPELINE_TOKEN` (mandatory).
/// Capability gate via `PIPELINE_REMOTE_MODE` (default `read_only`).
pub async fn serve_http(bind: Option<&str>) -> Result<(), McpError> {
    http_transport::serve_http(bind).await
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

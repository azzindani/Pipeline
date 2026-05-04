//! Pipeline MCP server · 19 super tools dispatching by `action`.
//!
//! Hand-rolled JSON-RPC 2.0 over stdio. Implements `initialize` ·
//! `tools/list` · `tools/call`. Tool calls deserialize into `ToolRequest`
//! and route through `dispatch::call_tool` which fans out to per-tool
//! handlers in `handlers/`.
//!
//! Surface design rationale lives in `PLAN.md` §3.

#![allow(clippy::doc_markdown, clippy::manual_let_else)] // domain prose · early-return readability

mod dispatch;
mod handlers;
mod registry;
mod server;
mod tools;

pub use dispatch::call_tool;
pub use registry::{ToolDescriptor, registry};
pub use server::ServerState;
pub use tools::{ToolName, ToolRequest, ToolResponse};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Start the MCP server on stdio transport · blocks until stdin closes.
pub async fn serve_stdio() -> Result<(), McpError> {
    server::run_stdio().await
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

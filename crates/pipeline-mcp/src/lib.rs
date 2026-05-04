//! Pipeline MCP server · 19 super tools dispatching by `action`.
//!
//! Day-1 scope: tool registry + request/response envelope + stub `serve_stdio`
//! that prints the tool list to stderr. Real `rmcp` transport wires in Day 2.
//!
//! Surface design rationale lives in `PLAN.md` §3.

mod registry;
mod tools;

pub use registry::{ToolDescriptor, registry};
pub use tools::{ToolName, ToolRequest, ToolResponse};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Start the MCP server on stdio transport.
///
/// Day-1 implementation: prints registered tools to stderr and exits cleanly.
/// Day-2 wires `rmcp` for real protocol handling.
#[allow(clippy::unused_async)] // signature locked for Day-2 rmcp wiring
pub async fn serve_stdio() -> Result<(), McpError> {
    let tools = registry();
    eprintln!("pipeline-mcp v{VERSION} · stdio");
    eprintln!("registered tools: {}", tools.len());
    for t in &tools {
        eprintln!("  - {} ({} actions)", t.name.as_str(), t.action_count());
    }
    eprintln!("[stub] rmcp transport wires in Day 2");
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

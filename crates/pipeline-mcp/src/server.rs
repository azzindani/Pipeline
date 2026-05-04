//! Minimal JSON-RPC 2.0 over stdio · MCP-compliant.
//!
//! Methods: `initialize` · `notifications/initialized` · `tools/list` ·
//! `tools/call`. Anything else returns `-32601 method not found`.

use crate::dispatch;
use crate::registry::registry;
use crate::tools::ToolRequest;
use crate::{McpError, VERSION};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// MCP protocol version we advertise.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Shared state held across MCP requests.
#[derive(Debug, Default)]
pub struct ServerState {
    /// `pipeline_session.lock` populates this · subsequent calls inherit it.
    pub project_id: Arc<Mutex<Option<String>>>,
    /// Memory handle, lazily opened on first `pipeline_session.lock`.
    pub memory: Arc<Mutex<Option<pipeline_memory::Memory>>>,
}

impl ServerState {
    pub fn new() -> Self {
        Self::default()
    }
}

pub async fn run_stdio() -> Result<(), McpError> {
    let state = Arc::new(ServerState::new());

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, line = %line, "invalid jsonrpc");
                continue;
            }
        };
        let resp = handle(req, state.clone()).await;
        if let Some(r) = resp {
            let mut bytes = serde_json::to_vec(&r)?;
            bytes.push(b'\n');
            stdout.write_all(&bytes).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

async fn handle(req: Value, state: Arc<ServerState>) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method")?.as_str()?.to_owned();

    match method.as_str() {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "serverInfo": {"name": "pipeline-mcp", "version": VERSION},
                "capabilities": {"tools": {}}
            }
        })),
        "notifications/initialized" | "initialized" => None, // notification · no response
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": build_tool_list()}
        })),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or_default();
            let result = handle_tool_call(params, state).await;
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
        }
        "ping" => Some(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("method '{method}' not found")}
        })),
    }
}

fn build_tool_list() -> Vec<Value> {
    registry()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name.as_str(),
                "description": format!("{} Actions: {}", t.summary, t.actions.join("|")),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": t.actions},
                        "args": {"type": "object", "additionalProperties": true}
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }
            })
        })
        .collect()
}

async fn handle_tool_call(params: Value, state: Arc<ServerState>) -> Value {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let arguments = params.get("arguments").cloned().unwrap_or_default();
    let req = ToolRequest {
        action: arguments
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        args: arguments.get("args").cloned().unwrap_or(Value::Null),
    };

    let resp = dispatch::call_tool(&name, req, state).await;
    let is_error = !resp.ok;
    let text = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error
    })
}

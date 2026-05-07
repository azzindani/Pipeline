//! HTTP transport · streamable-http-style MCP server over JSON-RPC.
//!
//! Modern MCP "Streamable HTTP" transport (spec 2025-03-26). Clients POST
//! a JSON-RPC envelope to `/mcp`, server returns the JSON-RPC response.
//! No SSE upgrade yet · single request → single response. Streaming
//! progress for long-running tools lands when an actual tool needs it
//! (RE family already uses file-backed async via `re_status` polling).
//!
//! Activated via `--transport http` or `PIPELINE_TRANSPORT=http` + bind
//! address from `--bind` or `PIPELINE_BIND` (default 127.0.0.1:8080).
//!
//! # Security
//!
//! Treat this endpoint as remote code execution. Bearer auth via
//! `PIPELINE_TOKEN` is mandatory in HTTP mode · server refuses to start
//! without it. `PIPELINE_REMOTE_MODE=read_only` (default) blocks every
//! destructive action listed in `is_safe_action`. Set `PIPELINE_REMOTE_MODE=full`
//! only when bound to localhost behind an authenticated reverse proxy.

#![allow(clippy::doc_markdown)]

use crate::dispatch;
use crate::registry::registry;
use crate::server::ServerState;
use crate::tools::ToolRequest;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteMode {
    /// Default · agent can call only read-only actions.
    ReadOnly,
    /// Full surface · agent can call destructive actions.
    /// Use behind authenticated reverse proxy + TLS only.
    Full,
}

impl RemoteMode {
    pub fn from_env() -> Self {
        match std::env::var("PIPELINE_REMOTE_MODE")
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Ok("full") => Self::Full,
            _ => Self::ReadOnly,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Full => "full",
        }
    }
}

#[derive(Clone)]
struct AppState {
    server: Arc<ServerState>,
    token: Arc<String>,
    mode: RemoteMode,
}

/// Run the HTTP MCP server. Bind defaults to 127.0.0.1:8080 if `bind` is None.
///
/// Returns `Err` immediately if `PIPELINE_TOKEN` is unset. Pipeline refuses to
/// expose remote code execution without an auth token.
pub async fn serve_http(bind: Option<&str>) -> Result<(), crate::McpError> {
    let token = match std::env::var("PIPELINE_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            return Err(crate::McpError::Transport(
                "PIPELINE_TOKEN env var is required for HTTP transport · refusing to start without auth"
                    .into(),
            ));
        }
    };

    let addr_str = bind
        .map(str::to_owned)
        .or_else(|| std::env::var("PIPELINE_BIND").ok())
        .unwrap_or_else(|| "127.0.0.1:8080".into());
    let addr = SocketAddr::from_str(&addr_str)
        .map_err(|e| crate::McpError::Transport(format!("bad bind '{addr_str}': {e}")))?;

    let mode = RemoteMode::from_env();
    let app_state = AppState {
        server: Arc::new(ServerState::new()),
        token: Arc::new(token),
        mode,
    };

    let app = Router::new()
        .route("/mcp", post(mcp_handler))
        .route("/health", get(health))
        .with_state(app_state);

    eprintln!(
        "pipeline-mcp http transport · {addr} · mode={}",
        mode.as_str()
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| crate::McpError::Transport(format!("bind {addr}: {e}")))?;
    axum::serve(listener, app)
        .await
        .map_err(|e| crate::McpError::Transport(format!("serve: {e}")))?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok", "transport": "http"}))
}

/// JSON-RPC request handler. Matches the methods the stdio handler
/// already supports: initialize · notifications/initialized · tools/list · tools/call · ping.
#[allow(clippy::too_many_lines)] // single dispatch pivot · splitting hurts readability
async fn mcp_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    if !verify_token(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.get("id").cloned().unwrap_or(Value::Null),
                "error": { "code": -32001, "message": "missing or invalid bearer token" },
            })),
        )
            .into_response();
    }

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {"name": "pipeline-mcp", "version": crate::VERSION},
                    "capabilities": {"tools": {"listChanged": false}},
                    "instructions": format!(
                        "Pipeline remote MCP · mode={} · all destructive actions {} \
                         · 19 super tools · see tools/list for the full surface.",
                        state.mode.as_str(),
                        if state.mode == RemoteMode::Full { "ENABLED" } else { "BLOCKED" },
                    ),
                },
            });
            Json(resp).into_response()
        }
        "ping" => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
        "notifications/initialized" | "initialized" => StatusCode::NO_CONTENT.into_response(),
        "tools/list" => {
            let tools = build_tool_list();
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": tools},
            }))
            .into_response()
        }
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or_default();
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let arguments = params.get("arguments").cloned().unwrap_or_default();
            let action = arguments
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();

            // Capability gate · enforce read-only mode before dispatching.
            if state.mode == RemoteMode::ReadOnly && !is_safe_action(&name, &action) {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string(&json!({
                                "ok": false,
                                "data": {},
                                "next_suggested": [],
                                "memory_refs": [],
                                "error": format!(
                                    "blocked by PIPELINE_REMOTE_MODE=read_only · '{name}.{action}' is destructive · \
                                     unlock by setting PIPELINE_REMOTE_MODE=full only when behind authenticated proxy + TLS"
                                ),
                            })).unwrap_or_else(|_| "{}".into()),
                        }],
                        "isError": true,
                    },
                });
                return Json(resp).into_response();
            }

            let inner_args = arguments.get("args").cloned().unwrap_or(Value::Null);
            let tool_req = ToolRequest {
                action,
                args: inner_args,
            };
            let resp = dispatch::call_tool(&name, tool_req, state.server.clone()).await;
            let is_error = !resp.ok;
            let payload = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": payload}],
                    "isError": is_error,
                },
            }))
            .into_response()
        }
        other => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("method '{other}' not found")},
        }))
        .into_response(),
    }
}

fn verify_token(headers: &HeaderMap, expected: &str) -> bool {
    let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = auth.to_str() else {
        return false;
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(token.as_bytes(), expected.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
                        "args": {"type": "object", "additionalProperties": true},
                    },
                    "required": ["action"],
                    "additionalProperties": false,
                },
            })
        })
        .collect()
}

/// Read-only allow-list. Any `(tool, action)` pair NOT in this set is
/// considered destructive and blocked when `PIPELINE_REMOTE_MODE=read_only`.
fn is_safe_action(tool: &str, action: &str) -> bool {
    match tool {
        "pipeline_session" => matches!(
            action,
            "handover" | "context" | "file_context" | "task_context"
        ),
        "pipeline_plan" => matches!(
            action,
            "prd_read"
                | "features_list"
                | "research_notes_list"
                | "research_notes_show"
                | "risk_list"
                | "progress"
                | "milestone_progress"
        ),
        "pipeline_standards" => matches!(action, "list" | "show" | "recommend"),
        "pipeline_project" => action == "template_list",
        "pipeline_run" => matches!(action, "status" | "logs" | "fix_suggestion" | "explain"),
        "pipeline_test" => action == "flake_detect",
        "pipeline_repo" => matches!(
            action,
            "list"
                | "list_capabilities"
                | "compare"
                | "capability_graph"
                | "re_status"
                | "re_report"
        ),
        "pipeline_docker" => matches!(action, "inspect" | "logs" | "compose_ps" | "compose_logs"),
        "pipeline_data" => action == "db_diff",
        "pipeline_observe" => matches!(action, "logs_aggregate" | "perf_compare"),
        "pipeline_memory" => matches!(
            action,
            "recall" | "history" | "known_issues" | "suggest_fix" | "pattern_report" | "export"
        ),
        "pipeline_report" => matches!(
            action,
            "dashboard" | "last" | "summary" | "velocity_metrics" | "burndown"
        ),
        "pipeline_meta" => matches!(action, "version" | "self_check" | "explain" | "config_get"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_actions_recognized() {
        assert!(is_safe_action("pipeline_meta", "version"));
        assert!(is_safe_action("pipeline_run", "status"));
        assert!(is_safe_action("pipeline_repo", "list"));
    }

    #[test]
    fn destructive_actions_blocked() {
        assert!(!is_safe_action("pipeline_run", "stage"));
        assert!(!is_safe_action("pipeline_run", "commit"));
        assert!(!is_safe_action("pipeline_run", "push"));
        assert!(!is_safe_action("pipeline_docker", "build"));
        assert!(!is_safe_action("pipeline_docker", "run"));
        assert!(!is_safe_action("pipeline_simulate", "chaos_inject"));
        assert!(!is_safe_action("pipeline_deploy", "target"));
    }

    #[test]
    fn constant_time_eq_handles_lengths() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    // RemoteMode::from_env() is intentionally not unit-tested · it reads
    // process env which is racy across parallel tests. Covered by the
    // integration smoke test against a live server.
}

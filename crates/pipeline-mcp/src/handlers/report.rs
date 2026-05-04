//! `pipeline_report` handler · dashboard · last · summary.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::json;
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "dashboard" | "last" | "summary" => dashboard(state).await,
        "velocity_metrics" | "burndown" => {
            ToolResponse::not_implemented(ToolName::Report, &req.action)
        }
        other => ToolResponse {
            ok: false,
            data: json!({}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some(format!("unknown action 'pipeline_report.{other}'")),
        },
    }
}

async fn dashboard(state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => {
            return ToolResponse {
                ok: false,
                data: json!({}),
                next_suggested: vec![],
                memory_refs: vec![],
                error: Some(e),
            };
        }
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => {
            return ToolResponse {
                ok: false,
                data: json!({}),
                next_suggested: vec![],
                memory_refs: vec![],
                error: Some(e),
            };
        }
    };
    match mem.handover(&cfg.project).await {
        Ok(pack) => ToolResponse::ok(serde_json::to_value(pack).unwrap_or(json!({}))),
        Err(e) => ToolResponse {
            ok: false,
            data: json!({}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some(e.to_string()),
        },
    }
}

//! `pipeline_memory` handler · remember · recall · history.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "remember" => remember(req.args, state).await,
        "recall" => recall(req.args, state).await,
        "history" => history(req.args, state).await,
        "known_issues" | "suggest_fix" | "pattern_report" | "export" | "import" => {
            ToolResponse::not_implemented(ToolName::Memory, &req.action)
        }
        other => err(format!("unknown action 'pipeline_memory.{other}'")),
    }
}

async fn remember(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) => k,
        None => return err("missing 'key'".into()),
    };
    let value = match args.get("value").and_then(Value::as_str) {
        Some(v) => v,
        None => return err("missing 'value'".into()),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    if let Err(e) = mem.remember(&cfg.project, scope, key, value).await {
        return err(e.to_string());
    }
    ToolResponse::ok(json!({"stored": true, "scope": scope, "key": key}))
}

async fn recall(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) => k,
        None => return err("missing 'key'".into()),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    match mem.recall(&cfg.project, scope, key).await {
        Ok(v) => ToolResponse::ok(json!({"scope": scope, "key": key, "value": v})),
        Err(e) => err(e.to_string()),
    }
}

async fn history(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(10);
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    match mem.run_history(&cfg.project, limit).await {
        Ok(rows) => {
            let stripped: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "stage": r.stage,
                        "status": r.status,
                        "profile": r.profile,
                        "duration_ms": r.duration_ms,
                        "created_at": r.created_at,
                    })
                })
                .collect();
            ToolResponse::ok(json!({"runs": stripped}))
        }
        Err(e) => err(e.to_string()),
    }
}

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

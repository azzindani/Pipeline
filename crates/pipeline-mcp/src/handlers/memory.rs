//! `pipeline_memory` handler · remember · recall · history.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "remember" => remember(req.args, state).await,
        "recall" => recall(req.args, state).await,
        "history" => history(req.args, state).await,
        "suggest_fix" => suggest_fix(req.args, state).await,
        "known_issues" => known_issues(state).await,
        "pattern_report" => pattern_report(state).await,
        "export" => export(req.args, state).await,
        "import" => import(req.args, state).await,
        other => err(format!("unknown action 'pipeline_memory.{other}'")),
    }
}

async fn suggest_fix(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let error_message = match args.get("error").and_then(Value::as_str) {
        Some(e) => e.to_owned(),
        None => return err("missing 'error'".into()),
    };
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(5);
    let similar = match mem
        .find_similar_failures(&cfg.project, &error_message, limit)
        .await
    {
        Ok(v) => v,
        Err(e) => return err(e.to_string()),
    };
    let prior_fixes: Vec<Value> = similar
        .iter()
        .filter(|f| f.fix_worked == Some(1) && f.fix_applied.is_some())
        .map(|f| {
            json!({
                "fix": f.fix_applied,
                "stage": f.stage,
                "ts": f.created_at,
            })
        })
        .collect();
    ToolResponse::ok(json!({
        "matches": similar.len(),
        "prior_fixes": prior_fixes,
        "candidates": similar.iter().map(|f| json!({
            "id": f.id,
            "stage": f.stage,
            "error_message": f.error_message,
            "ts": f.created_at,
        })).collect::<Vec<_>>(),
    }))
}

async fn known_issues(state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let patterns = mem.failure_patterns(&cfg.project).await.unwrap_or_default();
    let by_stage: Vec<Value> = patterns
        .iter()
        .map(|(stage, n)| json!({"stage": stage, "count": n}))
        .collect();
    ToolResponse::ok(json!({"by_stage": by_stage, "total_stages": patterns.len()}))
}

async fn pattern_report(state: Arc<ServerState>) -> ToolResponse {
    // Same data as known_issues plus a coarse total count.
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let patterns = mem.failure_patterns(&cfg.project).await.unwrap_or_default();
    let total: i64 = patterns.iter().map(|(_, n)| *n).sum();
    ToolResponse::ok(json!({
        "total_failures": total,
        "stages": patterns.iter().map(|(s, n)| json!({"stage": s, "count": n})).collect::<Vec<_>>(),
        "tip": if total == 0 {
            "no failures yet · this project has been green"
        } else {
            "look at the most-failing stage first · agent should add a check before that stage runs"
        },
    }))
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

async fn export(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let format = args.get("format").and_then(Value::as_str).unwrap_or("json");
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };

    // Collect all known scopes into a single bundle.
    let scopes = [
        "plan",
        "feature",
        "milestone",
        "decision",
        "risk",
        "persona",
        "journey",
        "use_case",
        "research_note",
        "checkpoint",
        "perf_baseline",
    ];
    let mut bundle = serde_json::Map::new();
    for s in scopes {
        let pairs = mem.list_scope(&cfg.project, s).await.unwrap_or_default();
        let entries: Vec<Value> = pairs
            .into_iter()
            .filter_map(|(k, v)| {
                serde_json::from_str::<Value>(&v)
                    .ok()
                    .map(|val| json!({"key": k, "value": val}))
            })
            .collect();
        bundle.insert(s.into(), json!(entries));
    }
    let runs = mem
        .run_history(&cfg.project, 1_000)
        .await
        .unwrap_or_default();
    bundle.insert("runs".into(), json!(runs));

    let cwd = std::env::current_dir().unwrap_or_default();
    let path = cwd.join(".pipeline").join(format!("export.{format}"));
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let body = match format {
        "json" => serde_json::to_string_pretty(&bundle).unwrap_or_else(|_| "{}".into()),
        "markdown" => render_export_markdown(&bundle),
        "llm_context" => render_export_llm(&bundle),
        other => {
            return err(format!(
                "unsupported format '{other}' · json|markdown|llm_context"
            ));
        }
    };
    if let Err(e) = tokio::fs::write(&path, &body).await {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({
        "format": format,
        "path": path.display().to_string(),
        "scopes": scopes.len(),
        "bytes": body.len(),
    }))
}

async fn import(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let path_str = match args.get("path").and_then(Value::as_str) {
        Some(p) => p.to_owned(),
        None => return err("missing 'path' (must be json export)".into()),
    };
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let raw = match tokio::fs::read_to_string(&path_str).await {
        Ok(s) => s,
        Err(e) => return err(format!("read: {e}")),
    };
    let bundle: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return err(format!("parse: {e}")),
    };

    let mut imported = 0usize;
    let mut overwrite = 0usize;
    if let Some(obj) = bundle.as_object() {
        for (scope, entries) in obj {
            if scope == "runs" {
                continue; // runs are append-only, skip on import
            }
            if let Some(arr) = entries.as_array() {
                for entry in arr {
                    let (Some(k), Some(v)) =
                        (entry.get("key").and_then(Value::as_str), entry.get("value"))
                    else {
                        continue;
                    };
                    if mem
                        .recall(&cfg.project, scope, k)
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        overwrite += 1;
                    }
                    if mem
                        .remember(&cfg.project, scope, k, &v.to_string())
                        .await
                        .is_ok()
                    {
                        imported += 1;
                    }
                }
            }
        }
    }
    ToolResponse::ok(json!({
        "imported": imported,
        "overwrote": overwrite,
        "from": path_str,
    }))
}

fn render_export_markdown(bundle: &serde_json::Map<String, Value>) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("# Pipeline memory export\n\n");
    for (scope, entries) in bundle {
        writeln!(out, "## {scope}").ok();
        if let Some(arr) = entries.as_array() {
            writeln!(out, "{} entries\n", arr.len()).ok();
        } else {
            writeln!(out, "{entries}\n").ok();
        }
    }
    out
}

fn render_export_llm(bundle: &serde_json::Map<String, Value>) -> String {
    // Pre-chunked, prefixed with scope tags · easy to inject into any model context.
    use std::fmt::Write as _;
    let mut out = String::new();
    for (scope, entries) in bundle {
        if let Some(arr) = entries.as_array() {
            for entry in arr {
                writeln!(out, "<{scope}>").ok();
                writeln!(out, "{}", serde_json::to_string(entry).unwrap_or_default()).ok();
                writeln!(out, "</{scope}>").ok();
            }
        }
    }
    out
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

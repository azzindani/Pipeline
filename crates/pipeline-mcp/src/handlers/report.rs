//! `pipeline_report` handler · dashboard · last · summary.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "dashboard" | "last" | "summary" => dashboard(state).await,
        "velocity_metrics" => velocity_metrics(state).await,
        "burndown" => burndown(&req.args, state).await,
        other => ToolResponse {
            ok: false,
            data: json!({}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some(format!("unknown action 'pipeline_report.{other}'")),
        },
    }
}

async fn velocity_metrics(state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let runs = mem.run_history(&cfg.project, 200).await.unwrap_or_default();

    // Inner loop time = median of "fast" or "static" runs.
    let mut fast_durations: Vec<i64> = runs
        .iter()
        .filter(|r| r.profile == "fast" || r.stage == "static")
        .map(|r| r.duration_ms)
        .collect();
    fast_durations.sort_unstable();
    let median_loop_ms = if fast_durations.is_empty() {
        0
    } else {
        fast_durations[fast_durations.len() / 2]
    };

    let total_runs = runs.len();
    let pass_count = runs.iter().filter(|r| r.status == "pass").count();
    #[allow(clippy::cast_precision_loss)]
    let pass_rate = if total_runs == 0 {
        0.0
    } else {
        (pass_count as f64 / total_runs as f64) * 100.0
    };

    let patterns = mem.failure_patterns(&cfg.project).await.unwrap_or_default();
    let total_failures: i64 = patterns.iter().map(|(_, n)| *n).sum();

    ToolResponse::ok(json!({
        "total_runs": total_runs,
        "pass_rate_percent": pass_rate,
        "median_inner_loop_ms": median_loop_ms,
        "total_failures": total_failures,
        "failures_by_stage": patterns.iter().map(|(s, n)| json!({"stage": s, "count": n})).collect::<Vec<_>>(),
        "targets": {
            "median_loop_target_ms": 10_000,
            "pass_rate_target_percent": 95.0,
        },
    }))
}

async fn burndown(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let milestone = args.get("milestone").and_then(Value::as_str);
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };

    // Per-milestone if provided · else aggregate across all features.
    let features = mem
        .list_scope(&cfg.project, "feature")
        .await
        .unwrap_or_default();
    let parsed: Vec<Value> = features
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str::<Value>(&v).ok())
        .collect();

    let included: Vec<&Value> = match milestone {
        Some(name) => {
            let milestone_blob = mem
                .recall(&cfg.project, "milestone", name)
                .await
                .ok()
                .flatten();
            let feature_ids: std::collections::HashSet<String> = milestone_blob
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.get("feature_ids").and_then(Value::as_array).cloned())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            parsed
                .iter()
                .filter(|f| {
                    feature_ids.is_empty()
                        || f.get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| feature_ids.contains(id))
                })
                .collect()
        }
        None => parsed.iter().collect(),
    };
    let total = included.len();
    let done = included
        .iter()
        .filter(|f| f.get("status").and_then(Value::as_str) == Some("done"))
        .count();
    let in_progress = included
        .iter()
        .filter(|f| f.get("status").and_then(Value::as_str) == Some("in_progress"))
        .count();
    let blocked = included
        .iter()
        .filter(|f| f.get("status").and_then(Value::as_str) == Some("blocked"))
        .count();
    let remaining = total.saturating_sub(done);
    #[allow(clippy::cast_precision_loss)]
    let percent_done = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64) * 100.0
    };
    ToolResponse::ok(json!({
        "milestone": milestone,
        "total": total,
        "done": done,
        "in_progress": in_progress,
        "blocked": blocked,
        "remaining": remaining,
        "percent_done": percent_done,
    }))
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

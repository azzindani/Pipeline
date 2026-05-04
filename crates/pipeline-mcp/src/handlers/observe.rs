//! `pipeline_observe` handler · metrics setup · perf baseline/compare.
//!
//! Day-6 wires: metrics_setup, perf_baseline, perf_compare. Other 6
//! actions return not_implemented (logs_aggregate needs centralized
//! collector · traces/alerts/optimize land MVP).

#![allow(clippy::doc_markdown)]

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "metrics_setup" => metrics_setup(&req.args).await,
        "perf_baseline" => perf_baseline(&req.args, state).await,
        "perf_compare" => perf_compare(&req.args, state).await,
        "logs_aggregate"
        | "traces_setup"
        | "alerts_define"
        | "optimize_suggest"
        | "image_size_optimize"
        | "query_optimize" => ToolResponse::not_implemented(ToolName::Observe, &req.action),
        other => err(format!("unknown action 'pipeline_observe.{other}'")),
    }
}

async fn metrics_setup(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let prom_path = cwd.join("observability/prometheus.yml");
    let otel_path = cwd.join("observability/otel-collector.yml");
    if let Some(parent) = prom_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if let Err(e) = tokio::fs::write(&prom_path, PROMETHEUS_YML).await {
        return err(format!("write prometheus.yml: {e}"));
    }
    if let Err(e) = tokio::fs::write(&otel_path, OTEL_YML).await {
        return err(format!("write otel-collector.yml: {e}"));
    }
    ToolResponse::ok(json!({
        "stack": stack,
        "prometheus": prom_path.display().to_string(),
        "otel_collector": otel_path.display().to_string(),
    }))
}

async fn perf_baseline(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let suite = args
        .get("suite")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let metrics = args.get("metrics").cloned().unwrap_or(json!({}));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let blob = json!({"suite": suite, "metrics": metrics, "ts": pipeline_memory::now_rfc3339()});
    if let Err(e) = mem
        .remember(&project, "perf_baseline", suite, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(blob)
}

async fn perf_compare(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let suite = args
        .get("suite")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let current = args.get("metrics").cloned().unwrap_or(json!({}));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let baseline = match mem.recall(&project, "perf_baseline", suite).await {
        Ok(Some(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => v,
            Err(e) => return err(format!("corrupt baseline: {e}")),
        },
        Ok(None) => {
            return err(format!(
                "no baseline for suite '{suite}' · call perf_baseline first"
            ));
        }
        Err(e) => return err(e.to_string()),
    };
    let baseline_metrics = baseline.get("metrics").cloned().unwrap_or(json!({}));
    let mut deltas = serde_json::Map::new();
    if let (Some(b_obj), Some(c_obj)) = (baseline_metrics.as_object(), current.as_object()) {
        for (k, b_val) in b_obj {
            if let (Some(b_n), Some(c_val)) = (b_val.as_f64(), c_obj.get(k)) {
                if let Some(c_n) = c_val.as_f64() {
                    let delta = c_n - b_n;
                    let pct = if b_n.abs() > f64::EPSILON {
                        (delta / b_n) * 100.0
                    } else {
                        0.0
                    };
                    deltas.insert(
                        k.clone(),
                        json!({"baseline": b_n, "current": c_n, "delta": delta, "pct": pct}),
                    );
                }
            }
        }
    }
    ToolResponse::ok(json!({"suite": suite, "deltas": deltas}))
}

async fn cfg_project(state: &Arc<ServerState>) -> Result<String, String> {
    if let Some(p) = state.project_id.lock().await.clone() {
        return Ok(p);
    }
    load_config_in_cwd().map(|c| c.project)
}

const PROMETHEUS_YML: &str = "global:\n  scrape_interval: 15s\n\nscrape_configs:\n  - job_name: app\n    static_configs:\n      - targets: ['app:8080']\n";

const OTEL_YML: &str = "receivers:\n  otlp:\n    protocols:\n      grpc:\n      http:\nprocessors:\n  batch:\nexporters:\n  logging:\n    loglevel: info\nservice:\n  pipelines:\n    traces:\n      receivers: [otlp]\n      processors: [batch]\n      exporters: [logging]\n    metrics:\n      receivers: [otlp]\n      processors: [batch]\n      exporters: [logging]\n";

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

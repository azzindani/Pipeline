//! `pipeline_observe` handler · metrics setup · perf baseline/compare.
//!
//! Day-6 wires: metrics_setup, perf_baseline, perf_compare. Other 6
//! actions return not_implemented (logs_aggregate needs centralized
//! collector · traces/alerts/optimize land MVP).

#![allow(clippy::doc_markdown)]

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "metrics_setup" => metrics_setup(&req.args).await,
        "perf_baseline" => perf_baseline(&req.args, state).await,
        "perf_compare" => perf_compare(&req.args, state).await,
        "logs_aggregate" => logs_aggregate(&req.args).await,
        "traces_setup" => traces_setup(&req.args).await,
        "alerts_define" => alerts_define(&req.args).await,
        "optimize_suggest" => optimize_suggest(&req.args, state).await,
        "image_size_optimize" => image_size_optimize(&req.args).await,
        "query_optimize" => query_optimize(&req.args).await,
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

async fn logs_aggregate(args: &Value) -> ToolResponse {
    let env = args.get("env").and_then(Value::as_str).unwrap_or("dev");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let compose = cwd.join("docker-compose.yml");
    if !compose.exists() {
        return err("no docker-compose.yml · cannot aggregate logs".into());
    }
    let output = match tokio::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose.display().to_string(),
            "logs",
            "--no-color",
            "--tail=500",
        ])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("docker compose logs: {e}")),
    };
    ToolResponse::ok(json!({
        "env": env,
        "logs": String::from_utf8_lossy(&output.stdout).into_owned(),
    }))
}

#[allow(clippy::unused_async)]
async fn traces_setup(_args: &Value) -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join("observability/jaeger-compose.yml");
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("mkdir: {e}"));
        }
    }
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    let body = "services:\n  jaeger:\n    image: jaegertracing/all-in-one:latest\n    ports:\n      - \"16686:16686\"\n      - \"4317:4317\"\n      - \"4318:4318\"\n    environment:\n      COLLECTOR_OTLP_ENABLED: \"true\"\n";
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"path": path.display().to_string(), "ui": "http://localhost:16686"}))
}

#[allow(clippy::unused_async)]
async fn alerts_define(args: &Value) -> ToolResponse {
    let rule = match args.get("rule").and_then(Value::as_str) {
        Some(r) => r.to_owned(),
        None => return err("missing 'rule' (e.g. 'p99_latency > 500ms')".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join("observability/alerts.yml");
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("mkdir: {e}"));
        }
    }
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.is_empty() {
        existing.push_str("groups:\n  - name: pipeline\n    rules:\n");
    }
    writeln!(existing, "      - alert: {}", rule.replace(' ', "_")).ok();
    writeln!(existing, "        expr: {rule}").ok();
    writeln!(existing, "        for: 5m").ok();
    if let Err(e) = std::fs::write(&path, &existing) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"rule": rule, "path": path.display().to_string()}))
}

async fn optimize_suggest(_args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    let runs = mem.run_history(&cfg.project, 50).await.unwrap_or_default();
    let mut suggestions: Vec<&str> = Vec::new();
    let avg_static = avg_duration(&runs, "static");
    let avg_unit = avg_duration(&runs, "unit");
    if avg_static > 5_000 {
        suggestions.push("Static stage > 5s · consider --offline + cargo-chef caching");
    }
    if avg_unit > 30_000 {
        suggestions.push("Unit stage > 30s · split test crates · use cargo-nextest");
    }
    if avg_static + avg_unit > 10_000 {
        suggestions
            .push("Inner loop > 10s target (PLAN.md §8) · profile slow tests with --report-time");
    }
    ToolResponse::ok(json!({
        "avg_static_ms": avg_static,
        "avg_unit_ms": avg_unit,
        "suggestions": suggestions,
    }))
}

fn avg_duration(runs: &[pipeline_memory::RunRecord], stage: &str) -> i64 {
    let filtered: Vec<i64> = runs
        .iter()
        .filter(|r| r.stage == stage)
        .map(|r| r.duration_ms)
        .collect();
    if filtered.is_empty() {
        0
    } else {
        #[allow(clippy::cast_possible_wrap)]
        let sum: i64 = filtered.iter().sum();
        #[allow(clippy::cast_possible_wrap)]
        let n = filtered.len() as i64;
        sum / n
    }
}

async fn image_size_optimize(args: &Value) -> ToolResponse {
    let image = args.get("image").and_then(Value::as_str);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    if let Some(img) = image {
        let output = match tokio::process::Command::new("docker")
            .args([
                "history",
                "--human",
                "--format",
                "{{.Size}}\t{{.CreatedBy}}",
                img,
            ])
            .current_dir(&cwd)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => return err(format!("docker history: {e}")),
        };
        ToolResponse::ok(json!({
            "image": img,
            "history": String::from_utf8_lossy(&output.stdout).into_owned(),
            "tip": "biggest layer first · merge RUN steps · clean apt lists · multi-stage",
        }))
    } else {
        err("missing 'image' · pass an image tag to inspect".into())
    }
}

async fn query_optimize(args: &Value) -> ToolResponse {
    let sql = match args.get("sql").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'sql'".into()),
    };
    let dsn = args.get("dsn").and_then(Value::as_str);
    let Some(dsn) = dsn else {
        return ToolResponse::ok(json!({
            "sql": sql,
            "note": "no DSN provided · cannot run EXPLAIN · supply 'dsn' (postgres-only)",
        }));
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let explain = format!("EXPLAIN ANALYZE {sql}");
    let output = match tokio::process::Command::new("psql")
        .args([dsn, "-c", &explain])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("psql: {e}")),
    };
    ToolResponse::ok(json!({
        "sql": sql,
        "explain": String::from_utf8_lossy(&output.stdout).into_owned(),
        "stderr": String::from_utf8_lossy(&output.stderr).into_owned(),
    }))
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

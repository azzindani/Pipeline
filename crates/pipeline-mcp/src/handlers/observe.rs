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
use std::collections::{BTreeMap, BTreeSet};
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

/// How many recorded runs are read when measuring · a window, ✗ all history,
/// so a baseline reflects the code as it is now.
const PERF_WINDOW: i64 = 200;
/// Samples a stage needs before its mean is worth calling a baseline.
const PERF_MIN_RUNS: u64 = 3;
/// Percent slower than baseline that fails the call by default.
const PERF_THRESHOLD_PCT: f64 = 10.0;

/// Measured timings for one stage.
struct StageStat {
    samples: u64,
    mean_ms: i64,
    min_ms: i64,
    max_ms: i64,
}

/// Measure a performance baseline from **recorded** stage durations.
///
/// ! This used to store whatever `metrics` the caller passed, defaulting to
/// `{}` — so every regression gate built on it was inert, and a baseline of
/// nothing compared equal to everything. The numbers now come from
/// `run_history`, the same genuinely measured data `optimize_suggest` reads.
async fn perf_baseline(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let suite = args
        .get("suite")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let profile = args.get("profile").and_then(Value::as_str);
    let stages = string_set(args.get("stages"));
    let min_runs = args
        .get("min_runs")
        .and_then(Value::as_u64)
        .unwrap_or(PERF_MIN_RUNS)
        .max(1);
    let (mem, project) = match memory_and_project(&state).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let runs = match mem.run_history(&project, PERF_WINDOW).await {
        Ok(r) => r,
        // ! A failed read is UNKNOWN · storing an empty baseline here would
        // define "fast" as "any number at all".
        Err(e) => return err(format!("run_history: {e}")),
    };
    let measured = measure_stages(&runs, profile, stages.as_ref());
    let blob = match baseline_blob(suite, profile, &measured, min_runs) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    if let Err(e) = mem
        .remember(&project, "perf_baseline", suite, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(blob)
}

/// ! Only passing runs are samples. A failed run's duration measures how fast
/// the suite broke, ✗ how fast it is — mixing them makes a baseline that moves
/// whenever the failure mode changes.
fn measure_stages(
    runs: &[pipeline_memory::RunRecord],
    profile: Option<&str>,
    stages: Option<&BTreeSet<String>>,
) -> BTreeMap<String, StageStat> {
    let mut buckets: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for r in runs {
        if r.status != "pass" {
            continue;
        }
        if profile.is_some_and(|p| p != r.profile) {
            continue;
        }
        if stages.is_some_and(|s| !s.contains(&r.stage)) {
            continue;
        }
        buckets
            .entry(r.stage.clone())
            .or_default()
            .push(r.duration_ms);
    }
    buckets
        .into_iter()
        .map(|(stage, ms)| {
            let sum: i64 = ms.iter().sum();
            let n = i64::try_from(ms.len()).unwrap_or(1).max(1);
            let stat = StageStat {
                samples: ms.len() as u64,
                mean_ms: sum / n,
                min_ms: ms.iter().copied().min().unwrap_or(0),
                max_ms: ms.iter().copied().max().unwrap_or(0),
            };
            (stage, stat)
        })
        .collect()
}

/// Build the stored baseline · pure, so "insufficient history refuses" is
/// testable without a database.
fn baseline_blob(
    suite: &str,
    profile: Option<&str>,
    measured: &BTreeMap<String, StageStat>,
    min_runs: u64,
) -> Result<Value, String> {
    let kept: Vec<(&String, &StageStat)> = measured
        .iter()
        .filter(|(_, s)| s.samples >= min_runs)
        .collect();
    if kept.is_empty() {
        let best = measured.values().map(|s| s.samples).max().unwrap_or(0);
        return Err(format!(
            "insufficient history · no stage has {min_runs} passing runs recorded (best: {best}) \
             · run the suite {} more time(s) · ✗ storing an empty baseline",
            min_runs - best
        ));
    }
    let dropped: Vec<&String> = measured
        .iter()
        .filter(|(_, s)| s.samples < min_runs)
        .map(|(k, _)| k)
        .collect();
    let mut stages = serde_json::Map::new();
    let mut metrics = serde_json::Map::new();
    let mut total = 0_u64;
    for (stage, s) in &kept {
        total += s.samples;
        stages.insert(
            (*stage).clone(),
            json!({"samples": s.samples, "mean_ms": s.mean_ms,
                   "min_ms": s.min_ms, "max_ms": s.max_ms}),
        );
        metrics.insert(format!("{stage}_ms"), json!(s.mean_ms));
    }
    Ok(json!({
        "suite": suite,
        "profile": profile,
        "source": "run_history",
        "measured": true,
        "runs_analyzed": total,
        "min_runs": min_runs,
        "stages": stages,
        "metrics": metrics,
        "stages_below_min_runs": dropped,
        "ts": pipeline_memory::now_rfc3339(),
    }))
}

/// Compare current timings against the stored baseline and **rule on them**.
///
/// ! This used to return raw deltas with no threshold, so a +400% regression
/// still came back `ok:true` — a gate that cannot fail is not a gate.
async fn perf_compare(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let suite = args
        .get("suite")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let threshold = args
        .get("threshold_pct")
        .and_then(Value::as_f64)
        .unwrap_or(PERF_THRESHOLD_PCT);
    let (mem, project) = match memory_and_project(&state).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let baseline = match load_baseline(&mem, &project, suite).await {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    // Caller-supplied readings win · otherwise measure the same way the baseline
    // was measured, rather than comparing the baseline against an empty object.
    let supplied = args
        .get("metrics")
        .filter(|m| m.as_object().is_some_and(|o| !o.is_empty()));
    let current = if let Some(m) = supplied {
        m.clone()
    } else {
        let runs = match mem.run_history(&project, PERF_WINDOW).await {
            Ok(r) => r,
            Err(e) => return err(format!("run_history: {e}")),
        };
        let profile = baseline.get("profile").and_then(Value::as_str);
        metrics_of(&measure_stages(&runs, profile, None))
    };
    let baseline_metrics = baseline.get("metrics").cloned().unwrap_or(json!({}));
    let (deltas, regressions) = match compare_metrics(&baseline_metrics, &current, threshold) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    ToolResponse {
        ok: regressions.is_empty(),
        data: json!({
            "suite": suite,
            "threshold_pct": threshold,
            "verdict": if regressions.is_empty() { "pass" } else { "regression" },
            "deltas": deltas,
            "regressions": regressions,
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if regressions.is_empty() {
            None
        } else {
            Some(format!(
                "performance regression beyond {threshold}% on: {}",
                regressions.join(", ")
            ))
        },
    }
}

/// Per-key delta plus the keys that breached the threshold.
///
/// ! An empty intersection is an error, ✗ "no regressions". Comparing two
/// objects that share no key is zero work, and zero work must not read as a
/// pass.
fn compare_metrics(
    baseline: &Value,
    current: &Value,
    threshold_pct: f64,
) -> Result<(serde_json::Map<String, Value>, Vec<String>), String> {
    let (Some(b_obj), Some(c_obj)) = (baseline.as_object(), current.as_object()) else {
        return Err("baseline or current metrics are not objects · nothing was compared".into());
    };
    let mut deltas = serde_json::Map::new();
    let mut regressions = Vec::new();
    for (k, b_val) in b_obj {
        let (Some(b_n), Some(c_n)) = (b_val.as_f64(), c_obj.get(k).and_then(Value::as_f64)) else {
            continue;
        };
        let delta = c_n - b_n;
        // A zero baseline has no percentage · reporting 0.0 there silently puts
        // every possible regression inside the threshold.
        let (pct, over) = if b_n.abs() > f64::EPSILON {
            let p = delta / b_n * 100.0;
            (json!(p), p > threshold_pct)
        } else {
            (Value::Null, c_n > 0.0)
        };
        if over {
            regressions.push(k.clone());
        }
        deltas.insert(
            k.clone(),
            json!({"baseline": b_n, "current": c_n, "delta": delta,
                   "pct": pct, "regression": over}),
        );
    }
    if deltas.is_empty() {
        return Err(
            "no metric key is present in both the baseline and the current reading · \
             nothing was compared · ✗ a pass"
                .into(),
        );
    }
    Ok((deltas, regressions))
}

fn metrics_of(measured: &BTreeMap<String, StageStat>) -> Value {
    Value::Object(
        measured
            .iter()
            .map(|(stage, s)| (format!("{stage}_ms"), json!(s.mean_ms)))
            .collect(),
    )
}

fn string_set(v: Option<&Value>) -> Option<BTreeSet<String>> {
    let arr = v?.as_array()?;
    let set: BTreeSet<String> = arr
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect();
    if set.is_empty() { None } else { Some(set) }
}

async fn load_baseline(
    mem: &pipeline_memory::Memory,
    project: &str,
    suite: &str,
) -> Result<Value, String> {
    match mem.recall(project, "perf_baseline", suite).await {
        Ok(Some(s)) => {
            serde_json::from_str::<Value>(&s).map_err(|e| format!("corrupt baseline: {e}"))
        }
        Ok(None) => Err(format!(
            "no baseline for suite '{suite}' · call perf_baseline first"
        )),
        Err(e) => Err(e.to_string()),
    }
}

async fn memory_and_project(
    state: &Arc<ServerState>,
) -> Result<(pipeline_memory::Memory, String), String> {
    let project = cfg_project(state).await?;
    let mem = ensure_memory(state).await?;
    Ok((mem, project))
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
    // ! A dead daemon exits non-zero with empty stdout · reporting that as
    // ok:true, logs:"" reads as "the services produced no output".
    if !output.status.success() {
        return err(tool_failed(
            "docker compose logs",
            output.status.code(),
            &output.stderr,
        ));
    }
    ToolResponse::ok(json!({
        "env": env,
        "logs": String::from_utf8_lossy(&output.stdout).into_owned(),
    }))
}

/// Uniform message for a child process that ran but refused the work.
fn tool_failed(tool: &str, code: Option<i32>, stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr);
    let detail = detail.trim();
    let code = code.map_or_else(|| "signal".to_owned(), |c| c.to_string());
    if detail.is_empty() {
        format!("{tool} exited {code} · no stderr")
    } else {
        format!("{tool} exited {code} · {detail}")
    }
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
    // ! Only "absent" means empty. Treating a permission/encoding error as an
    // empty file makes the write below silently destroy every existing rule and
    // still report ok.
    let mut existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return err(format!("read {}: {e}", path.display())),
    };
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
    match optimize_payload(&mem, &cfg.project).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

/// ! A DB error propagates. Reporting `avg_static_ms:0, suggestions:[]` on a
/// failed read tells the agent its pipeline is fast · that is a fabricated
/// measurement. Zero recorded runs is `null` + a note, ✗ a measured zero.
async fn optimize_payload(mem: &pipeline_memory::Memory, project: &str) -> Result<Value, String> {
    let runs = mem
        .run_history(project, 50)
        .await
        .map_err(|e| format!("run_history: {e}"))?;
    if runs.is_empty() {
        return Ok(json!({
            "runs_analyzed": 0,
            "avg_static_ms": Value::Null,
            "avg_unit_ms": Value::Null,
            "suggestions": [],
            "note": "no runs recorded yet · insufficient data · ✗ evidence the pipeline is fast",
        }));
    }
    let avg_static = avg_duration(&runs, "static");
    let avg_unit = avg_duration(&runs, "unit");
    let mut suggestions: Vec<&str> = Vec::new();
    if avg_static.is_some_and(|v| v > 5_000) {
        suggestions.push("Static stage > 5s · consider --offline + cargo-chef caching");
    }
    if avg_unit.is_some_and(|v| v > 30_000) {
        suggestions.push("Unit stage > 30s · split test crates · use cargo-nextest");
    }
    // Only meaningful once at least one of the two stages was actually measured.
    if (avg_static.is_some() || avg_unit.is_some())
        && avg_static.unwrap_or(0) + avg_unit.unwrap_or(0) > 10_000
    {
        suggestions
            .push("Inner loop > 10s target (PLAN.md §8) · profile slow tests with --report-time");
    }
    Ok(json!({
        "runs_analyzed": runs.len(),
        "avg_static_ms": avg_static,
        "avg_unit_ms": avg_unit,
        "suggestions": suggestions,
    }))
}

/// `None` when the stage never ran · ✗ 0, which is indistinguishable from an
/// instant stage and silently satisfies every "is it fast?" threshold below.
fn avg_duration(runs: &[pipeline_memory::RunRecord], stage: &str) -> Option<i64> {
    let filtered: Vec<i64> = runs
        .iter()
        .filter(|r| r.stage == stage)
        .map(|r| r.duration_ms)
        .collect();
    if filtered.is_empty() {
        return None;
    }
    let sum: i64 = filtered.iter().sum();
    #[allow(clippy::cast_possible_wrap)]
    let n = filtered.len() as i64;
    Some(sum / n)
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
        // ! A nonexistent image exits non-zero · ok:true with an empty history
        // plus a canned tip reads as a completed analysis of a tiny image.
        if !output.status.success() {
            return err(tool_failed(
                "docker history",
                output.status.code(),
                &output.stderr,
            ));
        }
        let history = String::from_utf8_lossy(&output.stdout);
        ToolResponse::ok(layer_analysis(img, &history))
    } else {
        err("missing 'image' · pass an image tag to inspect".into())
    }
}

/// Advice is emitted only when layer data was actually obtained · a tip printed
/// over zero layers is a claim about an image nobody read.
fn layer_analysis(image: &str, history: &str) -> Value {
    let layers = history.lines().filter(|l| !l.trim().is_empty()).count();
    if layers == 0 {
        return json!({
            "image": image,
            "layers": 0,
            "history": history,
            "note": "docker history returned no layers · nothing to analyse",
        });
    }
    json!({
        "image": image,
        "layers": layers,
        "history": history,
        "tip": "biggest layer first · merge RUN steps · clean apt lists · multi-stage",
    })
}

async fn query_optimize(args: &Value) -> ToolResponse {
    let sql = match args.get("sql").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'sql'".into()),
    };
    let dsn = args.get("dsn").and_then(Value::as_str);
    // ! No DSN → no EXPLAIN ran → ✗ ok. Success for zero work done invites the
    // agent to treat an unanalysed query as analysed.
    let Some(dsn) = dsn else {
        return err("no 'dsn' · EXPLAIN was not run · supply a postgres DSN".into());
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
    // ! Syntax error, auth failure and unreachable host all exit non-zero with
    // empty stdout · ok:true, explain:"" claims the plan was read.
    if !output.status.success() {
        return err(tool_failed("psql", output.status.code(), &output.stderr));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_memory::{Memory, NewRun};

    async fn fresh() -> Memory {
        let m = Memory::open_in_memory().await.expect("open");
        m.upsert_project("p1", "pipeline", "rust")
            .await
            .expect("upsert");
        m
    }

    /// Closed pool → every query errors · stands in for an unreadable memory.db.
    async fn broken() -> Memory {
        let m = fresh().await;
        m.pool().close().await;
        m
    }

    async fn run(m: &Memory, stage: &str, ms: u128) {
        m.log_run(&NewRun {
            project_id: "p1",
            session_id: None,
            profile: "fast",
            stage,
            status: "pass",
            duration_ms: ms,
            triggered_by: None,
            commit_sha: None,
            stdout: None,
            stderr: None,
            failure_json: None,
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_database_error_is_never_reported_as_a_fast_pipeline() {
        // Regression: run_history().unwrap_or_default() made an unreadable DB report
        // avg_static_ms:0, suggestions:[] — "your pipeline is fast" from the code
        // path that means "I could not read the runs".
        let e = optimize_payload(&broken().await, "p1")
            .await
            .expect_err("a failed read must not produce timings");
        assert!(e.contains("run_history"), "{e}");
    }

    #[tokio::test]
    async fn no_runs_recorded_is_insufficient_data_not_a_measured_zero() {
        let v = optimize_payload(&fresh().await, "p1").await.unwrap();
        assert_eq!(v["runs_analyzed"], 0);
        assert!(v["avg_static_ms"].is_null(), "✗ 0, which reads as instant");
        assert!(v["note"].as_str().unwrap().contains("insufficient data"));
    }

    #[tokio::test]
    async fn a_stage_that_never_ran_is_null_not_zero() {
        let m = fresh().await;
        run(&m, "static", 1_000).await;
        let v = optimize_payload(&m, "p1").await.unwrap();
        assert_eq!(v["avg_static_ms"], 1_000);
        // ! unit never ran · 0 would silently satisfy the "< 30s" threshold.
        assert!(v["avg_unit_ms"].is_null());
        assert_eq!(v["runs_analyzed"], 1);
    }

    #[tokio::test]
    async fn a_slow_stage_still_produces_its_suggestion() {
        let m = fresh().await;
        run(&m, "static", 6_000).await;
        let v = optimize_payload(&m, "p1").await.unwrap();
        let s = v["suggestions"].as_array().unwrap();
        assert!(
            s.iter()
                .any(|x| x.as_str().unwrap().contains("Static stage > 5s"))
        );
    }

    #[test]
    fn avg_duration_distinguishes_no_samples_from_a_zero_average() {
        let runs: Vec<pipeline_memory::RunRecord> = vec![];
        assert_eq!(avg_duration(&runs, "static"), None);
    }

    #[test]
    fn a_failed_child_process_names_the_tool_and_its_stderr() {
        // Regression: exit status was never checked, so a dead docker daemon or a
        // nonexistent image returned ok:true with an empty payload.
        let msg = tool_failed("docker history", Some(1), b"No such image: nope\n");
        assert!(msg.contains("docker history"), "{msg}");
        assert!(msg.contains("No such image"), "{msg}");
        assert!(msg.contains('1'), "{msg}");
        // A process killed by a signal has no code · still reported, ✗ swallowed.
        assert!(tool_failed("psql", None, b"").contains("signal"));
    }

    #[test]
    fn no_layer_data_means_no_canned_advice() {
        // Regression: the "biggest layer first" tip was emitted unconditionally,
        // whether or not any layer was ever read.
        let v = layer_analysis("nope:latest", "");
        assert_eq!(v["layers"], 0);
        assert!(v["tip"].is_null(), "✗ advice about an image nobody read");
        assert!(v["note"].as_str().unwrap().contains("no layers"));
    }

    #[test]
    fn real_layer_data_is_analysed() {
        let v = layer_analysis("app:1", "10MB\tRUN apt-get\n2MB\tCOPY .\n");
        assert_eq!(v["layers"], 2);
        assert!(v["tip"].as_str().unwrap().contains("biggest layer first"));
    }

    #[tokio::test]
    async fn query_optimize_without_a_dsn_is_a_refusal_not_a_success() {
        // Regression: no DSN returned ok:true for zero work done, inviting the agent
        // to treat an unanalysed query as analysed.
        let r = query_optimize(&json!({"sql": "SELECT 1"})).await;
        assert!(!r.ok, "no EXPLAIN ran · ✗ ok");
        assert!(r.error.unwrap().contains("dsn"));
    }

    #[tokio::test]
    async fn query_optimize_still_requires_sql() {
        let r = query_optimize(&json!({})).await;
        assert!(!r.ok);
    }

    // ───────────────────────── perf baseline / compare ─────────────────────────

    fn record(stage: &str, status: &str, ms: i64) -> pipeline_memory::RunRecord {
        pipeline_memory::RunRecord {
            id: format!("{stage}-{ms}-{status}"),
            project_id: "p1".into(),
            session_id: None,
            profile: "fast".into(),
            stage: stage.into(),
            status: status.into(),
            duration_ms: ms,
            created_at: "2026-01-01T00:00:00Z".into(),
            stdout: None,
            stderr: None,
            failure_json: None,
        }
    }

    #[test]
    fn a_baseline_is_measured_not_supplied() {
        // Regression: perf_baseline stored whatever `metrics` the caller passed,
        // so the gate compared a fiction against a fiction. The numbers must come
        // from recorded stage durations.
        let runs = vec![
            record("static", "pass", 1_000),
            record("static", "pass", 2_000),
            record("static", "pass", 3_000),
        ];
        let measured = measure_stages(&runs, None, None);
        let b = baseline_blob("default", None, &measured, 3).unwrap();
        assert_eq!(b["measured"], json!(true));
        assert_eq!(b["source"], "run_history");
        assert_eq!(b["metrics"]["static_ms"], 2_000);
        assert_eq!(b["stages"]["static"]["samples"], 3);
        assert_eq!(b["stages"]["static"]["min_ms"], 1_000);
        assert_eq!(b["stages"]["static"]["max_ms"], 3_000);
        assert_eq!(b["runs_analyzed"], 3);
    }

    #[test]
    fn insufficient_history_refuses_rather_than_storing_an_empty_baseline() {
        // ! An empty baseline compares equal to everything · the gate is then
        // inert forever, and nobody finds out.
        let measured = measure_stages(&[record("static", "pass", 900)], None, None);
        let e = baseline_blob("default", None, &measured, 3).unwrap_err();
        assert!(e.contains("insufficient history"), "{e}");
        assert!(
            e.contains("2 more time(s)"),
            "must say how many are needed: {e}"
        );
        // No history at all is the same refusal, ✗ a zero baseline.
        let e = baseline_blob("default", None, &BTreeMap::new(), 3).unwrap_err();
        assert!(e.contains("insufficient history"), "{e}");
    }

    #[test]
    fn a_failed_run_is_not_a_performance_sample() {
        // A failed run's duration measures how fast the suite broke · including
        // it makes the baseline move whenever the failure mode changes.
        let runs = vec![
            record("unit", "pass", 1_000),
            record("unit", "fail", 10),
            record("unit", "pass", 3_000),
        ];
        let m = measure_stages(&runs, None, None);
        assert_eq!(m["unit"].samples, 2);
        assert_eq!(m["unit"].mean_ms, 2_000);
    }

    #[test]
    fn a_stage_short_of_samples_is_dropped_visibly_not_silently() {
        let runs = vec![
            record("static", "pass", 100),
            record("static", "pass", 200),
            record("unit", "pass", 5_000),
        ];
        let b = baseline_blob("default", None, &measure_stages(&runs, None, None), 2).unwrap();
        assert!(b["metrics"]["static_ms"].is_number());
        assert!(b["metrics"]["unit_ms"].is_null(), "unit has 1 sample");
        assert_eq!(b["stages_below_min_runs"], json!(["unit"]));
    }

    #[test]
    fn a_regression_beyond_threshold_fails_the_call() {
        // Regression: perf_compare returned raw deltas with no threshold, so a
        // +400% blow-up still came back ok:true. A gate that cannot fail is not a
        // gate.
        let base = json!({"unit_ms": 1_000});
        let (deltas, regressions) =
            compare_metrics(&base, &json!({"unit_ms": 5_000}), 10.0).unwrap();
        assert_eq!(regressions, vec!["unit_ms".to_owned()]);
        assert_eq!(deltas["unit_ms"]["pct"], json!(400.0));
        assert_eq!(deltas["unit_ms"]["regression"], json!(true));

        // Within threshold · still reported, but ✗ a failure.
        let (_, ok) = compare_metrics(&base, &json!({"unit_ms": 1_050}), 10.0).unwrap();
        assert!(ok.is_empty());
        // Faster than baseline is never a regression.
        let (_, faster) = compare_metrics(&base, &json!({"unit_ms": 10}), 10.0).unwrap();
        assert!(faster.is_empty());
    }

    #[test]
    fn nothing_in_common_is_an_error_not_a_pass() {
        // ! Zero comparisons must not read as zero regressions — that is exactly
        // how the empty-baseline gate stayed green.
        let e = compare_metrics(&json!({"unit_ms": 10}), &json!({}), 10.0).unwrap_err();
        assert!(e.contains("nothing was compared"), "{e}");
    }

    #[test]
    fn a_zero_baseline_has_no_percentage_and_any_slowdown_is_a_regression() {
        // Regression: pct was forced to 0.0 when the baseline was 0, putting every
        // possible slowdown inside the threshold.
        let (deltas, regressions) =
            compare_metrics(&json!({"unit_ms": 0}), &json!({"unit_ms": 900}), 10.0).unwrap();
        assert!(deltas["unit_ms"]["pct"].is_null(), "✗ 0.0");
        assert_eq!(regressions, vec!["unit_ms".to_owned()]);
    }
}

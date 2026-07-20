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
    match velocity_payload(&mem, &cfg.project).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

/// ! Both reads propagate. A DB error reported as `total_runs:0, pass_rate:0`
/// reads as a measurement of a broken project, ✗ a failure to measure.
/// With zero runs the rates are `null` · "nothing recorded" ✗ "measured zero".
async fn velocity_payload(mem: &pipeline_memory::Memory, project: &str) -> Result<Value, String> {
    let runs = mem
        .run_history(project, 200)
        .await
        .map_err(|e| format!("run_history: {e}"))?;
    let patterns = mem
        .failure_patterns(project)
        .await
        .map_err(|e| format!("failure_patterns: {e}"))?;
    let total_failures: i64 = patterns.iter().map(|(_, n)| *n).sum();

    // Inner loop time = median of "fast" or "static" runs.
    let mut fast_durations: Vec<i64> = runs
        .iter()
        .filter(|r| r.profile == "fast" || r.stage == "static")
        .map(|r| r.duration_ms)
        .collect();
    fast_durations.sort_unstable();

    let total_runs = runs.len();
    let pass_count = runs.iter().filter(|r| r.status == "pass").count();
    #[allow(clippy::cast_precision_loss)]
    let pass_rate = if total_runs == 0 {
        None
    } else {
        Some((pass_count as f64 / total_runs as f64) * 100.0)
    };

    Ok(json!({
        "total_runs": total_runs,
        "pass_rate_percent": pass_rate,
        "median_inner_loop_ms": median(&fast_durations),
        "total_failures": total_failures,
        "failures_by_stage": patterns.iter().map(|(s, n)| json!({"stage": s, "count": n})).collect::<Vec<_>>(),
        "targets": {
            "median_loop_target_ms": 10_000,
            "pass_rate_target_percent": 95.0,
        },
    }))
}

/// True median of an ascending slice · `None` when there is nothing to measure.
///
/// ! Even counts average the two middle elements. Indexing `len/2` alone returns
/// the upper-middle element, which overstates the inner loop on every even sample.
fn median(sorted: &[i64]) -> Option<i64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    if n % 2 == 1 {
        Some(sorted[n / 2])
    } else {
        Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2)
    }
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

    match burndown_payload(&mem, &cfg.project, milestone).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

async fn burndown_payload(
    mem: &pipeline_memory::Memory,
    project: &str,
    milestone: Option<&str>,
) -> Result<Value, String> {
    let (parsed, unparseable) = load_features(mem, project).await?;
    let (included, missing) = select_features(mem, project, milestone, &parsed).await?;

    let count = |status: &str| -> usize {
        included
            .iter()
            .filter(|f| f.get("status").and_then(Value::as_str) == Some(status))
            .count()
    };
    let total = included.len();
    let done = count("done");
    #[allow(clippy::cast_precision_loss)]
    let percent_done = if total == 0 {
        None
    } else {
        Some((done as f64 / total as f64) * 100.0)
    };
    Ok(json!({
        "milestone": milestone,
        "total": total,
        "done": done,
        "in_progress": count("in_progress"),
        "blocked": count("blocked"),
        "remaining": total.saturating_sub(done),
        "percent_done": percent_done,
        // Surfaced, ✗ silently dropped · both shrink the denominator above.
        "unparseable_features": unparseable,
        "milestone_feature_ids_not_found": missing,
    }))
}

/// Every feature blob in the project · plus a count of blobs that would not parse.
async fn load_features(
    mem: &pipeline_memory::Memory,
    project: &str,
) -> Result<(Vec<Value>, usize), String> {
    let rows = mem
        .list_scope(project, "feature")
        .await
        .map_err(|e| format!("feature list: {e}"))?;
    let mut parsed = Vec::with_capacity(rows.len());
    let mut unparseable = 0usize;
    for (_, v) in rows {
        match serde_json::from_str::<Value>(&v) {
            Ok(f) => parsed.push(f),
            Err(_) => unparseable += 1,
        }
    }
    Ok((parsed, unparseable))
}

/// Features belonging to `milestone` · all features when none is given.
///
/// ! An unknown milestone is `Err`, ✗ a silent fallback to every feature: the old
/// `feature_ids.is_empty() || …` filter made `burndown(milestone="typo")` report
/// whole-project numbers under a milestone name that does not exist.
/// ! A milestone that genuinely lists no features yields zero, ✗ everything.
async fn select_features<'a>(
    mem: &pipeline_memory::Memory,
    project: &str,
    milestone: Option<&str>,
    parsed: &'a [Value],
) -> Result<(Vec<&'a Value>, Vec<String>), String> {
    let Some(name) = milestone else {
        return Ok((parsed.iter().collect(), Vec::new()));
    };
    let blob = mem
        .recall(project, "milestone", name)
        .await
        .map_err(|e| format!("milestone lookup: {e}"))?
        .ok_or_else(|| {
            format!("unknown milestone '{name}' · ✗ whole-project numbers · omit 'milestone' to count every feature")
        })?;
    let value: Value =
        serde_json::from_str(&blob).map_err(|e| format!("corrupt milestone '{name}': {e}"))?;
    let ids: Vec<String> = value
        .get("feature_ids")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let wanted: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let included: Vec<&Value> = parsed
        .iter()
        .filter(|f| {
            f.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| wanted.contains(id))
        })
        .collect();
    let present: std::collections::HashSet<&str> = parsed
        .iter()
        .filter_map(|f| f.get("id").and_then(Value::as_str))
        .collect();
    let missing = ids
        .into_iter()
        .filter(|id| !present.contains(id.as_str()))
        .collect();
    Ok((included, missing))
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

    async fn feature(m: &Memory, key: &str, id: &str, status: &str) {
        let blob = json!({"id": id, "name": key, "status": status}).to_string();
        m.remember("p1", "feature", key, &blob).await.unwrap();
    }

    async fn run(m: &Memory, profile: &str, status: &str, ms: u128) {
        m.log_run(&NewRun {
            project_id: "p1",
            session_id: None,
            profile,
            stage: "static",
            status,
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
    async fn a_database_error_is_never_reported_as_a_measurement() {
        // Regression: both reads were unwrap_or_default()'d, so an unreadable DB
        // returned ok:true {total_runs:0, pass_rate_percent:0.0} — a fabricated
        // measurement of a project nobody could read.
        velocity_payload(&broken().await, "p1")
            .await
            .expect_err("a failed read must not produce metrics");
    }

    #[tokio::test]
    async fn no_runs_recorded_yields_null_rates_not_zero() {
        let v = velocity_payload(&fresh().await, "p1").await.unwrap();
        assert_eq!(v["total_runs"], 0);
        // ! null = "not measured" · 0.0 would read as "measured, and it is 0%".
        assert!(v["pass_rate_percent"].is_null());
        assert!(v["median_inner_loop_ms"].is_null());
    }

    #[tokio::test]
    async fn velocity_measures_what_was_recorded() {
        let m = fresh().await;
        run(&m, "fast", "pass", 100).await;
        run(&m, "fast", "fail", 300).await;
        let v = velocity_payload(&m, "p1").await.unwrap();
        assert_eq!(v["total_runs"], 2);
        assert_eq!(v["pass_rate_percent"], 50.0);
        assert_eq!(v["median_inner_loop_ms"], 200); // (100+300)/2
    }

    #[test]
    fn the_median_of_an_even_sample_is_not_the_upper_middle_element() {
        // Regression: sorted[len/2] returned the upper-middle element, overstating
        // the inner loop on every even-sized sample.
        assert_eq!(median(&[10, 20, 30, 40]), Some(25));
        assert_eq!(median(&[10, 20, 30]), Some(20));
        assert_eq!(median(&[10]), Some(10));
        assert_eq!(median(&[]), None);
    }

    #[tokio::test]
    async fn an_unknown_milestone_is_an_error_not_whole_project_numbers() {
        // Regression (worst in the file): a missing milestone left feature_ids empty
        // and the `feature_ids.is_empty() || …` filter then passed EVERY feature, so
        // burndown(milestone="typo") reported whole-project numbers under a name
        // that does not exist.
        let m = fresh().await;
        feature(&m, "f1", "f1", "done").await;
        feature(&m, "f2", "f2", "todo").await;
        let e = burndown_payload(&m, "p1", Some("typo"))
            .await
            .expect_err("unknown milestone must fail");
        assert!(e.contains("typo"), "the error names the milestone: {e}");
    }

    #[tokio::test]
    async fn a_milestone_with_no_features_counts_zero_not_everything() {
        let m = fresh().await;
        feature(&m, "f1", "f1", "done").await;
        feature(&m, "f2", "f2", "todo").await;
        m.remember("p1", "milestone", "M1", r#"{"feature_ids":[]}"#)
            .await
            .unwrap();
        let v = burndown_payload(&m, "p1", Some("M1")).await.unwrap();
        assert_eq!(v["total"], 0, "✗ the whole project");
        assert_eq!(v["done"], 0);
        assert!(v["percent_done"].is_null(), "nothing to divide by");
    }

    #[tokio::test]
    async fn a_milestone_counts_only_its_own_features() {
        let m = fresh().await;
        feature(&m, "f1", "f1", "done").await;
        feature(&m, "f2", "f2", "in_progress").await;
        feature(&m, "f3", "f3", "blocked").await;
        m.remember("p1", "milestone", "M1", r#"{"feature_ids":["f1","f2"]}"#)
            .await
            .unwrap();
        let v = burndown_payload(&m, "p1", Some("M1")).await.unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["done"], 1);
        assert_eq!(v["in_progress"], 1);
        assert_eq!(v["blocked"], 0, "f3 belongs to no milestone");
        assert_eq!(v["percent_done"], 50.0);
    }

    #[tokio::test]
    async fn a_milestone_naming_absent_features_says_so() {
        let m = fresh().await;
        feature(&m, "f1", "f1", "done").await;
        m.remember("p1", "milestone", "M1", r#"{"feature_ids":["f1","ghost"]}"#)
            .await
            .unwrap();
        let v = burndown_payload(&m, "p1", Some("M1")).await.unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["milestone_feature_ids_not_found"][0], "ghost");
    }

    #[tokio::test]
    async fn unparseable_features_are_counted_not_silently_dropped() {
        let m = fresh().await;
        feature(&m, "f1", "f1", "done").await;
        m.remember("p1", "feature", "broken", "{not json")
            .await
            .unwrap();
        let v = burndown_payload(&m, "p1", None).await.unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(
            v["unparseable_features"], 1,
            "the shrunk denominator is visible"
        );
    }

    #[tokio::test]
    async fn burndown_propagates_a_read_failure() {
        // ✗ unwrap_or_default() → an empty feature list → "0 of 0 done, all clear".
        burndown_payload(&broken().await, "p1", None)
            .await
            .expect_err("unreadable db must be an error");
    }

    #[tokio::test]
    async fn omitting_the_milestone_counts_every_feature() {
        let m = fresh().await;
        feature(&m, "f1", "f1", "done").await;
        feature(&m, "f2", "f2", "todo").await;
        let v = burndown_payload(&m, "p1", None).await.unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["remaining"], 1);
    }
}

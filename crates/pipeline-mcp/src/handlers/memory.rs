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
    match known_issues_payload(&mem, &cfg.project).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

/// ! A failed read is `Err`, ✗ an empty issue list. An unreadable memory.db must
/// never be indistinguishable from a project that has never failed.
async fn known_issues_payload(
    mem: &pipeline_memory::Memory,
    project: &str,
) -> Result<Value, String> {
    let patterns = mem
        .failure_patterns(project)
        .await
        .map_err(|e| format!("failure_patterns: {e}"))?;
    let by_stage: Vec<Value> = patterns
        .iter()
        .map(|(stage, n)| json!({"stage": stage, "count": n}))
        .collect();
    Ok(json!({"by_stage": by_stage, "total_stages": patterns.len()}))
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
    match pattern_report_payload(&mem, &cfg.project).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

/// ! Three distinct states, never collapsed:
/// unreadable DB → `Err` · no runs recorded → insufficient evidence ·
/// runs recorded with zero failures → genuinely green.
async fn pattern_report_payload(
    mem: &pipeline_memory::Memory,
    project: &str,
) -> Result<Value, String> {
    let patterns = mem
        .failure_patterns(project)
        .await
        .map_err(|e| format!("failure_patterns: {e}"))?;
    // Existence probe only · limit 1 · ✗ a second full scan.
    let has_runs = !mem
        .run_history(project, 1)
        .await
        .map_err(|e| format!("run_history: {e}"))?
        .is_empty();
    let total: i64 = patterns.iter().map(|(_, n)| *n).sum();
    let tip = match (has_runs, total) {
        // ! Zero failures with zero runs is absence of evidence, ✗ evidence of green.
        (false, _) => "no runs recorded yet · insufficient data · ✗ evidence this project is green",
        (true, 0) => "runs recorded, none failed · this project has been green",
        _ => {
            "look at the most-failing stage first · agent should add a check before that stage runs"
        }
    };
    Ok(json!({
        "total_failures": total,
        "runs_recorded": has_runs,
        "stages": patterns.iter().map(|(s, n)| json!({"stage": s, "count": n})).collect::<Vec<_>>(),
        "tip": tip,
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
        Ok(v) => ToolResponse::ok(recall_payload(scope, key, v.as_deref())),
        Err(e) => err(e.to_string()),
    }
}

/// ! `found` is the only reliable miss signal: a stored value may itself be the
/// JSON literal `null`, which `value` alone cannot distinguish from absence.
fn recall_payload(scope: &str, key: &str, value: Option<&str>) -> Value {
    json!({
        "scope": scope,
        "key": key,
        "found": value.is_some(),
        "value": value,
    })
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

    let (bundle, counts) = match collect_bundle(&mem, &cfg.project).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join(".pipeline").join(format!("export.{format}"));
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir {}: {e}", parent.display()));
        }
    }
    let body = match format {
        // ✗ fall back to "{}" — writing an empty bundle under ok:true loses the
        // whole export and reports success.
        "json" => match serde_json::to_string_pretty(&bundle) {
            Ok(s) => s,
            Err(e) => return err(format!("serialize bundle: {e}")),
        },
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
        "scopes": counts.scopes,
        "entries": counts.entries,
        "raw_values": counts.raw_values,
        "runs": counts.runs,
        "skipped": 0,
        "bytes": body.len(),
    }))
}

#[derive(Debug, Default)]
struct ExportStats {
    scopes: usize,
    entries: usize,
    /// Entries whose stored text was not JSON · preserved verbatim, ✗ dropped.
    raw_values: usize,
    runs: usize,
}

/// Bundle every scope that actually holds data.
///
/// ! Scopes come from the DB, ✗ a hardcoded list: `remember` defaults to the
/// `"default"` scope, which the old fixed list omitted, so the most common
/// memories were silently never exported.
async fn collect_bundle(
    mem: &pipeline_memory::Memory,
    project: &str,
) -> Result<(serde_json::Map<String, Value>, ExportStats), String> {
    let scopes = mem
        .scopes(project)
        .await
        .map_err(|e| format!("list scopes: {e}"))?;
    let mut bundle = serde_json::Map::new();
    let mut stats = ExportStats::default();
    for scope in &scopes {
        let pairs = mem
            .list_scope(project, scope)
            .await
            .map_err(|e| format!("scope '{scope}': {e}"))?;
        if pairs.is_empty() {
            continue;
        }
        let entries: Vec<Value> = pairs
            .into_iter()
            .map(|(k, v)| encode_entry(&k, &v, &mut stats.raw_values))
            .collect();
        stats.entries += entries.len();
        stats.scopes += 1;
        bundle.insert(scope.clone(), json!(entries));
    }
    let runs = mem
        .run_history(project, 1_000)
        .await
        .map_err(|e| format!("run history: {e}"))?;
    stats.runs = runs.len();
    bundle.insert("runs".into(), json!(runs));
    Ok((bundle, stats))
}

/// Values are opaque text. Parse when it is JSON so the export stays structured ·
/// otherwise keep the literal and tag it `encoding:"raw"` so import writes it
/// back byte-for-byte. ✗ drop it: plain-string memories are the common case.
fn encode_entry(key: &str, raw: &str, raw_count: &mut usize) -> Value {
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        json!({"key": key, "value": v})
    } else {
        *raw_count += 1;
        json!({"key": key, "value": raw, "encoding": "raw"})
    }
}

/// Inverse of `encode_entry` · returns the exact text to store.
fn decode_entry(entry: &Value) -> Option<(String, String)> {
    let key = entry.get("key").and_then(Value::as_str)?.to_owned();
    let value = entry.get("value")?;
    let text = if entry.get("encoding").and_then(Value::as_str) == Some("raw") {
        value.as_str()?.to_owned()
    } else {
        value.to_string()
    };
    Some((key, text))
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

    let Some(obj) = bundle.as_object() else {
        return err("export must be a JSON object of scope → entries".into());
    };
    let counts = import_bundle(&mem, &cfg.project, obj).await;
    let seen = counts.seen();
    let data = json!({
        "imported": counts.imported,
        "overwrote": counts.overwrote,
        "skipped": counts.skipped,
        "failed": counts.failed,
        "seen": seen,
        "from": path_str,
    });
    // ! A run where every write failed is a failure, ✗ ok:true with imported:0.
    match counts.first_error.as_ref() {
        Some(e) => err_data(
            format!("{} of {seen} writes failed · first: {e}", counts.failed),
            data,
        ),
        None => ToolResponse::ok(data),
    }
}

#[derive(Default)]
struct ImportStats {
    /// Keys that did not exist before · ✗ counted again in `overwrote`.
    imported: usize,
    /// Keys that replaced an existing value.
    overwrote: usize,
    /// Entries missing `key`/`value`, or a raw entry whose value was not a string.
    skipped: usize,
    failed: usize,
    first_error: Option<String>,
}

impl ImportStats {
    /// Every entry lands in exactly one bucket · imported+overwrote+skipped+failed.
    fn seen(&self) -> usize {
        self.imported + self.overwrote + self.skipped + self.failed
    }

    fn fail(&mut self, e: &str) {
        self.failed += 1;
        if self.first_error.is_none() {
            self.first_error = Some(e.to_owned());
        }
    }
}

async fn import_bundle(
    mem: &pipeline_memory::Memory,
    project: &str,
    obj: &serde_json::Map<String, Value>,
) -> ImportStats {
    let mut stats = ImportStats::default();
    for (scope, entries) in obj {
        if scope == "runs" {
            continue; // runs are append-only, skip on import
        }
        let Some(arr) = entries.as_array() else {
            continue;
        };
        for entry in arr {
            let Some((key, text)) = decode_entry(entry) else {
                stats.skipped += 1;
                continue;
            };
            // Probe before writing · an upsert cannot tell us afterwards whether
            // it created or replaced.
            let existed = match mem.recall(project, scope, &key).await {
                Ok(v) => v.is_some(),
                Err(e) => {
                    stats.fail(&e.to_string());
                    continue;
                }
            };
            match mem.remember(project, scope, &key, &text).await {
                Ok(()) if existed => stats.overwrote += 1,
                Ok(()) => stats.imported += 1,
                Err(e) => stats.fail(&e.to_string()),
            }
        }
    }
    stats
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
    err_data(msg, json!({}))
}

/// Failure that still carries partial counts · the agent needs to know how much
/// landed before it decides whether to retry.
fn err_data(msg: String, data: Value) -> ToolResponse {
    ToolResponse {
        ok: false,
        data,
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_memory::{Memory, NewFailure, NewRun};

    async fn fresh() -> Memory {
        let m = Memory::open_in_memory().await.expect("open");
        m.upsert_project("p1", "pipeline", "rust")
            .await
            .expect("upsert");
        m
    }

    /// Closed pool → every query errors. Stands in for an unreadable or corrupt
    /// memory.db without needing a real broken file on disk.
    async fn broken() -> Memory {
        let m = fresh().await;
        m.pool().close().await;
        m
    }

    async fn log_failure(m: &Memory, stage: &str, msg: &str) {
        let run_id = m
            .log_run(&NewRun {
                project_id: "p1",
                session_id: None,
                profile: "fast",
                stage,
                status: "fail",
                duration_ms: 10,
                triggered_by: None,
                commit_sha: None,
                stdout: None,
                stderr: None,
                failure_json: None,
            })
            .await
            .expect("log_run");
        m.log_failure(&NewFailure {
            run_id: &run_id,
            stage,
            error_message: msg,
            file: None,
            line: None,
        })
        .await
        .expect("log_failure");
    }

    #[tokio::test]
    async fn a_database_error_is_never_reported_as_a_green_project() {
        // Regression: failure_patterns().unwrap_or_default() turned an unreadable
        // memory.db into an empty vec → total 0 → "no failures yet · this project
        // has been green". The agent was told it was clean by the exact code path
        // that means "I could not read the data".
        let e = pattern_report_payload(&broken().await, "p1")
            .await
            .expect_err("a failed read must not produce a report");
        assert!(e.contains("failure_patterns"), "{e}");
    }

    #[tokio::test]
    async fn known_issues_propagates_a_read_failure() {
        // ✗ an empty issue list, which is indistinguishable from a clean project.
        known_issues_payload(&broken().await, "p1")
            .await
            .expect_err("unreadable db must be an error");
    }

    #[tokio::test]
    async fn no_runs_recorded_is_not_the_same_claim_as_green() {
        let m = fresh().await;
        let v = pattern_report_payload(&m, "p1").await.unwrap();
        assert_eq!(v["total_failures"], 0);
        assert_eq!(v["runs_recorded"], false);
        let tip = v["tip"].as_str().unwrap();
        assert!(tip.contains("insufficient data"), "{tip}");
        assert!(
            !tip.contains("has been green"),
            "absence of evidence: {tip}"
        );
    }

    #[tokio::test]
    async fn zero_failures_across_real_runs_is_reported_as_green() {
        let m = fresh().await;
        m.log_run(&NewRun {
            project_id: "p1",
            session_id: None,
            profile: "fast",
            stage: "static",
            status: "pass",
            duration_ms: 10,
            triggered_by: None,
            commit_sha: None,
            stdout: None,
            stderr: None,
            failure_json: None,
        })
        .await
        .unwrap();
        let v = pattern_report_payload(&m, "p1").await.unwrap();
        assert_eq!(v["runs_recorded"], true);
        assert!(v["tip"].as_str().unwrap().contains("green"));
    }

    #[tokio::test]
    async fn counted_failures_are_reported_per_stage() {
        let m = fresh().await;
        log_failure(&m, "unit", "assertion failed").await;
        log_failure(&m, "unit", "assertion failed again").await;
        log_failure(&m, "static", "clippy").await;
        let v = pattern_report_payload(&m, "p1").await.unwrap();
        assert_eq!(v["total_failures"], 3);
        assert_eq!(v["stages"][0]["stage"], "unit");
        assert_eq!(v["stages"][0]["count"], 2);
    }

    #[test]
    fn a_recall_miss_is_distinguishable_from_a_stored_null() {
        // Regression: a miss returned {"value": null}, byte-identical to a key whose
        // stored value is the JSON literal null.
        let miss = recall_payload("default", "k", None);
        let hit = recall_payload("default", "k", Some("null"));
        assert_eq!(miss["found"], false);
        assert_eq!(hit["found"], true);
        assert_ne!(miss["found"], hit["found"]);
    }

    #[tokio::test]
    async fn export_covers_the_default_scope_it_used_to_omit() {
        // Regression: the scope list was hardcoded to 11 names and omitted "default",
        // the scope `remember` writes to when the caller gives none — so the most
        // common memories were silently never exported.
        let m = fresh().await;
        m.remember("p1", "default", "k", "v").await.unwrap();
        m.remember("p1", "invented_by_a_caller", "k2", "v2")
            .await
            .unwrap();
        let (bundle, stats) = collect_bundle(&m, "p1").await.unwrap();
        assert!(bundle.contains_key("default"), "{bundle:?}");
        assert!(bundle.contains_key("invented_by_a_caller"));
        assert_eq!(stats.scopes, 2, "reports scopes with data, ✗ a constant");
        assert_eq!(stats.entries, 2);
    }

    #[tokio::test]
    async fn export_preserves_values_that_are_not_json() {
        // Regression: from_str().ok() dropped every non-JSON value with no count and
        // no warning — plain-string memories vanished under ok:true.
        let m = fresh().await;
        m.remember("p1", "default", "plain", "not json at all")
            .await
            .unwrap();
        m.remember("p1", "default", "structured", r#"{"a":1}"#)
            .await
            .unwrap();
        let (bundle, stats) = collect_bundle(&m, "p1").await.unwrap();
        assert_eq!(stats.entries, 2, "nothing dropped");
        assert_eq!(stats.raw_values, 1, "the non-JSON one is counted, ✗ silent");
        let entries = bundle["default"].as_array().unwrap();
        let plain = entries
            .iter()
            .find(|e| e["key"] == "plain")
            .expect("preserved");
        assert_eq!(plain["value"], "not json at all");
        assert_eq!(plain["encoding"], "raw");
    }

    #[tokio::test]
    async fn export_propagates_a_read_failure() {
        collect_bundle(&broken().await, "p1")
            .await
            .expect_err("unreadable db must not yield an empty bundle under ok:true");
    }

    #[tokio::test]
    async fn a_non_json_value_survives_an_export_import_round_trip() {
        let m = fresh().await;
        m.remember("p1", "default", "plain", "not json at all")
            .await
            .unwrap();
        let (bundle, _) = collect_bundle(&m, "p1").await.unwrap();
        m.forget("p1", "default", "plain").await.unwrap();

        let stats = import_bundle(&m, "p1", &bundle).await;
        assert_eq!(stats.imported, 1);
        // ! Byte-for-byte · ✗ re-quoted as "\"not json at all\"".
        assert_eq!(
            m.recall("p1", "default", "plain").await.unwrap().as_deref(),
            Some("not json at all")
        );
    }

    #[tokio::test]
    async fn import_counts_each_entry_exactly_once() {
        // Regression: an overwritten key incremented both `overwrite` and `imported`,
        // so imported + overwrote exceeded the number of entries supplied.
        let m = fresh().await;
        m.remember("p1", "default", "existing", "\"old\"")
            .await
            .unwrap();
        let bundle: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "default": [
                {"key": "existing", "value": "new"},
                {"key": "brand_new", "value": "x"},
                {"no_key": true},
            ]
        }))
        .unwrap();
        let stats = import_bundle(&m, "p1", &bundle).await;
        assert_eq!(stats.imported, 1);
        assert_eq!(stats.overwrote, 1);
        assert_eq!(
            stats.skipped, 1,
            "the malformed entry is counted, ✗ dropped"
        );
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.seen(), 3, "every entry lands in exactly one bucket");
    }

    #[tokio::test]
    async fn an_import_where_every_write_failed_is_not_a_success() {
        // Regression: remember() errors were dropped, so a totally failed run still
        // returned ok:true, imported:0 — "nothing to do" instead of "nothing worked".
        let m = broken().await;
        let bundle: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "default": [{"key": "k", "value": "v"}]
        }))
        .unwrap();
        let stats = import_bundle(&m, "p1", &bundle).await;
        assert_eq!(stats.imported, 0);
        assert_eq!(stats.failed, 1);
        assert!(stats.first_error.is_some(), "the write error is surfaced");
    }

    #[tokio::test]
    async fn import_skips_runs_because_they_are_append_only() {
        let m = fresh().await;
        let bundle: serde_json::Map<String, Value> = serde_json::from_value(json!({
            "runs": [{"key": "r1", "value": "x"}],
            "default": [{"key": "k", "value": "v"}],
        }))
        .unwrap();
        let stats = import_bundle(&m, "p1", &bundle).await;
        assert_eq!(stats.seen(), 1, "runs are not entries");
    }
}

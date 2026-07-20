//! `pipeline_run` handler · stage execution · preflight · git commit/push ·
//! status · fix_suggestion.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use pipeline_core::{StageContext, StageProfile, StageStatus};
use pipeline_memory::NewRun;
use pipeline_stages::Runner;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "stage" => stage(req.args, state).await,
        "preflight" => preflight(state).await,
        "fmt" => fmt(&req.args).await,
        "commit" => commit(&req.args, state).await,
        "push" => push(&req.args, state).await,
        "status" => status(state).await,
        "fix_suggestion" => fix_suggestion(&req.args, state).await,
        "explain" => explain(&req.args),
        "logs" => logs(&req.args, state).await,
        other => err(format!("unknown action 'pipeline_run.{other}'")),
    }
}

async fn preflight(state: Arc<ServerState>) -> ToolResponse {
    // Preflight = full pipeline + security in clean Docker. Reuses stage(profile=preflight).
    let args = json!({"profile": "preflight"});
    stage(args, state).await
}

/// Apply the fixes the toolchain can make on its own.
///
/// ! Exists because the gate reported problems and offered no way to fix them
/// *through the tool*. Every agent therefore shelled out to `cargo fmt`, which
/// defeats Pipeline's premise that an agent never needs to know the toolchain.
/// `stage` tells you what is wrong; this is the paired verb that fixes it.
///
/// Formatting only. ✗ `clippy --fix`: it rewrites logic, and an agent applying
/// unreviewed semantic edits to get a green gate is how a gate stops meaning
/// anything. Clippy findings stay the agent's to reason about.
async fn fmt(args: &Value) -> ToolResponse {
    let check = args.get("check").and_then(as_bool).unwrap_or(false);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mut flags = vec!["fmt", "--all"];
    if check {
        flags.push("--check");
    }
    let out = match Command::new("cargo")
        .args(&flags)
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("cargo fmt: {e} · is the toolchain installed?")),
    };
    let ok = out.status.success();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    ToolResponse {
        ok,
        data: json!({
            "mode": if check { "check" } else { "write" },
            "changed": !check && ok,
            "diff": if check { stdout.clone() } else { String::new() },
            "stderr": String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        next_suggested: if ok && !check {
            vec!["pipeline_run.stage(profile=fast)".into()]
        } else {
            vec![]
        },
        memory_refs: vec![],
        error: if ok {
            None
        } else if check {
            Some("formatting differs · rerun without check to apply".into())
        } else {
            Some(format!(
                "cargo fmt exit {} · {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        },
    }
}

/// Refuse a git write while the last recorded run is red.
///
/// ! CLAUDE.md states "all green → push allowed", and until now that rule was
/// enforced by nothing: `commit` and `push` were honest git wrappers that never
/// consulted run state. A gate nobody consults is decoration.
///
/// Returns `Some(refusal)` when the caller should be stopped. `force` is an
/// explicit, recorded override — the point is that bypassing is *deliberate*,
/// ✗ that it is impossible.
async fn gate_refusal(
    action: &str,
    args: &Value,
    state: &Arc<ServerState>,
) -> Option<ToolResponse> {
    if args.get("force").and_then(as_bool).unwrap_or(false) {
        return None;
    }
    let cfg = load_config_in_cwd().ok()?;
    let mem = ensure_memory(state).await.ok()?;
    let runs = mem.run_history(&cfg.project, 20).await.ok()?;
    // Newest run per stage · an old failure already superseded by a pass must
    // not block forever.
    let mut newest: std::collections::HashMap<String, &pipeline_memory::RunRecord> =
        std::collections::HashMap::new();
    for r in &runs {
        newest.entry(r.stage.clone()).or_insert(r);
    }
    let red: Vec<&str> = newest
        .values()
        .filter(|r| r.status == "fail" || r.status == "error")
        .map(|r| r.stage.as_str())
        .collect();
    // Empty history is allowed through deliberately: never-having-run is not a
    // red gate, and a tool that cannot make a project's first commit is
    // unusable. The gate blocks *known* failure, ✗ absence of evidence.
    if red.is_empty() {
        return None;
    }
    let mut stages: Vec<&str> = red;
    stages.sort_unstable();
    Some(ToolResponse {
        ok: false,
        data: json!({
            "gate": "red",
            "failing_stages": stages,
            "override": "pass force=true to proceed anyway",
        }),
        next_suggested: vec![
            "pipeline_run.stage(profile=fast)".into(),
            "pipeline_run.fix_suggestion".into(),
        ],
        memory_refs: vec![],
        error: Some(format!(
            "pipeline_run.{action} refused · last run failed on: {} · fix and rerun the gate, or pass force=true",
            stages.join(" · ")
        )),
    })
}

fn as_bool(v: &Value) -> Option<bool> {
    v.as_bool().or_else(|| v.as_str()?.parse().ok())
}

async fn commit(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let message = match args.get("message").and_then(Value::as_str) {
        Some(m) => m.to_owned(),
        None => return err("missing 'message'".into()),
    };
    if let Some(refusal) = gate_refusal("commit", args, &state).await {
        return refusal;
    }
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // git add -A · stages everything tracked or untracked.
    let add = match Command::new("git")
        .args(["add", "-A"])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git add: {e}")),
    };
    if !add.status.success() {
        return err(format!(
            "git add failed: {}",
            String::from_utf8_lossy(&add.stderr).trim()
        ));
    }
    let cm = match Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git commit: {e}")),
    };
    let ok = cm.status.success();
    let stdout = String::from_utf8_lossy(&cm.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&cm.stderr).into_owned();

    let sha = if ok {
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&cwd)
            .output()
            .await
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_owned())
                } else {
                    None
                }
            })
    } else {
        None
    };
    ToolResponse {
        ok,
        data: json!({
            "message": message,
            "sha": sha,
            "stdout": stdout,
            "stderr": stderr,
        }),
        next_suggested: if ok {
            vec!["pipeline_run.push".into()]
        } else {
            vec![]
        },
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "git commit exit {}",
                cm.status.code().unwrap_or(-1)
            ))
        },
    }
}

async fn push(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    if let Some(refusal) = gate_refusal("push", args, &state).await {
        return refusal;
    }
    let remote = args
        .get("remote")
        .and_then(Value::as_str)
        .unwrap_or("origin");
    let branch_arg = args.get("branch").and_then(Value::as_str);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let branch = match branch_arg.map(str::to_owned) {
        Some(b) => b,
        None => match Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&cwd)
            .output()
            .await
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
            _ => return err("could not determine current branch".into()),
        },
    };
    let output = match Command::new("git")
        .args(["push", "-u", remote, &branch])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git push: {e}")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "remote": remote,
            "branch": branch,
            "stdout": String::from_utf8_lossy(&output.stdout).into_owned(),
            "stderr": String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "git push exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

async fn status(state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    let pack = match mem.handover(&cfg.project).await {
        Ok(p) => p,
        Err(e) => return err(e.to_string()),
    };
    let recent = mem.run_history(&cfg.project, 5).await.unwrap_or_default();
    let last_status = pack
        .last_run
        .as_ref()
        .map_or("unknown", |r| r.status.as_str());
    ToolResponse::ok(json!({
        "project": pack.project,
        "active_session": pack.active_session,
        "last_status": last_status,
        "recent_runs": recent.iter().map(|r| json!({
            "stage": r.stage, "status": r.status, "duration_ms": r.duration_ms,
            "created_at": r.created_at,
        })).collect::<Vec<_>>(),
    }))
}

async fn logs(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let stage_filter = args.get("stage").and_then(Value::as_str);
    let tail = args.get("tail").and_then(Value::as_i64).unwrap_or(20);
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    let runs = mem
        .run_history(&cfg.project, tail)
        .await
        .unwrap_or_default();
    let filtered: Vec<&pipeline_memory::RunRecord> = runs
        .iter()
        .filter(|r| stage_filter.is_none_or(|s| r.stage == s))
        .collect();
    ToolResponse::ok(json!({
        "stage": stage_filter,
        "tail": tail,
        "count": filtered.len(),
        "runs": filtered.iter().map(|r| json!({
            "id": r.id,
            "stage": r.stage,
            "status": r.status,
            "duration_ms": r.duration_ms,
            "created_at": r.created_at,
            "stdout": r.stdout,
            "stderr": r.stderr,
        })).collect::<Vec<_>>(),
    }))
}

async fn fix_suggestion(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let stage_filter = args.get("stage").and_then(Value::as_str);
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };

    // Pull the most recent failure (optionally filtered by stage) from
    // the latest run, then look for similar past failures with a fix that worked.
    let runs = mem.run_history(&cfg.project, 5).await.unwrap_or_default();
    let target_run = runs
        .iter()
        .find(|r| r.status == "fail" && stage_filter.is_none_or(|s| r.stage == s));
    let Some(run) = target_run else {
        return ToolResponse::ok(json!({
            "message": "no recent failure to suggest a fix for",
            "stage_filter": stage_filter,
        }));
    };

    // Use the run's stderr as the error signal · best signal we have.
    let signal = run.stderr.as_deref().unwrap_or("");
    let similar = mem
        .find_similar_failures(&cfg.project, signal, 5)
        .await
        .unwrap_or_default();
    let fixes_that_worked: Vec<&pipeline_memory::FailureRecord> = similar
        .iter()
        .filter(|f| f.fix_worked == Some(1) && f.fix_applied.is_some())
        .collect();
    ToolResponse::ok(json!({
        "stage": run.stage,
        "run_id": run.id,
        "similar_failures": similar.len(),
        "prior_fixes": fixes_that_worked.iter().map(|f| json!({
            "fix": f.fix_applied,
            "ts": f.created_at,
        })).collect::<Vec<_>>(),
        "tip": if fixes_that_worked.is_empty() {
            "no prior successful fix · agent should diagnose from stderr"
        } else {
            "applying the most recent prior fix is a strong starting point"
        },
    }))
}

async fn stage(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let profile_arg = args
        .get("profile")
        .or_else(|| args.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("fast");
    let Some(profile) = StageProfile::parse(profile_arg) else {
        return err(format!("unknown profile '{profile_arg}'"));
    };

    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let project_id = cfg.project.clone();
    let stack = cfg.stack.runtime.clone();

    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    if let Err(e) = mem.upsert_project(&project_id, &project_id, &stack).await {
        return err(e.to_string());
    }

    // Session id is resolved purely from the lock table · state.project_id only
    // tells us whether *some* session is active for this MCP connection.
    let session_id = mem
        .current_lock(&project_id)
        .await
        .ok()
        .flatten()
        .map(|l| l.session_id);

    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let ctx = StageContext {
        project_root,
        config: cfg,
    };
    let summary = Runner::run_profile(profile, &ctx).await;

    let mut run_ids: Vec<String> = Vec::new();
    for r in &summary.results {
        let failure_json = r
            .failure
            .as_ref()
            .and_then(|f| serde_json::to_string(f).ok());
        let stored = mem
            .log_run(&NewRun {
                project_id: &project_id,
                session_id: session_id.as_deref(),
                profile: profile_arg,
                stage: r.stage.as_str(),
                status: status_label(r.status),
                duration_ms: r.duration.as_millis(),
                triggered_by: Some("mcp"),
                commit_sha: None,
                stdout: Some(&r.stdout),
                stderr: Some(&r.stderr),
                failure_json: failure_json.as_deref(),
            })
            .await;
        if let Ok(id) = stored {
            run_ids.push(id);
        }
    }

    let next = if matches!(summary.overall, StageStatus::Pass) {
        vec![
            "pipeline_run.stage(profile=full)".into(),
            "pipeline_session.checkpoint".into(),
        ]
    } else {
        vec![
            "pipeline_memory.suggest_fix".into(),
            "pipeline_run.fix_suggestion".into(),
        ]
    };

    let passed = matches!(summary.overall, StageStatus::Pass);
    ToolResponse {
        ok: passed,
        data: serde_json::to_value(&summary).unwrap_or(json!({})),
        next_suggested: next,
        memory_refs: run_ids.into_iter().map(|id| format!("run:{id}")).collect(),
        // ! `error` was unconditionally None, so every failing run returned
        // ok:false with error:null — the one envelope field an agent reads
        // first on failure said nothing, and the detail sat buried in `data`.
        error: if passed {
            None
        } else {
            Some(failure_summary(&summary))
        },
    }
}

/// One line naming what failed · enough to route the next action without
/// parsing the full summary.
fn failure_summary(summary: &pipeline_stages::RunnerSummary) -> String {
    use std::fmt::Write as _;
    let failed: Vec<&str> = summary
        .results
        .iter()
        .filter(|r| matches!(r.status, StageStatus::Fail | StageStatus::Error))
        .map(|r| r.stage.as_str())
        .collect();
    let mut s = if failed.is_empty() {
        format!("{} profile did not pass", summary.profile)
    } else {
        format!("{} failed: {}", summary.profile, failed.join(" · "))
    };
    if !summary.skipped.is_empty() {
        let names: Vec<&str> = summary.skipped.iter().map(|s| s.stage.as_str()).collect();
        let _ = write!(s, " · did not run: {}", names.join(" · "));
    }
    s
}

fn explain(args: &Value) -> ToolResponse {
    let stage = args.get("stage").and_then(Value::as_str).unwrap_or("");
    let text = match stage {
        "static" => {
            "Static stage runs `cargo fmt --check` + `cargo clippy -D warnings`. \
             No code execution. Fast (sub-second on small workspaces)."
        }
        "unit" => {
            "Unit stage runs `cargo test --workspace --no-fail-fast`. \
             No containers required. Coverage gate via pipeline_test.coverage."
        }
        "container" => {
            "Container stage builds the multi-stage Dockerfile · Trivy vulnerability scan · image size gate. \
             Skips without a Dockerfile or a reachable daemon — a skip fails preflight."
        }
        "integration" => {
            "Integration stage runs docker-compose up · health checks · API contract tests · compose down. \
             Skips without a compose file — a skip fails preflight."
        }
        "security" => {
            "Security stage runs a trufflehog secret scan + dependency audit. \
             ! A scanner that cannot start is UNKNOWN and fails the stage — ✗ treated as clean."
        }
        _ => "Unknown stage. Valid: static · unit · container · integration · security.",
    };
    ToolResponse::ok(json!({"stage": stage, "explanation": text}))
}

const fn status_label(s: StageStatus) -> &'static str {
    match s {
        StageStatus::Pass => "pass",
        StageStatus::Fail => "fail",
        StageStatus::Skipped => "skipped",
        StageStatus::Error => "error",
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

#[cfg(test)]
mod gate_tests {
    use super::*;
    use pipeline_core::{StageKind, StageResult};
    use pipeline_stages::{RunnerSummary, SkippedStage};

    fn result(stage: StageKind, status: StageStatus) -> StageResult {
        StageResult {
            stage,
            status,
            duration: std::time::Duration::ZERO,
            stdout: String::new(),
            stderr: String::new(),
            failure: None,
        }
    }

    fn summary(results: Vec<StageResult>, skipped: Vec<SkippedStage>) -> RunnerSummary {
        RunnerSummary {
            profile: "fast".into(),
            overall: StageStatus::Fail,
            results,
            skipped,
            total_duration_ms: 0,
            strict: false,
            stages_planned: 2,
            stages_executed: 1,
            gate_note: String::new(),
        }
    }

    #[test]
    fn a_failing_run_names_the_stage_in_the_error_envelope() {
        // ! `error` was unconditionally None on failure · the field an agent
        // reads first said nothing while the detail hid inside `data`.
        let s = summary(
            vec![
                result(StageKind::Static, StageStatus::Pass),
                result(StageKind::Unit, StageStatus::Fail),
            ],
            vec![],
        );
        let msg = failure_summary(&s);
        assert!(msg.contains("unit"), "{msg}");
        assert!(!msg.contains("static"), "passing stages are noise: {msg}");
    }

    #[test]
    fn a_stage_that_never_ran_is_named_separately_from_one_that_failed() {
        // "did not run" and "ran and failed" route to different next actions ·
        // collapsing them sends the agent debugging code that was never tested.
        let s = summary(
            vec![result(StageKind::Unit, StageStatus::Fail)],
            vec![SkippedStage {
                stage: StageKind::Container,
                reason: "no Dockerfile".into(),
            }],
        );
        let msg = failure_summary(&s);
        assert!(msg.contains("failed: unit"), "{msg}");
        assert!(msg.contains("did not run: container"), "{msg}");
    }

    #[test]
    fn an_error_status_counts_as_a_failure_not_a_skip() {
        let s = summary(vec![result(StageKind::Static, StageStatus::Error)], vec![]);
        assert!(failure_summary(&s).contains("static"));
    }

    #[test]
    fn a_quoted_boolean_still_disarms_the_gate() {
        // ! Agents routinely send "true" · if force silently failed to parse,
        // the override would look broken and invite worse workarounds.
        assert_eq!(as_bool(&json!(true)), Some(true));
        assert_eq!(as_bool(&json!("true")), Some(true));
        assert_eq!(as_bool(&json!("false")), Some(false));
        assert_eq!(as_bool(&json!("yes")), None);
        assert_eq!(as_bool(&json!(1)), None);
    }

    #[tokio::test]
    async fn force_skips_the_gate_lookup_entirely() {
        // force must not depend on a readable project or memory · it is the
        // escape hatch for exactly the situation where those are broken.
        let state = Arc::new(ServerState::new());
        let args = json!({"message": "wip", "force": true});
        assert!(gate_refusal("commit", &args, &state).await.is_none());
    }

    #[tokio::test]
    async fn a_project_with_no_history_is_not_blocked() {
        // ✗ blocking a first commit · never-ran is not a red gate, and a tool
        // that cannot make the initial commit is unusable on a fresh project.
        let state = Arc::new(ServerState::new());
        let args = json!({"message": "initial"});
        assert!(gate_refusal("commit", &args, &state).await.is_none());
    }
}

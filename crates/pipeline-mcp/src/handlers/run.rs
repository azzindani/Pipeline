//! `pipeline_run` handler · stage execution · preflight · git commit/push ·
//! status · fix_suggestion.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
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
        "commit" => commit(&req.args).await,
        "push" => push(&req.args).await,
        "status" => status(state).await,
        "fix_suggestion" => fix_suggestion(&req.args, state).await,
        "explain" => explain(&req.args),
        "logs" => ToolResponse::not_implemented(ToolName::Run, &req.action),
        other => err(format!("unknown action 'pipeline_run.{other}'")),
    }
}

async fn preflight(state: Arc<ServerState>) -> ToolResponse {
    // Preflight = full pipeline + security in clean Docker. Reuses stage(profile=preflight).
    let args = json!({"profile": "preflight"});
    stage(args, state).await
}

async fn commit(args: &Value) -> ToolResponse {
    let message = match args.get("message").and_then(Value::as_str) {
        Some(m) => m.to_owned(),
        None => return err("missing 'message'".into()),
    };
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

async fn push(args: &Value) -> ToolResponse {
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

    ToolResponse {
        ok: matches!(summary.overall, StageStatus::Pass),
        data: serde_json::to_value(&summary).unwrap_or(json!({})),
        next_suggested: next,
        memory_refs: run_ids.into_iter().map(|id| format!("run:{id}")).collect(),
        error: None,
    }
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
             No containers required. Coverage gate lands at MVP."
        }
        "container" => {
            "Container stage builds the multi-stage Dockerfile · Trivy vulnerability scan · image size gate. Lands POC week 2."
        }
        "integration" => {
            "Integration stage runs docker-compose up · health checks · API contract tests · compose down. Lands POC week 2."
        }
        "security" => {
            "Security stage runs trufflehog secret scan + dependency audit + threat model gates. Lands MVP week 6."
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

//! `pipeline_run` handler · stage execution + commit/push (commit/push wire MVP).

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use pipeline_core::{StageContext, StageProfile, StageStatus};
use pipeline_memory::NewRun;
use pipeline_stages::Runner;
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "stage" => stage(req.args, state).await,
        "explain" => explain(&req.args),
        "preflight" | "commit" | "push" | "status" | "logs" | "fix_suggestion" => {
            ToolResponse::not_implemented(ToolName::Run, &req.action)
        }
        other => err(format!("unknown action 'pipeline_run.{other}'")),
    }
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

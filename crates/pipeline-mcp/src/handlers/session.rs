//! `pipeline_session` handler · lock · start · end · handover · context.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "lock" => lock(req.args, state).await,
        "unlock" => unlock(state).await,
        "steal" => steal(req.args, state).await,
        "end" => end(req.args, state).await,
        "handover" => handover(state).await,
        "start" => start(req.args, state).await,
        "checkpoint" => checkpoint(req.args, state).await,
        "agent_register" => agent_register(req.args, state).await,
        "context" | "file_context" | "task_context" => {
            ToolResponse::not_implemented(ToolName::Session, &req.action)
        }
        other => unknown(other),
    }
}

/// Start a session WITHOUT taking the exclusive lock · returns an ephemeral
/// session id keyed against the current project. Agents that want strict
/// concurrency should use `lock` instead.
async fn start(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    if let Err(e) = mem
        .upsert_project(&cfg.project, &cfg.project, &cfg.stack.runtime)
        .await
    {
        return err(e.to_string());
    }
    // Prefer per-call agent_id; fall back to whatever was registered earlier
    // in this MCP connection; finally fall back to "anonymous".
    let agent_id = match args.get("agent_id").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => state
            .agent_id
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| "anonymous".into()),
    };
    let goal = args.get("goal").and_then(Value::as_str);

    let started_at = pipeline_memory::now_rfc3339();
    *state.project_id.lock().await = Some(cfg.project.clone());
    ToolResponse::ok(json!({
        "project": cfg.project,
        "agent_id": agent_id,
        "goal": goal,
        "started_at": started_at,
        "lock_held": false,
    }))
}

/// Persist a free-form note keyed by an auto-generated checkpoint id.
async fn checkpoint(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let note = args.get("note").and_then(Value::as_str).unwrap_or("");
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let blob = json!({
        "id": id,
        "note": note,
        "ts": pipeline_memory::now_rfc3339(),
        "agent_id": state.agent_id.lock().await.clone(),
    });
    if let Err(e) = mem
        .remember(&cfg.project, "checkpoint", &id, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse {
        ok: true,
        data: blob,
        next_suggested: vec!["pipeline_session.handover".into()],
        memory_refs: vec![format!("checkpoint:{id}")],
        error: None,
    }
}

/// Register agent identity + capabilities for this MCP connection.
/// Subsequent session ops inherit `agent_id`.
async fn agent_register(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let agent_id = match args.get("agent_id").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'agent_id'".into()),
    };
    let caps: Vec<String> = args
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    *state.agent_id.lock().await = Some(agent_id.clone());
    *state.agent_capabilities.lock().await = caps.clone();
    ToolResponse::ok(json!({
        "agent_id": agent_id,
        "capabilities": caps,
        "registered_at": pipeline_memory::now_rfc3339(),
    }))
}

async fn lock(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    if let Err(e) = mem
        .upsert_project(&cfg.project, &cfg.project, &cfg.stack.runtime)
        .await
    {
        return err(e.to_string());
    }
    let agent_id = args.get("agent_id").and_then(Value::as_str);
    let goal = args.get("goal").and_then(Value::as_str);
    let lock = match mem.lock_session(&cfg.project, agent_id, goal).await {
        Ok(l) => l,
        Err(e) => return err(e.to_string()),
    };
    *state.project_id.lock().await = Some(cfg.project.clone());
    ToolResponse {
        ok: true,
        data: json!(lock),
        next_suggested: vec![
            "pipeline_session.handover".into(),
            "pipeline_run.stage(fast)".into(),
        ],
        memory_refs: vec![format!("session:{}", lock.session_id)],
        error: None,
    }
}

async fn unlock(state: Arc<ServerState>) -> ToolResponse {
    let project_id = match state.project_id.lock().await.clone() {
        Some(p) => p,
        None => return err("no active session".into()),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let lock = match mem.current_lock(&project_id).await {
        Ok(Some(l)) => l,
        Ok(None) => return err("no lock to release".into()),
        Err(e) => return err(e.to_string()),
    };
    if let Err(e) = mem.end_session(&lock.session_id, "unlocked", None).await {
        return err(e.to_string());
    }
    *state.project_id.lock().await = None;
    ToolResponse::ok(json!({"released": lock.session_id}))
}

async fn steal(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let project = match args.get("project_id").and_then(Value::as_str) {
        Some(p) => p.to_owned(),
        None => match load_config_in_cwd() {
            Ok(c) => c.project,
            Err(e) => return err(format!("config: {e}")),
        },
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    if let Err(e) = mem.force_unlock(&project).await {
        return err(e.to_string());
    }
    *state.project_id.lock().await = None;
    ToolResponse::ok(json!({"stolen": project}))
}

async fn end(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let session_id = match args.get("session_id").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing session_id".into()),
    };
    let outcome = args.get("outcome").and_then(Value::as_str).unwrap_or("ok");
    let summary = args.get("summary").and_then(Value::as_str);
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    if let Err(e) = mem.end_session(&session_id, outcome, summary).await {
        return err(e.to_string());
    }
    *state.project_id.lock().await = None;
    ToolResponse::ok(json!({"ended": session_id, "outcome": outcome}))
}

async fn handover(state: Arc<ServerState>) -> ToolResponse {
    let project_id = match state.project_id.lock().await.clone() {
        Some(p) => p,
        None => match load_config_in_cwd() {
            Ok(c) => c.project,
            Err(e) => return err(format!("config: {e}")),
        },
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    match mem.handover(&project_id).await {
        Ok(pack) => ToolResponse::ok(serde_json::to_value(pack).unwrap_or(json!({}))),
        Err(e) => err(e.to_string()),
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

fn unknown(action: &str) -> ToolResponse {
    err(format!("unknown action 'pipeline_session.{action}'"))
}

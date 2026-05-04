//! `pipeline_simulate` handler · personas · journeys · use cases.
//!
//! Day-6 wires: persona_create, journey_define, use_case_define.
//! journey_simulate, load, chaos_inject return not_implemented (need
//! containerized load harness · MVP+).

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "persona_create" => persona_create(&req.args, state).await,
        "journey_define" => journey_define(&req.args, state).await,
        "use_case_define" => use_case_define(&req.args, state).await,
        "journey_simulate" | "load" | "chaos_inject" => {
            ToolResponse::not_implemented(ToolName::Simulate, &req.action)
        }
        other => err(format!("unknown action 'pipeline_simulate.{other}'")),
    }
}

async fn persona_create(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let role = match args.get("role").and_then(Value::as_str) {
        Some(r) => r.to_owned(),
        None => return err("missing 'role'".into()),
    };
    let goals = args.get("goals").cloned().unwrap_or(json!([]));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let blob =
        json!({"id": id, "role": role, "goals": goals, "ts": pipeline_memory::now_rfc3339()});
    if let Err(e) = mem
        .remember(&project, "persona", &id, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse {
        ok: true,
        data: blob,
        next_suggested: vec!["pipeline_simulate.journey_define".into()],
        memory_refs: vec![format!("persona:{id}")],
        error: None,
    }
}

async fn journey_define(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let persona = args
        .get("persona")
        .and_then(Value::as_str)
        .unwrap_or("anonymous")
        .to_owned();
    let steps = args.get("steps").cloned().unwrap_or(json!([]));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let blob =
        json!({"id": id, "persona": persona, "steps": steps, "ts": pipeline_memory::now_rfc3339()});
    if let Err(e) = mem
        .remember(&project, "journey", &id, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(blob)
}

async fn use_case_define(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let actor = match args.get("actor").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'actor'".into()),
    };
    let intent = args.get("intent").and_then(Value::as_str).unwrap_or("");
    let flow = args.get("flow").cloned().unwrap_or(json!([]));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let blob = json!({
        "id": id, "actor": actor, "intent": intent, "flow": flow,
        "ts": pipeline_memory::now_rfc3339()
    });
    if let Err(e) = mem
        .remember(&project, "use_case", &id, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(blob)
}

async fn cfg_project(state: &Arc<ServerState>) -> Result<String, String> {
    if let Some(p) = state.project_id.lock().await.clone() {
        return Ok(p);
    }
    load_config_in_cwd().map(|c| c.project)
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

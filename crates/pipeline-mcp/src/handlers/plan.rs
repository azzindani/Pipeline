//! `pipeline_plan` handler · idea intake · PRD · features · milestones · ADRs · risks.
//!
//! Storage strategy (Day-3): every artifact is a JSON blob in `memory_kv`,
//! keyed by scope:
//!
//!   scope="plan"      key="idea"       → captured idea text + timestamp
//!   scope="plan"      key="prd"        → PRD document (goals · users · features_ref)
//!   scope="plan"      key="type"       → project type string
//!   scope="feature"   key=<id>         → Feature record
//!   scope="milestone" key=<name>       → Milestone record
//!   scope="decision"  key=<id>         → ADR record
//!   scope="risk"      key=<id>         → Risk record
//!
//! `list_scope(project, scope)` enumerates each kind. A schema migration
//! to dedicated tables can land later without breaking callers — they
//! see structured `data` either way.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "idea_capture" => idea_capture(req.args, state).await,
        "create" => create_plan(req.args, state).await,
        "prd_write" => prd_write(req.args, state).await,
        "prd_read" => prd_read(state).await,
        "prd_update" => prd_update(req.args, state).await,
        "features_add" => features_add(req.args, state).await,
        "features_list" => features_list(state).await,
        "features_update" => features_update(req.args, state).await,
        "features_track" => features_track(req.args, state).await,
        "acceptance_define" => acceptance_define(req.args, state).await,
        "milestone_create" => milestone_create(req.args, state).await,
        "milestone_progress" => milestone_progress(req.args, state).await,
        "progress" => progress(state).await,
        "decision_log" => decision_log(req.args, state).await,
        "risk_add" => risk_add(req.args, state).await,
        "risk_list" => risk_list(state).await,
        // Defer to MVP: needs HTTP/scraping + scoring.
        "link_ingest"
        | "feasibility"
        | "research_notes_list"
        | "research_notes_show"
        | "estimate" => ToolResponse::not_implemented(ToolName::Plan, &req.action),
        other => err(format!("unknown action 'pipeline_plan.{other}'")),
    }
}

// ---------- ideas + plan creation ----------

async fn idea_capture(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let text = match args.get("text").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'text'".into()),
    };
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let blob = json!({"text": text, "captured_at": pipeline_memory::now_rfc3339()});
    if let Err(e) = mem
        .remember(&project, "plan", "idea", &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse {
        ok: true,
        data: blob,
        next_suggested: vec![
            "pipeline_plan.create".into(),
            "pipeline_plan.feasibility".into(),
        ],
        memory_refs: vec!["plan:idea".into()],
        error: None,
    }
}

async fn create_plan(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let project_type = args.get("type").and_then(Value::as_str).unwrap_or("custom");
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    if let Err(e) = mem.remember(&project, "plan", "type", project_type).await {
        return err(e.to_string());
    }
    // Seed an empty PRD if one doesn't exist yet.
    let existing = mem.recall(&project, "plan", "prd").await.ok().flatten();
    if existing.is_none() {
        let prd = json!({
            "goals": [],
            "non_goals": [],
            "users": [],
            "summary": "",
            "created_at": pipeline_memory::now_rfc3339(),
        });
        let _ = mem
            .remember(&project, "plan", "prd", &prd.to_string())
            .await;
    }
    ToolResponse {
        ok: true,
        data: json!({"type": project_type, "prd_initialized": existing.is_none()}),
        next_suggested: vec![
            "pipeline_plan.prd_write".into(),
            "pipeline_plan.features_add".into(),
            "pipeline_plan.milestone_create".into(),
        ],
        memory_refs: vec!["plan:type".into()],
        error: None,
    }
}

// ---------- PRD ----------

async fn prd_write(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let mut prd = json!({
        "goals": args.get("goals").cloned().unwrap_or(json!([])),
        "non_goals": args.get("non_goals").cloned().unwrap_or(json!([])),
        "users": args.get("users").cloned().unwrap_or(json!([])),
        "summary": args.get("summary").cloned().unwrap_or(json!("")),
    });
    prd["created_at"] = json!(pipeline_memory::now_rfc3339());
    if let Err(e) = mem
        .remember(&project, "plan", "prd", &prd.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(prd)
}

async fn prd_read(state: Arc<ServerState>) -> ToolResponse {
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    match mem.recall(&project, "plan", "prd").await {
        Ok(Some(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => ToolResponse::ok(v),
            Err(e) => err(format!("corrupt PRD: {e}")),
        },
        Ok(None) => err("no PRD yet · call pipeline_plan.prd_write first".into()),
        Err(e) => err(e.to_string()),
    }
}

async fn prd_update(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let mut current: Value = match mem.recall(&project, "plan", "prd").await {
        Ok(Some(s)) => serde_json::from_str(&s).unwrap_or(json!({})),
        Ok(None) => json!({"goals": [], "non_goals": [], "users": [], "summary": ""}),
        Err(e) => return err(e.to_string()),
    };
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            current[k] = v.clone();
        }
    }
    current["updated_at"] = json!(pipeline_memory::now_rfc3339());
    if let Err(e) = mem
        .remember(&project, "plan", "prd", &current.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(current)
}

// ---------- features ----------

async fn features_add(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let feature = json!({
        "id": id,
        "name": name,
        "description": args.get("description").cloned().unwrap_or(json!("")),
        "ac": args.get("ac").cloned().unwrap_or(json!([])),
        "status": "todo",
        "created_at": pipeline_memory::now_rfc3339(),
    });
    if let Err(e) = mem
        .remember(&project, "feature", &id, &feature.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse {
        ok: true,
        data: feature,
        next_suggested: vec![
            "pipeline_plan.acceptance_define".into(),
            "pipeline_plan.features_track".into(),
        ],
        memory_refs: vec![format!("feature:{id}")],
        error: None,
    }
}

async fn features_list(state: Arc<ServerState>) -> ToolResponse {
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let pairs = match mem.list_scope(&project, "feature").await {
        Ok(p) => p,
        Err(e) => return err(e.to_string()),
    };
    let features: Vec<Value> = pairs
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str::<Value>(&v).ok())
        .collect();
    let counts = count_by_status(&features);
    ToolResponse::ok(json!({"features": features, "counts": counts}))
}

async fn features_update(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let id = match args.get("id").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'id'".into()),
    };
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let mut current: Value = match mem.recall(&project, "feature", &id).await {
        Ok(Some(s)) => serde_json::from_str(&s).unwrap_or(json!({})),
        Ok(None) => return err(format!("feature {id} not found")),
        Err(e) => return err(e.to_string()),
    };
    let patch = args.get("patch").cloned().unwrap_or(args.clone());
    if let Some(obj) = patch.as_object() {
        for (k, v) in obj {
            if k == "id" {
                continue;
            }
            current[k] = v.clone();
        }
    }
    current["updated_at"] = json!(pipeline_memory::now_rfc3339());
    if let Err(e) = mem
        .remember(&project, "feature", &id, &current.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(current)
}

async fn features_track(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let status = match args.get("status").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'status' (todo|in_progress|blocked|done)".into()),
    };
    let mut patched = args.clone();
    patched["status"] = json!(status);
    features_update(patched, state).await
}

async fn acceptance_define(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let id = match args
        .get("feature_id")
        .or_else(|| args.get("id"))
        .and_then(Value::as_str)
    {
        Some(s) => s.to_owned(),
        None => return err("missing 'feature_id'".into()),
    };
    let criteria = args.get("criteria").cloned().unwrap_or(json!([]));
    let mut patched = json!({"id": id, "ac": criteria});
    if let Some(obj) = patched.as_object_mut() {
        obj.insert("id".into(), json!(id));
    }
    features_update(patched, state).await
}

// ---------- milestones ----------

async fn milestone_create(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let milestone = json!({
        "name": name,
        "exit_criteria": args.get("exit_criteria").cloned().unwrap_or(json!([])),
        "feature_ids": args.get("feature_ids").cloned().unwrap_or(json!([])),
        "status": "planned",
        "created_at": pipeline_memory::now_rfc3339(),
    });
    if let Err(e) = mem
        .remember(&project, "milestone", &name, &milestone.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(milestone)
}

async fn milestone_progress(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let milestone: Value = match mem.recall(&project, "milestone", &name).await {
        Ok(Some(s)) => serde_json::from_str(&s).unwrap_or(json!({})),
        Ok(None) => return err(format!("milestone '{name}' not found")),
        Err(e) => return err(e.to_string()),
    };
    let feature_ids: Vec<String> = milestone
        .get("feature_ids")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let mut done = 0usize;
    let total = feature_ids.len();
    for fid in &feature_ids {
        if let Ok(Some(s)) = mem.recall(&project, "feature", fid).await {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                if v.get("status").and_then(Value::as_str) == Some("done") {
                    done += 1;
                }
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let percent = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64) * 100.0
    };
    ToolResponse::ok(json!({
        "milestone": name,
        "features_total": total,
        "features_done": done,
        "percent": percent,
        "exit_criteria": milestone.get("exit_criteria").cloned().unwrap_or(json!([])),
    }))
}

// ---------- aggregate progress ----------

async fn progress(state: Arc<ServerState>) -> ToolResponse {
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };

    let features = mem
        .list_scope(&project, "feature")
        .await
        .unwrap_or_default();
    let parsed: Vec<Value> = features
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str(&v).ok())
        .collect();
    let counts = count_by_status(&parsed);
    let total = parsed.len();
    let done = counts.get("done").and_then(Value::as_u64).unwrap_or(0);
    #[allow(clippy::cast_precision_loss)]
    let percent = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64) * 100.0
    };

    let milestones = mem
        .list_scope(&project, "milestone")
        .await
        .unwrap_or_default();
    let milestone_count = milestones.len();

    let decisions = mem
        .list_scope(&project, "decision")
        .await
        .unwrap_or_default();
    let risks = mem.list_scope(&project, "risk").await.unwrap_or_default();

    ToolResponse::ok(json!({
        "features_total": total,
        "features_by_status": counts,
        "features_done_percent": percent,
        "milestones": milestone_count,
        "decisions": decisions.len(),
        "risks": risks.len(),
    }))
}

// ---------- decisions ----------

async fn decision_log(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let title = match args.get("title").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'title'".into()),
    };
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let decision = json!({
        "id": id,
        "title": title,
        "context": args.get("context").cloned().unwrap_or(json!("")),
        "decision": args.get("decision").cloned().unwrap_or(json!("")),
        "alternatives": args.get("alternatives").cloned().unwrap_or(json!([])),
        "ts": pipeline_memory::now_rfc3339(),
    });
    if let Err(e) = mem
        .remember(&project, "decision", &id, &decision.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(decision)
}

// ---------- risks ----------

async fn risk_add(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let title = match args.get("title").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'title'".into()),
    };
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let risk = json!({
        "id": id,
        "title": title,
        "likelihood": args.get("likelihood").cloned().unwrap_or(json!("medium")),
        "impact": args.get("impact").cloned().unwrap_or(json!("medium")),
        "mitigation": args.get("mitigation").cloned().unwrap_or(json!("")),
        "ts": pipeline_memory::now_rfc3339(),
    });
    if let Err(e) = mem.remember(&project, "risk", &id, &risk.to_string()).await {
        return err(e.to_string());
    }
    ToolResponse::ok(risk)
}

async fn risk_list(state: Arc<ServerState>) -> ToolResponse {
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let pairs = match mem.list_scope(&project, "risk").await {
        Ok(p) => p,
        Err(e) => return err(e.to_string()),
    };
    let risks: Vec<Value> = pairs
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str(&v).ok())
        .collect();
    ToolResponse::ok(json!({"risks": risks}))
}

// ---------- helpers ----------

async fn cfg_project(state: &Arc<ServerState>) -> Result<String, String> {
    if let Some(p) = state.project_id.lock().await.clone() {
        return Ok(p);
    }
    load_config_in_cwd().map(|c| c.project)
}

fn count_by_status(features: &[Value]) -> serde_json::Map<String, Value> {
    let mut counts = serde_json::Map::new();
    for f in features {
        let s = f
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("todo")
            .to_owned();
        let entry = counts.entry(s).or_insert(json!(0));
        if let Some(n) = entry.as_u64() {
            *entry = json!(n + 1);
        }
    }
    counts
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

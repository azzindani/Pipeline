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
use crate::tools::{ToolRequest, ToolResponse};
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
        "link_ingest" => link_ingest(req.args, state).await,
        "feasibility" => feasibility(req.args, state).await,
        "research_notes_list" => research_notes_list(state).await,
        "research_notes_show" => research_notes_show(req.args, state).await,
        "estimate" => estimate(req.args, state).await,
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
    // ! Stored on the record, ✗ inferred later. plan.estimate used to look up a
    // field nothing ever wrote, so every feature silently defaulted to medium.
    let complexity = match validate_complexity(args.get("complexity")) {
        Ok(c) => c.unwrap_or_else(|| DEFAULT_COMPLEXITY.to_owned()),
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let feature = json!({
        "id": id,
        "name": name,
        "description": args.get("description").cloned().unwrap_or(json!("")),
        "ac": args.get("ac").cloned().unwrap_or(json!([])),
        "complexity": complexity,
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
    // `complexity` is a declared arg and feeds plan.estimate · a typo must fail
    // here rather than store an unreadable tag that silently reads as medium.
    let supplied = args.get("complexity").or_else(|| patch.get("complexity"));
    if let Err(e) = validate_complexity(supplied) {
        return err(e);
    }
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

async fn estimate(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let feature_id = args.get("feature_id").and_then(Value::as_str);
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    match estimate_payload(&mem, &project, feature_id).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

const DEFAULT_COMPLEXITY: &str = "medium";
/// Complexity tag → effort multiplier. Order is the order shown to callers.
const COMPLEXITY: &[(&str, f64)] = &[
    ("trivial", 0.5),
    ("small", 1.0),
    ("medium", 1.5),
    ("large", 2.5),
    ("epic", 4.0),
];
/// Cost of existing at all: design · review · wiring. Present at zero ACs.
const BASE_HOURS: f64 = 2.0;
const HOURS_PER_AC: f64 = 4.0;
const HOURS_PER_DAY: f64 = 6.0;
const DAYS_PER_WEEK: f64 = 5.0;
/// Below this many completed features, a calibration factor is noise.
const MIN_CALIBRATION_SAMPLE: usize = 3;

fn complexity_mult(tag: &str) -> Option<f64> {
    COMPLEXITY
        .iter()
        .find(|(name, _)| *name == tag)
        .map(|(_, m)| *m)
}

/// `Ok(None)` → not supplied · `Err` → supplied but unusable.
///
/// ! Refuse rather than default. Silently mapping an unknown tag to medium is
/// how the old estimator returned a confident number for input it did not read.
fn validate_complexity(v: Option<&Value>) -> Result<Option<String>, String> {
    let Some(v) = v else { return Ok(None) };
    if v.is_null() {
        return Ok(None);
    }
    let tags = COMPLEXITY
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(" | ");
    match v.as_str() {
        Some(s) if complexity_mult(s).is_some() => Ok(Some(s.to_owned())),
        Some(s) => Err(format!(
            "unknown complexity '{s}' · expected one of: {tags}"
        )),
        None => Err(format!("'complexity' must be a string · one of: {tags}")),
    }
}

/// Effort for one feature.
///
/// ! Multiplicative on the whole cost, ✗ on the AC term alone. The old form
/// `ac_count × 4 × mult + 2` annihilated complexity at zero ACs, so an epic and
/// a trivial both estimated 2.0h — and since nothing ever stored `complexity`,
/// every feature on a normally-built plan hit exactly that path.
fn feature_hours(f: &Value) -> (usize, String, f64, f64) {
    let ac_count = f.get("ac").and_then(Value::as_array).map_or(0, Vec::len);
    let tag = f
        .get("complexity")
        .and_then(Value::as_str)
        .filter(|t| complexity_mult(t).is_some())
        .unwrap_or(DEFAULT_COMPLEXITY)
        .to_owned();
    let mult = complexity_mult(&tag).unwrap_or(1.5);
    let acs = u32::try_from(ac_count).unwrap_or(u32::MAX);
    let hours = (BASE_HOURS + HOURS_PER_AC * f64::from(acs)) * mult;
    (ac_count, tag, mult, hours)
}

async fn estimate_payload(
    mem: &pipeline_memory::Memory,
    project: &str,
    feature_id: Option<&str>,
) -> Result<Value, String> {
    // ! Propagate · unwrap_or_default() turned an unreadable database into
    // "no features to estimate", which reads as "your plan is empty".
    let pairs = mem
        .list_scope(project, "feature")
        .await
        .map_err(|e| format!("features unreadable: {e}"))?;
    let all: Vec<Value> = pairs
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str::<Value>(&v).ok())
        .collect();
    let selected: Vec<&Value> = all
        .iter()
        .filter(|f| match feature_id {
            Some(id) => f.get("id").and_then(Value::as_str) == Some(id),
            None => true,
        })
        .collect();
    if selected.is_empty() {
        return Err(match feature_id {
            Some(id) => format!("feature '{id}' not found"),
            None => "no features to estimate · call pipeline_plan.features_add first".to_owned(),
        });
    }

    let mut effort_hours = 0.0;
    let mut per_feature: Vec<Value> = Vec::new();
    for f in &selected {
        let (ac_count, tag, mult, hours) = feature_hours(f);
        effort_hours += hours;
        per_feature.push(json!({
            "id": f.get("id"),
            "name": f.get("name"),
            "ac_count": ac_count,
            "complexity": tag,
            "multiplier": mult,
            "hours": hours,
        }));
    }

    // Calibration reads every feature, ✗ only the selected ones — history is
    // history regardless of what is being estimated now.
    let history = calibration(&all);
    let days = (effort_hours / HOURS_PER_DAY).ceil();
    Ok(json!({
        "scope": if feature_id.is_some() { "single_feature" } else { "all_features" },
        "features_considered": selected.len(),
        "per_feature": per_feature,
        "effort_hours": effort_hours,
        "estimate_days": days,
        "estimate_weeks": (days / DAYS_PER_WEEK).ceil(),
        "history": history,
        "model": estimate_model(&history),
    }))
}

/// Ground the heuristic in this project's own delivery record.
///
/// ! `pipeline_runs.duration_ms` measures how long CI took, ✗ how long a
/// feature took to build — calibrating effort against it would be a number
/// dressed as evidence. The only measured signal Pipeline actually holds is
/// feature `created_at` → `updated_at` once status reaches done, and that is
/// **calendar elapsed time, ✗ effort**. Reported as its own axis, never folded
/// into `effort_hours`.
fn calibration(all: &[Value]) -> Value {
    let mut ratios: Vec<f64> = Vec::new();
    let mut elapsed: Vec<f64> = Vec::new();
    for f in all {
        if f.get("status").and_then(Value::as_str) != Some("done") {
            continue;
        }
        let (Some(start), Some(end)) = (
            f.get("created_at")
                .and_then(Value::as_str)
                .and_then(pipeline_memory::parse_rfc3339),
            f.get("updated_at")
                .and_then(Value::as_str)
                .and_then(pipeline_memory::parse_rfc3339),
        ) else {
            continue;
        };
        let hours =
            f64::from(i32::try_from((end - start).num_seconds()).unwrap_or(i32::MAX)) / 3600.0;
        let (_, _, _, predicted) = feature_hours(f);
        if hours <= 0.0 || predicted <= 0.0 {
            continue;
        }
        elapsed.push(hours);
        ratios.push(hours / predicted);
    }
    let basis = "feature.created_at → updated_at where status=done · calendar elapsed, ✗ effort";
    if ratios.len() < MIN_CALIBRATION_SAMPLE {
        return json!({
            "calibrated": false,
            "sample_size": ratios.len(),
            "reason": format!(
                "{} completed feature(s) carry usable timestamps · {MIN_CALIBRATION_SAMPLE} needed",
                ratios.len()
            ),
            "basis": basis,
        });
    }
    // Clamped: one feature left open over a holiday must not multiply the
    // whole plan by 40.
    let factor = median(&mut ratios).clamp(0.25, 8.0);
    json!({
        "calibrated": true,
        "sample_size": ratios.len(),
        "elapsed_to_effort_ratio": round2(factor),
        "median_elapsed_hours": round2(median(&mut elapsed)),
        "basis": basis,
    })
}

fn estimate_model(history: &Value) -> String {
    use std::fmt::Write as _;
    let tags = COMPLEXITY
        .iter()
        .map(|(n, m)| format!("{n}×{m}"))
        .collect::<Vec<_>>()
        .join(" · ");
    let mut s = String::new();
    write!(
        s,
        "effort_hours = ({BASE_HOURS}h base + {HOURS_PER_AC}h × ac_count) × complexity_mult \
         [{tags}] · days = hours ÷ {HOURS_PER_DAY}h · weeks = days ÷ {DAYS_PER_WEEK} · \
         heuristic, ✗ measured"
    )
    .ok();
    if history.get("calibrated").and_then(Value::as_bool) == Some(true) {
        let n = history
            .get("sample_size")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let r = history
            .get("elapsed_to_effort_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        write!(
            s,
            " · history: {n} completed features took a median {r}× their heuristic estimate in \
             calendar time — reported separately under `history`, ✗ applied to effort_hours"
        )
        .ok();
    } else {
        s.push_str(" · ✗ calibrated: see history.reason");
    }
    s
}

fn median(xs: &mut [f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = xs.len() / 2;
    if xs.len() % 2 == 0 {
        f64::midpoint(xs[mid - 1], xs[mid])
    } else {
        xs[mid]
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
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

// ---------- link_ingest + feasibility ----------

async fn link_ingest(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let urls: Vec<String> = match args.get("urls").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        None => return err("missing 'urls' (array of strings)".into()),
    };
    if urls.is_empty() {
        return err("'urls' is empty".into());
    }
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };

    let client = match reqwest::Client::builder()
        .user_agent("pipeline-mcp/0.0.1")
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return err(format!("http client: {e}")),
    };

    let mut notes: Vec<Value> = Vec::new();
    for url in &urls {
        let blob = fetch_note(&client, url).await;
        let id = blob
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Err(e) = mem
            .remember(&project, "research_note", &id, &blob.to_string())
            .await
        {
            return err(e.to_string());
        }
        notes.push(blob);
    }

    ToolResponse {
        ok: true,
        data: json!({"ingested": notes.len(), "notes": notes}),
        next_suggested: vec![
            "pipeline_plan.feasibility".into(),
            "pipeline_plan.research_notes_list".into(),
        ],
        memory_refs: notes
            .iter()
            .filter_map(|n| {
                n.get("id")
                    .and_then(Value::as_str)
                    .map(|i| format!("note:{i}"))
            })
            .collect(),
        error: None,
    }
}

async fn research_notes_list(state: Arc<ServerState>) -> ToolResponse {
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let pairs = mem
        .list_scope(&project, "research_note")
        .await
        .unwrap_or_default();
    let notes: Vec<Value> = pairs
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str::<Value>(&v).ok())
        .map(|n| {
            json!({
                "id": n.get("id"),
                "url": n.get("url"),
                "kind": n.get("kind"),
                "title": n.get("title"),
                "ts": n.get("ts"),
            })
        })
        .collect();
    ToolResponse::ok(json!({"notes": notes, "count": notes.len()}))
}

async fn research_notes_show(args: Value, state: Arc<ServerState>) -> ToolResponse {
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
    match mem.recall(&project, "research_note", &id).await {
        Ok(Some(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => ToolResponse::ok(v),
            Err(e) => err(format!("corrupt note: {e}")),
        },
        Ok(None) => err(format!("note '{id}' not found")),
        Err(e) => err(e.to_string()),
    }
}

/// Extract stack + capability signals from text, fetched links and digested
/// repos → seed a plan skeleton.
///
/// ! What this is **not**: a build/no-build judgement and an effort figure.
/// It previously emitted both — `verdict:"yes"` meant only "a digest file
/// existed on disk", and `effort_estimate` published weeks derived from how
/// many of 14 hardcoded keywords appeared, with no model disclosed. A
/// weeks-denominated number with an undisclosed model gets a quarter planned
/// around it. Effort now belongs to `plan.estimate`, which is grounded in
/// stored features and discloses its arithmetic.
async fn feasibility(args: Value, state: Arc<ServerState>) -> ToolResponse {
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let links = string_array(&args, "links");
    let repos = string_array(&args, "repos");
    if text.trim().is_empty() && links.is_empty() && repos.is_empty() {
        return err("nothing to assess · supply 'text', 'links' or 'repos'".into());
    }
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };

    let mut corpus = text.to_lowercase();
    // `links` used to be accepted and never fetched — it only filtered notes
    // ingested by an earlier call, so a first-time URL contributed nothing.
    let fetched = fetch_links(&mem, &project, &links, &mut corpus).await;
    let notes_considered = match stored_notes(&mem, &project, &links, &mut corpus).await {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let (prior_art, missing_digests) = repo_prior_art(&repos, &mut corpus);

    let identified_stack = identify_stack(&corpus);
    let core_capabilities = identify_capabilities(&corpus);
    let read_ok = fetched
        .iter()
        .filter(|f| f.get("excerpt").is_some())
        .count();

    ToolResponse {
        ok: true,
        data: json!({
            "verdict": if core_capabilities.is_empty() { "insufficient_signal" } else { "signals_identified" },
            "verdict_means": "whether recognizable stack/capability keywords were found in the \
                              corpus · ✗ a judgement that the project is buildable",
            "identified_stack": identified_stack,
            "core_capabilities": core_capabilities,
            "gaps": signal_gaps(&core_capabilities, &identified_stack, &missing_digests),
            "prior_art": prior_art,
            "plan_skeleton": plan_skeleton(&core_capabilities),
            "effort": {
                "available": false,
                "reason": "keyword counts ✗ measure effort · call plan.features_add then \
                           plan.estimate, which discloses its model and calibrates against \
                           this project's completed features",
            },
            "evidence": {
                "text_chars": text.len(),
                "links_given": links.len(),
                "links_read": read_ok,
                "links_failed": fetched.len() - read_ok,
                "link_results": fetched,
                "repos_given": repos.len(),
                "repos_digested": prior_art.len(),
                "repos_not_digested": missing_digests,
                "stored_notes_considered": notes_considered,
            },
            "model": "substring keyword match over the corpus (input text + fetched link text + \
                      stored note excerpts + repo digest summaries) against a fixed list of \
                      15 stack and 14 capability tags · ✗ semantic, ✗ exhaustive",
        }),
        next_suggested: vec![
            "pipeline_plan.create".into(),
            "pipeline_plan.prd_write".into(),
            "pipeline_plan.features_add".into(),
        ],
        memory_refs: vec![],
        error: None,
    }
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Fetch each link now, persist it as a research note, add what was actually
/// read to the corpus. Returns one result per link, successful or not.
async fn fetch_links(
    mem: &pipeline_memory::Memory,
    project: &str,
    links: &[String],
    corpus: &mut String,
) -> Vec<Value> {
    if links.is_empty() {
        return Vec::new();
    }
    let client = match reqwest::Client::builder()
        .user_agent("pipeline-mcp/0.0.1")
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return vec![json!({"error": format!("http client: {e}")})];
        }
    };
    let mut out = Vec::new();
    for url in links {
        let note = fetch_note(&client, url).await;
        if let Some(excerpt) = note.get("excerpt").and_then(Value::as_str) {
            corpus.push(' ');
            corpus.push_str(&excerpt.to_lowercase());
        }
        if let Some(id) = note.get("id").and_then(Value::as_str) {
            // Persisted so research_notes_show can reach the full text · a
            // failure to store must not be reported as a successful read.
            if let Err(e) = mem
                .remember(project, "research_note", id, &note.to_string())
                .await
            {
                out.push(json!({"url": url, "error": format!("store failed: {e}")}));
                continue;
            }
        }
        out.push(json!({
            "url": url,
            "http_status": note.get("http_status"),
            "title": note.get("title"),
            "read": note.get("excerpt").is_some(),
            "error": note.get("error"),
        }));
    }
    out
}

/// Fold previously ingested notes into the corpus · all of them when no
/// `links` were given, otherwise none (the given links were just fetched).
async fn stored_notes(
    mem: &pipeline_memory::Memory,
    project: &str,
    links: &[String],
    corpus: &mut String,
) -> Result<usize, String> {
    if !links.is_empty() {
        return Ok(0);
    }
    let notes = mem
        .list_scope(project, "research_note")
        .await
        .map_err(|e| format!("research notes unreadable: {e}"))?;
    for (_, blob) in &notes {
        if let Ok(v) = serde_json::from_str::<Value>(blob) {
            if let Some(excerpt) = v.get("excerpt").and_then(Value::as_str) {
                corpus.push(' ');
                corpus.push_str(&excerpt.to_lowercase());
            }
        }
    }
    Ok(notes.len())
}

/// Read digests for the named repos · returns (found, not-digested).
///
/// ! A missing digest is reported, ✗ silently skipped. Silence is what let
/// "no digest on disk" and "digest read" produce the same `verdict:"yes"`.
fn repo_prior_art(repos: &[String], corpus: &mut String) -> (Vec<Value>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    let root = std::env::current_dir().unwrap_or_default();
    for alias in repos {
        let path = root.join(".pipeline/digests").join(format!("{alias}.json"));
        let Ok(raw) = std::fs::read_to_string(&path) else {
            missing.push(alias.clone());
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            missing.push(alias.clone());
            continue;
        };
        if let Some(s) = v.get("summary") {
            corpus.push(' ');
            corpus.push_str(&s.to_string().to_lowercase());
        }
        found.push(json!({"alias": alias, "summary": v.get("summary").cloned()}));
    }
    (found, missing)
}

fn signal_gaps(caps: &[&str], stack: &[&str], missing_digests: &[String]) -> Vec<String> {
    let mut gaps = Vec::new();
    if caps.is_empty() {
        gaps.push(
            "no known capability keyword matched · describe the system in more detail, or \
                   the capability is outside the fixed tag list"
                .to_owned(),
        );
    }
    if stack.is_empty() {
        gaps.push("no concrete stack signals · agent should pick a default".to_owned());
    }
    for alias in missing_digests {
        gaps.push(format!(
            "repo '{alias}' has no digest at .pipeline/digests/{alias}.json · call \
             pipeline_repo.digest first — it contributed nothing here"
        ));
    }
    gaps
}

fn plan_skeleton(caps: &[&str]) -> Value {
    json!({
        "features": caps.iter().map(|c| json!({
            "name": c,
            "ac": [format!("Implements core {c} behavior"), "Has unit tests · green static stage"],
        })).collect::<Vec<_>>(),
        "milestones": [
            json!({"name": "POC", "exit_criteria": ["pipeline_run.stage(fast) green"]}),
            json!({"name": "MVP", "exit_criteria": ["pipeline_run.preflight green", "deploy to staging"]}),
            json!({"name": "v1", "exit_criteria": ["health green for 7 days unattended"]}),
        ],
    })
}

/// Fetch one URL into a research-note record · shared by `link_ingest` and
/// `feasibility` so both see the same fields and the same failure reporting.
///
/// ! A 4xx/5xx or a DNS failure still produces a record — carrying
/// `http_status` / `error` and no `excerpt`. The caller must count what it
/// actually read, ✗ what it was handed.
async fn fetch_note(client: &reqwest::Client, url: &str) -> Value {
    let kind = classify_url(url);
    let mut record = serde_json::Map::new();
    record.insert("id".into(), json!(Uuid::new_v4().to_string()));
    record.insert("url".into(), json!(url));
    record.insert("kind".into(), json!(kind));
    record.insert("ts".into(), json!(pipeline_memory::now_rfc3339()));

    if matches!(kind, "github" | "gitlab" | "bitbucket" | "git") {
        // Don't fetch · agent should call repo.register on these URLs.
        record.insert(
            "advice".into(),
            json!("git URL · call pipeline_repo.register(url) to track + digest"),
        );
        return Value::Object(record);
    }
    match client.get(url).send().await {
        Ok(resp) => {
            record.insert("http_status".into(), json!(resp.status().as_u16()));
            if resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                let extracted = extract_text(&body);
                record.insert("title".into(), json!(extract_title(&body)));
                record.insert("excerpt".into(), json!(truncate(&extracted, 4_000)));
                record.insert("byte_length".into(), json!(body.len()));
            }
        }
        Err(e) => {
            record.insert("error".into(), json!(e.to_string()));
        }
    }
    Value::Object(record)
}

#[allow(clippy::case_sensitive_file_extension_comparisons)] // already lowercased above
fn classify_url(url: &str) -> &'static str {
    let l = url.to_lowercase();
    if l.contains("github.com/") || l.ends_with(".git") {
        "github"
    } else if l.contains("gitlab.com/") {
        "gitlab"
    } else if l.contains("bitbucket.org/") {
        "bitbucket"
    } else if l.starts_with("git://") || l.starts_with("git@") {
        "git"
    } else if l.contains("arxiv.org/") {
        "paper"
    } else if l.contains("youtube.com/") || l.contains("youtu.be/") {
        "video"
    } else if l.contains("/docs") || l.contains("docs.") {
        "docs"
    } else if l.contains("medium.com/") || l.contains("dev.to/") || l.contains("substack.com/") {
        "blog"
    } else if l.contains("npmjs.com/") || l.contains("crates.io/") || l.contains("pypi.org/") {
        "package"
    } else if l.contains("twitter.com/") || l.contains("x.com/") {
        "social"
    } else {
        "article"
    }
}

fn extract_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(start) = lower.find("<title") {
        if let Some(gt) = lower[start..].find('>') {
            let after = &html[start + gt + 1..];
            if let Some(end) = after.to_ascii_lowercase().find("</title>") {
                return after[..end].trim().to_owned();
            }
        }
    }
    String::new()
}

fn extract_text(html: &str) -> String {
    // Strip <script> + <style> blocks, then all tags, then collapse whitespace.
    let no_script = strip_block(html, "<script", "</script>");
    let no_style = strip_block(&no_script, "<style", "</style>");
    let tag_re = regex::Regex::new(r"(?s)<[^>]+>").unwrap();
    let no_tags = tag_re.replace_all(&no_style, " ").into_owned();
    let ws_re = regex::Regex::new(r"\s+").unwrap();
    ws_re.replace_all(&no_tags, " ").trim().to_owned()
}

fn strip_block(input: &str, open: &str, close: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open_l = open.to_ascii_lowercase();
    let close_l = close.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut idx = 0usize;
    while idx < input.len() {
        if let Some(found) = lower[idx..].find(&open_l) {
            let start = idx + found;
            out.push_str(&input[idx..start]);
            if let Some(rel_end) = lower[start..].find(&close_l) {
                idx = start + rel_end + close_l.len();
            } else {
                idx = input.len();
            }
        } else {
            out.push_str(&input[idx..]);
            break;
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!("{}...", &s[..max])
    }
}

const STACKS: &[(&str, &[&str])] = &[
    ("rust", &["rust", "cargo", "tokio", "axum", "actix"]),
    (
        "python",
        &["python", "django", "flask", "fastapi", "uv ", "pip "],
    ),
    (
        "typescript",
        &[
            "typescript",
            " ts ",
            "node.js",
            "nodejs",
            "bun",
            "deno",
            "react",
            "next.js",
        ],
    ),
    ("go", &["golang", " go ", "go module"]),
    ("postgres", &["postgres", "postgresql"]),
    ("redis", &["redis"]),
    ("kafka", &["kafka"]),
    ("nats", &["nats.io", " nats "]),
    ("mongo", &["mongo", "mongodb"]),
    ("clickhouse", &["clickhouse"]),
    ("stripe", &["stripe"]),
    ("kubernetes", &["kubernetes", "k8s", "kubectl"]),
    ("docker", &["docker", "container", "compose"]),
    ("playwright", &["playwright"]),
];

const CAPABILITIES: &[(&str, &[&str])] = &[
    (
        "auth",
        &["authentication", "auth ", "oauth", "jwt", "login"],
    ),
    ("rate-limit", &["rate limit", "rate-limit", "throttling"]),
    ("billing", &["billing", "invoicing", "subscription"]),
    ("metering", &["metering", "usage tracking"]),
    ("rating", &["rating engine", "pricing rule"]),
    ("queue", &["queue", "message broker", "background job"]),
    ("webhook", &["webhook"]),
    (
        "search",
        &[
            "full-text search",
            "elasticsearch",
            "opensearch",
            " search ",
        ],
    ),
    (
        "multi-tenant",
        &["multi-tenant", "multi tenant", "tenant isolation"],
    ),
    ("analytics", &["analytics", "reporting"]),
    ("notifications", &["notification", "email send", "sms"]),
    ("file-upload", &["file upload", "object storage", "s3"]),
    ("real-time", &["websocket", "real-time", "realtime"]),
    ("audit-log", &["audit log", "audit trail"]),
];

fn identify_stack(corpus: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (name, keywords) in STACKS {
        if keywords.iter().any(|k| corpus.contains(k)) {
            out.push(*name);
        }
    }
    out
}

fn identify_capabilities(corpus: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (name, keywords) in CAPABILITIES {
        if keywords.iter().any(|k| corpus.contains(k)) {
            out.push(*name);
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_memory::Memory;

    async fn fresh() -> Memory {
        let m = Memory::open_in_memory().await.expect("open");
        m.upsert_project("p1", "pipeline", "rust")
            .await
            .expect("upsert");
        m
    }

    async fn add_feature(m: &Memory, id: &str, f: Value) {
        m.remember("p1", "feature", id, &f.to_string())
            .await
            .expect("remember");
    }

    fn feature(id: &str, complexity: &str, acs: usize) -> Value {
        json!({
            "id": id,
            "name": format!("feature {id}"),
            "complexity": complexity,
            "ac": (0..acs).map(|i| format!("criterion {i}")).collect::<Vec<_>>(),
            "status": "todo",
            "created_at": "2026-07-01T09:00:00Z",
        })
    }

    // ---------- estimate ----------

    #[tokio::test]
    async fn complexity_still_matters_when_a_feature_has_no_acceptance_criteria() {
        // The compounding bug: base = ac_count × 4 made the multiplier a factor
        // of zero, so `hours = base × mult + 2` returned 2.0h for an epic and
        // 2.0h for a trivial. Every feature on a normally-built plan hit this.
        let m = fresh().await;
        add_feature(&m, "epic", feature("epic", "epic", 0)).await;
        add_feature(&m, "triv", feature("triv", "trivial", 0)).await;

        let hours = |v: &Value| v["per_feature"][0]["hours"].as_f64().unwrap();
        let epic = estimate_payload(&m, "p1", Some("epic")).await.unwrap();
        let trivial = estimate_payload(&m, "p1", Some("triv")).await.unwrap();
        assert!(
            hours(&epic) > hours(&trivial),
            "epic {} must exceed trivial {}",
            hours(&epic),
            hours(&trivial)
        );
        // (2h base + 0 ACs) × 4.0 = 8h · × 0.5 = 1h
        assert_eq!(epic["per_feature"][0]["hours"], json!(8.0));
        assert_eq!(trivial["per_feature"][0]["hours"], json!(1.0));
    }

    #[tokio::test]
    async fn acceptance_criteria_still_raise_the_estimate() {
        let m = fresh().await;
        add_feature(&m, "a", feature("a", "medium", 0)).await;
        add_feature(&m, "b", feature("b", "medium", 3)).await;
        let a = estimate_payload(&m, "p1", Some("a")).await.unwrap();
        let b = estimate_payload(&m, "p1", Some("b")).await.unwrap();
        assert!(b["per_feature"][0]["hours"].as_f64() > a["per_feature"][0]["hours"].as_f64());
    }

    #[tokio::test]
    async fn an_estimate_discloses_its_model() {
        // An estimate that hides its model reads as analysis. The caller has to
        // be able to see the arithmetic and decide whether to trust it.
        let m = fresh().await;
        add_feature(&m, "a", feature("a", "large", 2)).await;
        let v = estimate_payload(&m, "p1", None).await.unwrap();
        let model = v["model"].as_str().expect("model disclosed");
        assert!(model.contains("effort_hours ="), "{model}");
        assert!(model.contains("complexity_mult"), "{model}");
        assert!(model.contains("large×2.5"), "{model}");
        assert!(model.contains("heuristic"), "{model}");
        // Uncalibrated is stated, ✗ implied by omission.
        assert_eq!(v["history"]["calibrated"], json!(false));
        assert!(model.contains("✗ calibrated"), "{model}");
    }

    #[tokio::test]
    async fn the_complexity_stored_by_features_add_is_the_one_estimate_reads() {
        // features_add never wrote `complexity`, so the lookup always missed and
        // silently defaulted to medium — the second half of the 2.0h bug.
        let m = fresh().await;
        let stored =
            json!({"id": "x", "name": "x", "complexity": "epic", "ac": [], "status": "todo"});
        add_feature(&m, "x", stored).await;
        let v = estimate_payload(&m, "p1", Some("x")).await.unwrap();
        assert_eq!(v["per_feature"][0]["complexity"], json!("epic"));
        assert_eq!(v["per_feature"][0]["multiplier"], json!(4.0));
    }

    #[tokio::test]
    async fn an_estimate_is_calibrated_against_completed_features_when_enough_exist() {
        // Grounded in measured history · each of these took 4× its heuristic in
        // calendar time, and that ratio is reported rather than assumed.
        let m = fresh().await;
        for i in 0..3 {
            let mut f = feature(&format!("d{i}"), "medium", 0); // heuristic 3h
            f["status"] = json!("done");
            f["created_at"] = json!("2026-07-01T00:00:00Z");
            f["updated_at"] = json!("2026-07-01T12:00:00Z"); // 12h elapsed
            add_feature(&m, &format!("d{i}"), f).await;
        }
        add_feature(&m, "todo", feature("todo", "medium", 0)).await;
        let v = estimate_payload(&m, "p1", Some("todo")).await.unwrap();
        assert_eq!(v["history"]["calibrated"], json!(true), "{v}");
        assert_eq!(v["history"]["sample_size"], json!(3));
        assert_eq!(v["history"]["elapsed_to_effort_ratio"], json!(4.0));
        // ! Calendar elapsed is reported on its own axis, never folded into effort.
        assert_eq!(v["effort_hours"], json!(3.0));
        assert!(v["history"]["basis"].as_str().unwrap().contains("✗ effort"));
    }

    #[tokio::test]
    async fn too_few_completed_features_says_so_instead_of_calibrating_on_noise() {
        let m = fresh().await;
        let mut done = feature("d0", "medium", 0);
        done["status"] = json!("done");
        done["updated_at"] = json!("2026-07-02T09:00:00Z");
        add_feature(&m, "d0", done).await;
        add_feature(&m, "t", feature("t", "medium", 0)).await;
        let v = estimate_payload(&m, "p1", Some("t")).await.unwrap();
        assert_eq!(v["history"]["calibrated"], json!(false));
        assert_eq!(v["history"]["sample_size"], json!(1));
        assert!(v["history"]["reason"].as_str().unwrap().contains("needed"));
    }

    #[tokio::test]
    async fn an_unreadable_feature_store_is_not_reported_as_an_empty_plan() {
        // "no features to estimate · call features_add first" told the agent its
        // plan was empty when the database was simply unreadable.
        let m = fresh().await;
        m.pool().close().await;
        let e = estimate_payload(&m, "p1", None)
            .await
            .expect_err("must fail");
        assert!(e.contains("unreadable"), "{e}");
    }

    #[tokio::test]
    async fn estimating_an_unknown_feature_id_fails() {
        let m = fresh().await;
        add_feature(&m, "a", feature("a", "small", 1)).await;
        let e = estimate_payload(&m, "p1", Some("nope"))
            .await
            .expect_err("✗ silently estimating the whole plan instead");
        assert!(e.contains("not found"), "{e}");
    }

    // ---------- complexity validation ----------

    #[test]
    fn an_unknown_complexity_tag_is_refused_not_defaulted() {
        // Defaulting a typo to medium is the exact "accepted, dropped, reported
        // as success" pattern the fidelity rule exists to stop.
        let e = validate_complexity(Some(&json!("humongous"))).expect_err("must refuse");
        assert!(e.contains("humongous") && e.contains("trivial"), "{e}");
        assert!(
            validate_complexity(Some(&json!(3))).is_err(),
            "✗ non-strings"
        );
        assert_eq!(validate_complexity(None).unwrap(), None);
        assert_eq!(
            validate_complexity(Some(&json!("epic"))).unwrap(),
            Some("epic".to_owned())
        );
    }

    #[test]
    fn every_complexity_tag_has_a_distinct_multiplier() {
        let mut seen: Vec<f64> = COMPLEXITY.iter().map(|(_, m)| *m).collect();
        let before = seen.len();
        seen.sort_by(|a, b| a.partial_cmp(b).unwrap());
        seen.dedup();
        assert_eq!(seen.len(), before, "duplicate multipliers collapse tags");
        assert!(complexity_mult(DEFAULT_COMPLEXITY).is_some());
    }

    // ---------- feasibility signals ----------

    #[test]
    fn a_missing_digest_is_reported_rather_than_silently_skipped() {
        // verdict:"yes" used to mean only "a digest file existed on disk", and a
        // missing one was invisible.
        let mut corpus = String::new();
        let (found, missing) = repo_prior_art(&["definitely-not-digested".to_owned()], &mut corpus);
        assert!(found.is_empty());
        assert_eq!(missing.len(), 1);
        let gaps = signal_gaps(&[], &[], &missing);
        assert!(gaps.iter().any(|g| g.contains("no digest at")), "{gaps:?}");
    }

    #[test]
    fn feasibility_reports_signals_it_actually_found() {
        let corpus = "a rust service using postgres with jwt login and rate limiting";
        assert!(identify_stack(corpus).contains(&"rust"));
        assert!(identify_stack(corpus).contains(&"postgres"));
        assert!(identify_capabilities(corpus).contains(&"auth"));
        assert!(identify_capabilities(corpus).contains(&"rate-limit"));
        // A corpus with no known tag reports none, ✗ invents one.
        assert!(identify_capabilities("an unremarkable sentence").is_empty());
    }

    #[test]
    fn a_plan_skeleton_is_derived_from_the_capabilities_found() {
        let skeleton = plan_skeleton(&["auth", "queue"]);
        assert_eq!(skeleton["features"].as_array().unwrap().len(), 2);
        assert_eq!(plan_skeleton(&[])["features"].as_array().unwrap().len(), 0);
    }
}

//! `pipeline_session` handler · lock · start · end · handover · context.
//!
//! Two kinds of session exist, and the difference is deliberate:
//!
//! | Kind | Opened by | Exclusive | Row lives in |
//! |---|---|---|---|
//! | lock | `session.lock` | yes · contention → `ok:false` | `sessions` + `session_locks` |
//! | observer | `session.start` | ✗ · never blocks the lock holder | `memory_kv` scope `session` |
//!
//! ! Both close through `session.end` with the id their opener returned —
//! `end` tries the lock table first, then falls back to the observer store.
//! An agent must not have to know which kind it holds to close it.
//!
//! Observer rows land in `memory_kv` rather than `sessions` because
//! `pipeline-mcp` has no direct `sqlx` dependency and `pipeline_memory` exposes
//! no non-locking session constructor. Same durability (same file, same
//! transaction log), different table — see the handover note in the crate.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use pipeline_memory::{Memory, MemoryError};
use serde_json::{Value, json};
use std::sync::Arc;

/// `memory_kv` scope holding observer sessions · one row per `session.start`.
const OBSERVER_SCOPE: &str = "session";

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
        "context" => context(&req.args, state).await,
        "file_context" => file_context(&req.args, state).await,
        "task_context" => task_context(&req.args, state).await,
        other => unknown(other),
    }
}

/// Open an observer session · ✗ takes the exclusive lock.
///
/// For agents that read a project while someone else builds it: a reviewer, a
/// monitor, a second pair of eyes. Lock-or-nothing forces those callers to
/// either block the builder or stay anonymous, so neither their goal nor their
/// runs are attributable afterwards.
///
/// Returns a `session_id` that `session.end` accepts · persists `goal` ·
/// surfaces in `handover.open_observers`.
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

    let data = match open_observer_session(&mem, &cfg.project, &agent_id, goal).await {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let session_id = data
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    *state.project_id.lock().await = Some(cfg.project.clone());
    ToolResponse {
        ok: true,
        data,
        next_suggested: vec![
            "pipeline_session.handover".into(),
            "pipeline_session.end(session_id)".into(),
        ],
        memory_refs: vec![format!("session:{session_id}")],
        error: None,
    }
}

/// Persist the observer row, then report who holds the lock right now.
///
/// ! `current_lock` errors propagate. "I could not read the lock table" must
/// never render as `lock_held: false` — that is the exact collapse of
/// "unknown" into "fine" that the fidelity rule forbids.
async fn open_observer_session(
    mem: &Memory,
    project: &str,
    agent_id: &str,
    goal: Option<&str>,
) -> Result<Value, String> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let stored = json!({
        "session_id": session_id,
        "project": project,
        "agent_id": agent_id,
        "goal": goal,
        "kind": "observer",
        "started_at": pipeline_memory::now_rfc3339(),
        "ended_at": Value::Null,
    });
    mem.remember(project, OBSERVER_SCOPE, &session_id, &stored.to_string())
        .await
        .map_err(|e| e.to_string())?;

    // Live lock state is read, ✗ stored — a persisted copy goes stale the
    // moment the holder unlocks.
    let lock = mem
        .current_lock(project)
        .await
        .map_err(|e| format!("lock state unreadable: {e}"))?;
    let mut data = stored;
    data["lock_held"] = json!(false);
    data["exclusive_lock_holder"] = match &lock {
        Some(l) => json!({"session_id": l.session_id, "agent_id": l.agent_id}),
        None => Value::Null,
    };
    Ok(data)
}

/// Observer sessions with no `ended_at` · surfaced on the handover packet so a
/// joining agent sees every attached agent, ✗ only the lock holder.
async fn open_observers(mem: &Memory, project: &str) -> Result<Vec<Value>, String> {
    let rows = mem
        .list_scope(project, OBSERVER_SCOPE)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str::<Value>(&v).ok())
        .filter(|r| r.get("ended_at").is_none_or(Value::is_null))
        .collect())
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
    // Observer rows are keyed by project, so a lookup needs one. Absent config
    // is ✗ fatal — the lock path does not need it.
    let project = match state.project_id.lock().await.clone() {
        Some(p) => Some(p),
        None => load_config_in_cwd().ok().map(|c| c.project),
    };
    let data = match close_session(&mem, project.as_deref(), &session_id, outcome, summary).await {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    // Only the exclusive lock binds this connection to a project · closing an
    // observer must not detach an agent that also holds the lock.
    if data.get("kind").and_then(Value::as_str) == Some("lock") {
        *state.project_id.lock().await = None;
    }
    ToolResponse::ok(data)
}

/// Close either kind of session by id · lock table first, observer store second.
async fn close_session(
    mem: &Memory,
    project: Option<&str>,
    session_id: &str,
    outcome: &str,
    summary: Option<&str>,
) -> Result<Value, String> {
    match mem.end_session(session_id, outcome, summary).await {
        Ok(()) => Ok(json!({"ended": session_id, "outcome": outcome, "kind": "lock"})),
        // ! Only "no such row" falls through. A real SQLite error must surface
        // as itself, ✗ be retried as a missing observer and reported as one.
        Err(MemoryError::SessionNotFound(_)) => {
            close_observer_session(mem, project, session_id, outcome, summary).await
        }
        Err(e) => Err(e.to_string()),
    }
}

async fn close_observer_session(
    mem: &Memory,
    project: Option<&str>,
    session_id: &str,
    outcome: &str,
    summary: Option<&str>,
) -> Result<Value, String> {
    let Some(project) = project else {
        return Err(format!(
            "session '{session_id}' not found in the lock table · no project context (pipeline.yaml) to search observer sessions"
        ));
    };
    let raw = mem
        .recall(project, OBSERVER_SCOPE, session_id)
        .await
        .map_err(|e| e.to_string())?;
    let Some(raw) = raw else {
        return Err(format!("session '{session_id}' not found"));
    };
    let mut rec: Value =
        serde_json::from_str(&raw).map_err(|e| format!("corrupt session record: {e}"))?;
    // Re-ending would silently overwrite the first outcome · the second caller
    // has to know its close was a no-op.
    if let Some(at) = rec.get("ended_at").and_then(Value::as_str) {
        return Err(format!("session '{session_id}' already ended at {at}"));
    }
    rec["ended_at"] = json!(pipeline_memory::now_rfc3339());
    rec["outcome"] = json!(outcome);
    rec["summary"] = json!(summary);
    mem.remember(project, OBSERVER_SCOPE, session_id, &rec.to_string())
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({"ended": session_id, "outcome": outcome, "kind": "observer"}))
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
    match handover_payload(&mem, &project_id).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

/// Canonical packet plus attached observers.
///
/// `active_session` stays the lock holder — that is what it has always meant
/// and what `unlock`/`steal` act on. Observers are a separate list so a session
/// opened by `start` is visible without implying it holds anything.
async fn handover_payload(mem: &Memory, project_id: &str) -> Result<Value, String> {
    let pack = mem.handover(project_id).await.map_err(|e| e.to_string())?;
    let mut data = serde_json::to_value(pack).map_err(|e| e.to_string())?;
    data["open_observers"] = json!(open_observers(mem, project_id).await?);
    Ok(data)
}

async fn context(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let scope = args.get("scope").and_then(Value::as_str);
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20);
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
    let mut data = json!({
        "project": pack.project,
        "active_session": pack.active_session,
        "last_run": pack.last_run,
        "recent_failures": pack.recent_failures,
    });
    if let Some(s) = scope {
        let pairs = mem.list_scope(&cfg.project, s).await.unwrap_or_default();
        let entries: Vec<Value> = pairs
            .into_iter()
            .take(limit.max(0).try_into().unwrap_or(usize::MAX))
            .filter_map(|(k, v)| {
                serde_json::from_str::<Value>(&v)
                    .ok()
                    .map(|val| json!({"key": k, "value": val}))
            })
            .collect();
        data["scope"] = json!(s);
        data["entries"] = json!(entries);
    }
    ToolResponse::ok(data)
}

async fn file_context(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let path = match args.get("path").and_then(Value::as_str) {
        Some(p) => p.to_owned(),
        None => return err("missing 'path'".into()),
    };
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    // Search recent runs for stderr/stdout mentioning the file path.
    let runs = mem.run_history(&cfg.project, 20).await.unwrap_or_default();
    let mut hits: Vec<Value> = Vec::new();
    for r in &runs {
        let mention = r.stderr.as_deref().is_some_and(|s| s.contains(&path))
            || r.stdout.as_deref().is_some_and(|s| s.contains(&path));
        if mention {
            hits.push(json!({
                "run_id": r.id,
                "stage": r.stage,
                "status": r.status,
                "created_at": r.created_at,
            }));
        }
    }
    ToolResponse::ok(json!({
        "path": path,
        "mentioned_in_runs": hits.len(),
        "runs": hits,
    }))
}

async fn task_context(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let description = match args.get("description").and_then(Value::as_str) {
        Some(d) => d.to_owned(),
        None => return err("missing 'description'".into()),
    };
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n.clamp(1, 100)).ok())
        .unwrap_or(10);
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    match task_context_payload(&mem, &cfg.project, &description, limit).await {
        Ok(v) => ToolResponse::ok(v),
        Err(e) => err(e),
    }
}

/// Scopes searched for prior work · (`memory_kv` scope, fields that carry text).
const TASK_SCOPES: &[(&str, &[&str])] = &[
    ("feature", &["name", "description", "ac"]),
    ("research_note", &["title", "excerpt", "url"]),
    ("decision", &["title", "context", "decision"]),
];

/// Rank stored work by term overlap with the task description.
///
/// ! The three outcomes are distinct and must stay distinct:
///   `nothing_stored`            → the project has no prior work at all
///   `no_match_above_threshold`  → prior work exists, none of it looks related
///   `matches`                   → ranked hits, with the terms that matched
/// Collapsing the middle case into the first is what told agents "greenfield"
/// on a project that already had the feature — and got it built twice.
async fn task_context_payload(
    mem: &Memory,
    project: &str,
    description: &str,
    limit: usize,
) -> Result<Value, String> {
    let query = search_terms(description);
    if query.is_empty() {
        return Err(format!(
            "'{description}' has no searchable terms · all tokens were stopwords or shorter than {MIN_TERM_LEN} characters"
        ));
    }
    let mut stored = serde_json::Map::new();
    let mut hits: Vec<Value> = Vec::new();
    for (scope, fields) in TASK_SCOPES {
        // ! Propagate · unwrap_or_default() here reports an unreadable database
        // as an empty project, which reads as "nothing was ever built".
        let rows = mem
            .list_scope(project, scope)
            .await
            .map_err(|e| format!("{scope} scope unreadable: {e}"))?;
        stored.insert((*scope).to_owned(), json!(rows.len()));
        for (key, raw) in rows {
            let Ok(rec) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let haystack = collect_text(&rec, fields);
            let matched = matched_terms(&query, &haystack);
            if !meets_floor(matched.len(), query.len()) {
                continue;
            }
            hits.push(json!({
                "scope": scope,
                "id": rec.get("id").and_then(Value::as_str).unwrap_or(&key),
                "label": record_label(&rec),
                "score": round2(ratio(matched.len(), query.len())),
                "matched_terms": matched,
                "record": rec,
            }));
        }
    }
    sort_by_score(&mut hits);
    hits.truncate(limit);
    let total_stored: u64 = stored.values().filter_map(Value::as_u64).sum();
    Ok(json!({
        "description": description,
        "terms": query,
        "result": outcome_label(hits.len(), total_stored),
        "matches": hits,
        "match_count": hits.len(),
        "stored": stored,
        "model": format!(
            "term overlap · description tokenized, stopwords + tokens under {MIN_TERM_LEN} chars dropped · \
             score = matched_terms / query_terms · floor = {FLOOR_HINT} · ✗ semantic, ✗ fuzzy"
        ),
    }))
}

const MIN_TERM_LEN: usize = 3;
const FLOOR_HINT: &str = "2 terms (1 when the query has ≤2 terms)";

/// Words carrying no discriminating signal · kept small on purpose: an
/// over-eager list silently deletes real query terms.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "our", "its", "was", "were",
    "are", "has", "have", "had", "not", "but", "all", "any", "can", "will", "should", "would",
    "add", "new", "use", "using", "make", "get", "set", "when", "then", "than", "how", "why",
    "what", "who", "some", "each", "via", "per", "out", "off", "over", "under", "about",
];

fn search_terms(description: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
    {
        if tok.len() < MIN_TERM_LEN || STOPWORDS.contains(&tok) || out.iter().any(|t| t == tok) {
            continue;
        }
        out.push(tok.to_owned());
    }
    out
}

/// Query terms present in `haystack` as whole tokens.
///
/// Whole-token, ✗ substring: substring matching makes "auth" hit "author" and
/// every three-letter term hit something.
fn matched_terms(query: &[String], haystack: &str) -> Vec<String> {
    let tokens: Vec<&str> = haystack
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    query
        .iter()
        .filter(|q| tokens.iter().any(|t| t == q))
        .cloned()
        .collect()
}

/// A single shared term is coincidence on a long query · evidence on a short one.
fn meets_floor(matched: usize, query_len: usize) -> bool {
    if query_len <= 2 {
        matched >= 1
    } else {
        matched >= 2
    }
}

/// Flatten the named fields (strings and string arrays) into one lowercase blob.
fn collect_text(rec: &Value, fields: &[&str]) -> String {
    let mut out = String::new();
    for f in fields {
        match rec.get(*f) {
            Some(Value::String(s)) => {
                out.push(' ');
                out.push_str(&s.to_lowercase());
            }
            Some(Value::Array(items)) => {
                for i in items.iter().filter_map(Value::as_str) {
                    out.push(' ');
                    out.push_str(&i.to_lowercase());
                }
            }
            _ => {}
        }
    }
    out
}

fn record_label(rec: &Value) -> String {
    for field in ["name", "title", "url"] {
        if let Some(s) = rec.get(field).and_then(Value::as_str) {
            if !s.is_empty() {
                return s.to_owned();
            }
        }
    }
    String::new()
}

fn outcome_label(hits: usize, total_stored: u64) -> &'static str {
    if hits > 0 {
        "matches"
    } else if total_stored == 0 {
        "nothing_stored"
    } else {
        "no_match_above_threshold"
    }
}

fn sort_by_score(hits: &mut [Value]) {
    hits.sort_by(|a, b| {
        let sa = a.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let sb = b.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Integer ratio without a lossy `as` cast.
fn ratio(matched: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let m = u32::try_from(matched).unwrap_or(u32::MAX);
    let t = u32::try_from(total).unwrap_or(u32::MAX);
    f64::from(m) / f64::from(t)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> Memory {
        let m = Memory::open_in_memory().await.expect("open");
        m.upsert_project("p1", "pipeline", "rust")
            .await
            .expect("upsert");
        m
    }

    async fn add(mem: &Memory, scope: &str, id: &str, rec: Value) {
        mem.remember("p1", scope, id, &rec.to_string())
            .await
            .expect("remember");
    }

    // ---------- start / end ----------

    #[tokio::test]
    async fn a_session_can_be_closed_by_the_id_that_start_returned() {
        // The whole defect: start returned no id, so end (which hard-requires
        // session_id) could never close what start opened.
        let m = fresh().await;
        let started = open_observer_session(&m, "p1", "observer-1", Some("review auth"))
            .await
            .expect("start");
        let id = started["session_id"].as_str().expect("session_id returned");

        let ended = close_session(&m, Some("p1"), id, "ok", Some("looked fine"))
            .await
            .expect("end must accept the id start returned");
        assert_eq!(ended["ended"], json!(id));
        assert_eq!(ended["kind"], json!("observer"));
    }

    #[tokio::test]
    async fn start_persists_the_goal_it_was_given() {
        // `goal` was read and echoed, never written · nothing could recover it.
        let m = fresh().await;
        let started = open_observer_session(&m, "p1", "a1", Some("fix JWT flow"))
            .await
            .unwrap();
        let id = started["session_id"].as_str().unwrap();
        let raw = m
            .recall("p1", OBSERVER_SCOPE, id)
            .await
            .unwrap()
            .expect("a row exists");
        let stored: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(stored["goal"], json!("fix JWT flow"));
        assert_eq!(stored["agent_id"], json!("a1"));
    }

    #[tokio::test]
    async fn handover_reports_a_session_that_start_opened() {
        // Regression: handover said active_session:null immediately after a
        // successful start.
        let m = fresh().await;
        let started = open_observer_session(&m, "p1", "a1", Some("watch"))
            .await
            .unwrap();
        let id = started["session_id"].as_str().unwrap().to_owned();

        let pack = handover_payload(&m, "p1").await.unwrap();
        let observers = pack["open_observers"].as_array().expect("field present");
        assert_eq!(observers.len(), 1, "{pack}");
        assert_eq!(observers[0]["session_id"], json!(id));

        close_session(&m, Some("p1"), &id, "ok", None)
            .await
            .unwrap();
        let after = handover_payload(&m, "p1").await.unwrap();
        assert!(
            after["open_observers"].as_array().unwrap().is_empty(),
            "a closed observer must drop off the packet"
        );
    }

    #[tokio::test]
    async fn an_observer_session_does_not_block_the_lock() {
        // The reason start exists at all: an observer must attach to a project
        // someone else is building, ✗ contend for the lock.
        let m = fresh().await;
        let lock = m
            .lock_session("p1", Some("builder"), Some("ship"))
            .await
            .unwrap();
        let started = open_observer_session(&m, "p1", "reviewer", None)
            .await
            .expect("observer must open while the lock is held");
        assert_eq!(started["lock_held"], json!(false));
        assert_eq!(
            started["exclusive_lock_holder"]["session_id"],
            json!(lock.session_id),
            "the observer is told who holds the lock"
        );
        // ...and the lock holder still closes through the same action.
        let ended = close_session(&m, Some("p1"), &lock.session_id, "ok", None)
            .await
            .unwrap();
        assert_eq!(ended["kind"], json!("lock"));
    }

    #[tokio::test]
    async fn closing_an_unknown_session_fails() {
        let m = fresh().await;
        let e = close_session(&m, Some("p1"), "no-such-id", "ok", None)
            .await
            .expect_err("✗ ok:true for a session that never existed");
        assert!(e.contains("not found"), "{e}");
    }

    #[tokio::test]
    async fn a_session_cannot_be_ended_twice() {
        // A second close would otherwise overwrite the first outcome silently.
        let m = fresh().await;
        let started = open_observer_session(&m, "p1", "a1", None).await.unwrap();
        let id = started["session_id"].as_str().unwrap();
        close_session(&m, Some("p1"), id, "ok", None).await.unwrap();
        let e = close_session(&m, Some("p1"), id, "failed", None)
            .await
            .expect_err("second close must refuse");
        assert!(e.contains("already ended"), "{e}");
    }

    // ---------- task_context ----------

    async fn seeded() -> Memory {
        let m = fresh().await;
        add(
            &m,
            "feature",
            "f1",
            json!({"id": "f1", "name": "JWT login", "description": "issue and verify tokens", "ac": ["rejects expired token"]}),
        )
        .await;
        add(
            &m,
            "feature",
            "f2",
            json!({"id": "f2", "name": "CSV export", "description": "download reports"}),
        )
        .await;
        add(
            &m,
            "research_note",
            "n1",
            json!({"id": "n1", "title": "OAuth vs JWT", "excerpt": "comparing login flow options"}),
        )
        .await;
        add(
            &m,
            "decision",
            "d1",
            json!({"id": "d1", "title": "use JWT", "context": "session cookies rejected", "decision": "JWT with short expiry"}),
        )
        .await;
        m
    }

    #[tokio::test]
    async fn task_context_matches_on_terms_not_the_whole_phrase() {
        // Before: needle = the entire lowercased description, one .contains().
        // "fix the JWT login flow" matched only text containing that exact
        // 22-char phrase — i.e. never — and reported greenfield.
        let m = seeded().await;
        let v = task_context_payload(&m, "p1", "fix the JWT login flow", 10)
            .await
            .unwrap();
        assert_eq!(v["result"], json!("matches"), "{v}");

        let labels: Vec<&str> = v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["label"].as_str())
            .collect();
        assert!(labels.contains(&"JWT login"), "{labels:?}");
        assert!(labels.contains(&"OAuth vs JWT"), "{labels:?}");
        assert!(
            !labels.contains(&"CSV export"),
            "unrelated work must stay out: {labels:?}"
        );
    }

    #[tokio::test]
    async fn a_match_reports_its_score_and_the_terms_that_matched() {
        // The agent has to judge relevance itself · a bare list of hits with no
        // reason is a claim it cannot check.
        let m = seeded().await;
        let v = task_context_payload(&m, "p1", "fix the JWT login flow", 10)
            .await
            .unwrap();
        let top = &v["matches"][0];
        let terms: Vec<&str> = top["matched_terms"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(terms.contains(&"jwt"), "{terms:?}");
        let score = top["score"].as_f64().expect("score reported");
        assert!(score > 0.0 && score <= 1.0, "{score}");
        assert!(v["model"].as_str().unwrap().contains("term overlap"));
    }

    #[tokio::test]
    async fn no_match_is_distinguishable_from_nothing_stored() {
        // ! Two different facts. "we have prior work, none of it relates" vs
        // "this project is empty" lead to opposite next actions.
        let empty = fresh().await;
        let v = task_context_payload(&empty, "p1", "billing reconciliation", 10)
            .await
            .unwrap();
        assert_eq!(v["result"], json!("nothing_stored"));

        let seeded = seeded().await;
        let v = task_context_payload(&seeded, "p1", "kubernetes ingress certificates", 10)
            .await
            .unwrap();
        assert_eq!(v["result"], json!("no_match_above_threshold"));
        assert_eq!(v["stored"]["feature"], json!(2), "{v}");
    }

    #[tokio::test]
    async fn a_description_of_only_stopwords_is_refused() {
        // Returning every stored record for a query with no signal is worse
        // than saying the query carried none.
        let m = seeded().await;
        let e = task_context_payload(&m, "p1", "the and for with", 10)
            .await
            .expect_err("no searchable terms must refuse");
        assert!(e.contains("no searchable terms"), "{e}");
    }

    #[tokio::test]
    async fn an_unreadable_scope_is_not_reported_as_no_prior_work() {
        let m = seeded().await;
        m.pool().close().await;
        task_context_payload(&m, "p1", "JWT login flow", 10)
            .await
            .expect_err("a read failure must surface, ✗ read as greenfield");
    }

    #[test]
    fn a_single_shared_term_is_not_enough_on_a_long_query() {
        assert!(!meets_floor(1, 5));
        assert!(meets_floor(2, 5));
        assert!(meets_floor(1, 2), "short queries have no spare terms");
    }

    #[test]
    fn terms_are_matched_whole_not_as_substrings() {
        // "auth" ✗ matching "author" · substring matching made every short
        // term hit something.
        let q = vec!["auth".to_owned()];
        assert!(matched_terms(&q, "the author of the paper").is_empty());
        assert_eq!(matched_terms(&q, "auth service"), vec!["auth".to_owned()]);
    }
}

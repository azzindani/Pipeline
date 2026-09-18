//! `pipeline_meta` handler · explain · version · self_check · config get/set.

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "version" => ToolResponse::ok(json!({
            "pipeline_mcp": crate::VERSION,
            "pipeline_core": pipeline_core::VERSION,
            "pipeline_config": pipeline_config::VERSION,
            "pipeline_memory": pipeline_memory::VERSION,
            "pipeline_stages": pipeline_stages::VERSION,
        })),
        "self_check" => self_check().await,
        "health" => health(state.clone()).await,
        "audit" => audit(state.clone()).await,
        "review_brief" => review_brief(&req.args),
        "explain" => explain(&req.args),
        "config_get" => config_get(&req.args).await,
        "config_set" => config_set(&req.args).await,
        other => ToolResponse {
            ok: false,
            data: json!({}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some(format!("unknown action 'pipeline_meta.{other}'")),
        },
    }
}

/// Read the live config · `pipeline.yaml`, ✗ a side file.
///
/// ! get and set must address the same document. They used to share
/// `.pipeline/config.json`, which nothing else in the tree ever read, so the pair
/// was internally consistent and externally inert.
async fn config_get(args: &Value) -> ToolResponse {
    let key = args.get("key").and_then(Value::as_str);
    let (path, text) = match read_config().await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let cfg = match pipeline_config::PipelineConfig::parse(&text) {
        Ok(c) => c,
        Err(e) => return err(format!("parse {}: {e}", path.display())),
    };
    let blob = serde_json::to_value(&cfg).unwrap_or(json!({}));
    let data = match key {
        Some(k) => {
            if lookup_key(&SETTABLE, k).is_none() && blob.get(k).is_none() {
                return err(unknown_key(k));
            }
            json!({"key": k, "value": lookup(&blob, k), "file": path.display().to_string()})
        }
        None => json!({"config": blob, "file": path.display().to_string()}),
    };
    ToolResponse::ok(data)
}

/// The keys `config_set` accepts · every one is a field of `PipelineConfig`.
///
/// ! An allowlist, ✗ a free-form merge. A typo'd key used to become a brand-new
/// field nobody reads, and the caller was told `ok:true`.
const SETTABLE: [(&str, Kind); 19] = [
    ("project", Kind::Str),
    ("version", Kind::Str),
    ("stack.runtime", Kind::Str),
    ("stack.services", Kind::StrList),
    ("stages.fast", Kind::StrList),
    ("stages.full", Kind::StrList),
    ("stages.preflight", Kind::StrList),
    ("gates.coverage", Kind::Percent),
    ("gates.image_size_mb", Kind::Uint),
    ("gates.critical_vulns", Kind::Uint),
    ("deploy.registry", Kind::Str),
    ("maintenance.schedule", Kind::Str),
    ("maintenance.auto_merge", Kind::Bool),
    ("maintenance.notify_on_fail", Kind::Bool),
    ("standards.source", Kind::Str),
    ("standards.pin", Kind::Str),
    ("standards.project_type", Kind::Str),
    ("standards.surfaces", Kind::StrList),
    ("standards.languages", Kind::StrList),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Bool,
    Uint,
    /// `gates.coverage` is a u8 percentage · 300 would fail deserialization *after*
    /// the file was rewritten, so it is rejected before the write.
    Percent,
    StrList,
}

fn lookup_key(table: &[(&str, Kind)], key: &str) -> Option<Kind> {
    table.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

fn unknown_key(key: &str) -> String {
    let known: Vec<&str> = SETTABLE.iter().map(|(k, _)| *k).collect();
    // Nearest match by shared prefix · a typo is almost always in the leaf.
    let hint = known
        .iter()
        .find(|k| k.split('.').next() == key.split('.').next())
        .map_or(String::new(), |k| format!(" · did you mean '{k}'?"));
    format!(
        "unknown config key '{key}'{hint} · settable: {}",
        known.join(" · ")
    )
}

/// Change the live config · surgical YAML edit that preserves comments.
///
/// Modeled on `handlers::standards::write_pin`: a serde round-trip would reformat
/// the whole document and delete every comment a human wrote. Two guards run
/// before the file is replaced — the result must parse, and the key must read back
/// as the value that was asked for.
async fn config_set(args: &Value) -> ToolResponse {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) => k.to_owned(),
        None => return err("missing 'key'".into()),
    };
    let value = match args.get("value") {
        Some(v) => v.clone(),
        None => return err("missing 'value'".into()),
    };
    let (path, text) = match read_config().await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let (updated, previous) = match apply_config_set(&text, &key, &value) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if let Err(e) = tokio::fs::write(&path, &updated).await {
        return err(format!("write {}: {e}", path.display()));
    }
    ToolResponse {
        ok: true,
        data: json!({
            "key": key,
            "value": value,
            "previous": previous,
            "file": path.display().to_string(),
        }),
        next_suggested: vec!["pipeline_run.stage(fast)".into()],
        memory_refs: vec![],
        error: None,
    }
}

/// Validate → edit → verify. Pure: returns the document to write and the value
/// that was there before. Every refusal happens here, before anything is written.
fn apply_config_set(text: &str, key: &str, value: &Value) -> Result<(String, Value), String> {
    let Some(kind) = lookup_key(&SETTABLE, key) else {
        return Err(unknown_key(key));
    };
    check_type(kind, value).map_err(|e| format!("'{key}': {e}"))?;
    let previous = pipeline_config::PipelineConfig::parse(text)
        .ok()
        .and_then(|c| serde_json::to_value(&c).ok())
        .map_or(Value::Null, |b| lookup(&b, key));

    let updated = set_yaml_key(text, key, &render(value));
    // ! never write a pipeline.yaml we can no longer parse
    let cfg = pipeline_config::PipelineConfig::parse(&updated)
        .map_err(|e| format!("refusing to write · result would not parse: {e}"))?;
    // ! …and never report a change that did not land. An edit that hit the wrong
    // block still parses, and would be exactly the lie this action existed to tell.
    let readback = serde_json::to_value(&cfg).map_or(Value::Null, |b| lookup(&b, key));
    if readback != *value {
        return Err(format!(
            "refusing to write · '{key}' would read back as {readback} not {value}"
        ));
    }
    Ok((updated, previous))
}

async fn read_config() -> Result<(std::path::PathBuf, String), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let path = cwd.join("pipeline.yaml");
    let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
        format!(
            "read {}: {e} · run pipeline_project.init to create one",
            path.display()
        )
    })?;
    Ok((path, text))
}

fn check_type(kind: Kind, v: &Value) -> Result<(), String> {
    let ok = match kind {
        Kind::Str => v.is_string(),
        Kind::Bool => v.is_boolean(),
        Kind::Uint => v.is_u64(),
        Kind::Percent => v.as_u64().is_some_and(|n| n <= 100),
        Kind::StrList => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
    };
    if ok {
        return Ok(());
    }
    Err(match kind {
        Kind::Str => "expects a string".into(),
        Kind::Bool => "expects a boolean".into(),
        Kind::Uint => "expects a non-negative integer".into(),
        Kind::Percent => "expects an integer percentage 0-100".into(),
        Kind::StrList => "expects an array of strings".into(),
    })
}

/// Render a JSON value as YAML · flow style, which is a superset of JSON.
fn render(v: &Value) -> String {
    match v {
        Value::String(s) => render_scalar(s),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(render).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.to_string(),
    }
}

/// Quote only when the plain form would parse as something other than this string.
/// pipeline.yaml is hand-edited · gratuitous quotes are noise in every diff.
fn render_scalar(s: &str) -> String {
    let plain = !s.is_empty()
        && !s.starts_with([
            '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`', '-', '?', ':', ' ', '[', '{',
            ',',
        ])
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.ends_with(' ')
        && !matches!(
            s.to_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        )
        && s.parse::<f64>().is_err();
    if plain {
        s.to_owned()
    } else {
        // JSON string syntax is valid YAML double-quoted style.
        serde_json::to_string(s).unwrap_or_else(|_| format!("\"{s}\""))
    }
}

fn lookup(root: &Value, key: &str) -> Value {
    let mut cur = root;
    for part in key.split('.') {
        match cur.get(part) {
            Some(v) => cur = v,
            None => return Value::Null,
        }
    }
    cur.clone()
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Set a dotted key, rewriting one line and leaving every other byte alone.
fn set_yaml_key(text: &str, key: &str, rendered: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    match key.split_once('.') {
        None => set_top_level(&mut lines, key, rendered),
        Some((parent, child)) => set_child(&mut lines, parent, child, rendered),
    }
    lines.join("\n") + "\n"
}

fn set_top_level(lines: &mut Vec<String>, key: &str, rendered: &str) {
    let head = format!("{key}:");
    if let Some(i) = lines
        .iter()
        .position(|l| indent_of(l) == 0 && l.trim_end().starts_with(&head))
    {
        lines[i] = format!("{key}: {rendered}");
        drop_continuation(lines, i);
        return;
    }
    lines.push(format!("{key}: {rendered}"));
}

/// Place `child` inside the top-level `parent:` block · append the block if absent.
///
/// The block ends at the next top-level key. A comment does ✗ end it — trailing
/// comments belong to the block above them, and inserting past one strands the key.
fn set_child(lines: &mut Vec<String>, parent: &str, child: &str, rendered: &str) {
    let head = format!("{parent}:");
    let Some(start) = lines
        .iter()
        .position(|l| indent_of(l) == 0 && l.trim_end() == head)
    else {
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(head);
        lines.push(format!("  {child}: {rendered}"));
        return;
    };

    let mut last_content = start;
    let mut child_indent = 2;
    let mut existing = None;
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim();
        let indent = indent_of(line);
        if indent == 0 && !trimmed.is_empty() && !trimmed.starts_with('#') {
            break; // next top-level key · block is over
        }
        if trimmed.is_empty() || indent == 0 {
            continue; // blank, or a top-level comment · not block content
        }
        if last_content == start {
            child_indent = indent; // adopt the block's own indentation
        }
        last_content = i;
        if indent == child_indent && trimmed.starts_with(&format!("{child}:")) {
            existing = Some(i);
        }
    }

    let pad = " ".repeat(child_indent);
    match existing {
        Some(i) => {
            lines[i] = format!("{pad}{child}: {rendered}");
            drop_continuation(lines, i);
        }
        None => lines.insert(last_content + 1, format!("{pad}{child}: {rendered}")),
    }
}

/// Remove the lines that belonged to a replaced key.
///
/// ! A scalar written over a block list would otherwise leave the old `- item`
/// lines orphaned under the new value — a file that no longer parses, or worse,
/// one that parses as something nobody asked for.
fn drop_continuation(lines: &mut Vec<String>, at: usize) {
    let base = indent_of(&lines[at]);
    let mut end = at + 1;
    while end < lines.len() {
        let l = &lines[end];
        if l.trim().is_empty() || indent_of(l) <= base {
            break;
        }
        end += 1;
    }
    lines.drain(at + 1..end);
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

async fn self_check() -> ToolResponse {
    let cargo = which("cargo").await;
    let docker = which("docker").await;
    let git = which("git").await;
    let rustc = which("rustc").await;
    ToolResponse::ok(json!({
        "cargo": cargo,
        "rustc": rustc,
        "docker": docker,
        "git": git,
        "tools_registered": crate::registry().len(),
        // ! `found: false` is not a fault on the remote server. Its image ships git
        // + curl only, on purpose: every action that needs cargo · rustc · docker
        // executes code or containers, and PIPELINE_REMOTE_MODE=read_only blocks
        // all of them. Installing them would arm actions the gate exists to stop.
        "note": "cargo · rustc · docker are needed only by actions that execute \
                 (run.stage · test.* · docker.build|run · docs.update_from_code); \
                 the remote read_only image omits them by design · git serves every \
                 read_only action that shells out",
    }))
}

async fn which(program: &str) -> Value {
    use tokio::process::Command;
    match Command::new(program).arg("--version").output().await {
        Ok(o) if o.status.success() => json!({
            "found": true,
            "version": String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_owned()
        }),
        _ => json!({"found": false}),
    }
}

fn explain(args: &Value) -> ToolResponse {
    let topic = args.get("topic").and_then(Value::as_str).unwrap_or("");
    let text = match topic {
        "" | "pipeline" => {
            "Pipeline is a local-first, MCP-native CI/CD orchestrator. See CLAUDE.md."
        }
        "stages" => {
            "Five stages: static · unit · container · integration · security. Profiles: fast · full · preflight · confirm."
        }
        "memory" => {
            "SQLite at .pipeline/memory.db · projects · sessions · pipeline_runs · failures · memory_kv tables."
        }
        "tools" => "19 super tools dispatching by `action`. See PLAN.md §3.",
        _ => "Unknown topic. Try: pipeline · stages · memory · tools.",
    };
    ToolResponse::ok(json!({"topic": topic, "text": text}))
}

// ── health + audit ──────────────────────────────────────────────────────────
//
// ! Both GATHER · ✗ judge. Pipeline reports what is true about the project and
// names what is missing; whether that is acceptable is the caller's decision.
// A tool that returns a verdict on code quality is guessing, and an agent that
// trusts the guess stops looking.
//
// `health` answers "what shape is this in right now" in one call · `audit` is
// the slower, wider pass that names every gap against the loaded standards.

/// Fast project health · designed to be the first call of a session.
async fn health(state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };

    let runs = mem.run_history(&cfg.project, 20).await.unwrap_or_default();
    let last = runs.first();
    // ! Age matters as much as outcome. A green run from three weeks ago says
    // nothing about the tree as it stands now, and reporting only "pass" would
    // let a stale success read as a current one.
    let last_run_age_days = last.and_then(|r| days_since(&r.created_at));
    let consecutive_failures = runs.iter().take_while(|r| r.status != "pass").count();

    let dirty = git_lines(&["status", "--porcelain"]).map(|l| l.len());
    let branch = git_lines(&["rev-parse", "--abbrev-ref", "HEAD"]).and_then(|l| l.first().cloned());

    let mut concerns: Vec<String> = Vec::new();
    match last {
        None => concerns.push("no run recorded · nothing here has been verified".into()),
        Some(r) if r.status != "pass" => {
            concerns.push(format!("last run failed at stage '{}'", r.stage));
        }
        _ => {}
    }
    if last_run_age_days.is_some_and(|d| d >= 7) {
        concerns.push(format!(
            "last run is {} days old · its result describes an older tree",
            last_run_age_days.unwrap_or_default()
        ));
    }
    if consecutive_failures >= 3 {
        concerns.push(format!(
            "{consecutive_failures} consecutive failures · the loop is not converging"
        ));
    }
    if dirty.is_some_and(|n| n > 0) {
        concerns.push(format!(
            "{} uncommitted file(s) · a run measures the tree, ✗ the commit",
            dirty.unwrap_or_default()
        ));
    }

    ToolResponse::ok(json!({
        "project": cfg.project,
        "branch": branch,
        "uncommitted_files": dirty,
        "last_run": last.map(|r| json!({
            "profile": r.profile,
            "stage": r.stage,
            "status": r.status,
            "age_days": last_run_age_days,
        })),
        "consecutive_failures": consecutive_failures,
        "concerns": concerns,
        "verdict": if concerns.is_empty() { "no concerns found" } else { "see concerns" },
    }))
}

/// Wider pass · what this project is missing against the standards it loads.
async fn audit(state: Arc<ServerState>) -> ToolResponse {
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };

    let findings = audit_findings(&cfg, &mem).await;
    let high = findings.iter().filter(|f| f["severity"] == "high").count();
    ToolResponse::ok(json!({
        "project": cfg.project,
        "findings": findings,
        "high": high,
        "total": findings.len(),
        // ! Reported, ✗ judged. Whether these gaps are acceptable is the
        // caller's call — Pipeline names them.
        "note": "gaps found, ✗ a quality verdict · severity ranks attention, not acceptability",
    }))
}

/// One project's audit findings, severity-sorted.
///
/// ! Split out so `pipeline_repo.fleet_health` audits every registered repo
/// through THIS function rather than its own copy of the rules. A second
/// implementation drifts, and a fleet view that grades a tree differently from
/// `pipeline_meta.audit` on the same tree is worse than no fleet view.
pub(crate) async fn audit_findings(
    cfg: &pipeline_config::PipelineConfig,
    mem: &pipeline_memory::Memory,
) -> Vec<Value> {
    let mut findings: Vec<Value> = Vec::new();
    let mut note = |area: &str, severity: &str, detail: String| {
        findings.push(json!({ "area": area, "severity": severity, "detail": detail }));
    };

    // Gates declared but never measured are the defect this session found in
    // Pipeline itself: a gate that never runs provides nothing and reads as
    // protection.
    if cfg.gates.coverage.is_none() {
        note("gates", "medium", "no coverage gate declared".into());
    }
    if cfg.gates.critical_vulns.is_none() {
        note(
            "gates",
            "medium",
            "no critical-vulnerability gate declared".into(),
        );
    }

    // Standards binding.
    match (&cfg.standards.source, &cfg.standards.pin) {
        (_, None) => note(
            "standards",
            "high",
            "no standards pin · upstream can move a gate under you without a commit".into(),
        ),
        (Some(src), _) if src.starts_with('/') => note(
            "standards",
            "medium",
            format!("standards.source '{src}' is an absolute path · portable to no other machine"),
        ),
        _ => {}
    }
    if cfg.standards.project_type.is_none() {
        note(
            "standards",
            "low",
            "no project_type · only always-on and language routes load".into(),
        );
    }

    let runs = mem.run_history(&cfg.project, 200).await.unwrap_or_default();
    let evidence_gaps = stage_evidence_gaps(&runs);

    // Tracked work · untracked work is invisible debt (workflow §8).
    let tasks_raw = mem
        .recall(&cfg.project, "task", "list")
        .await
        .ok()
        .flatten();
    let task_count = tasks_raw
        .and_then(|raw| serde_json::from_str::<Vec<Value>>(&raw).ok())
        .map_or(0, |t| t.len());
    if task_count == 0 {
        note(
            "workflow",
            "low",
            "no tracked tasks · untracked work is invisible debt".into(),
        );
    }

    // Learning loop · a project that never records a fix cannot surface one.
    let fixes_recorded = runs.is_empty()
        || mem
            .recall(&cfg.project, "progress", "tracker")
            .await
            .ok()
            .flatten()
            .is_some();
    if !fixes_recorded {
        note(
            "memory",
            "low",
            "no progress tracker · a context reset loses the thread".into(),
        );
    }

    findings.extend(evidence_gaps);
    // Severity first, so the caller reads the loudest gap without sorting.
    findings.sort_by_key(|f| match f["severity"].as_str() {
        Some("high") => 0,
        Some("medium") => 1,
        _ => 2,
    });

    findings
}

/// Stage evidence gaps · a stage that never ran and one that ran red are both
/// absent evidence.
///
/// ! Split out of `audit` to keep it readable, ✗ because it is separable: the
/// never-ran and ran-red branches must stay adjacent, since treating either as
/// a pass is the same mistake.
fn stage_evidence_gaps(runs: &[pipeline_memory::RunRecord]) -> Vec<Value> {
    // Runs arrive newest-first, so the first sighting of a stage is its latest.
    let mut latest: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for r in runs {
        latest.entry(r.stage.as_str()).or_insert(r.status.as_str());
    }
    let mut out = Vec::new();
    for required in ["static", "unit", "container", "integration"] {
        let severity = if matches!(required, "static" | "unit") {
            "high"
        } else {
            "medium"
        };
        let detail = match latest.get(required) {
            None => format!("stage '{required}' has never run · absent evidence, ✗ a pass"),
            Some(&status) if status != "pass" => {
                format!("stage '{required}' last ran and FAILED · a red stage is not evidence")
            }
            _ => continue,
        };
        out.push(json!({ "area": "evidence", "severity": severity, "detail": detail }));
    }
    out
}

/// Assemble the material a reviewer needs · ✗ perform the review.
///
/// ! `code_review/STANDARDS.md` §12 is explicit: AI findings are suggestions,
/// ✗ verdicts, and AI cannot approve. So this gathers — diff, size against the
/// §2 thresholds, sensitive paths — and returns it for the reviewer to judge.
/// A tool that returned findings here would be the thing §12 forbids, wearing a
/// tool's authority.
///
/// ! Sensitive paths are flagged for HUMAN-ONLY review (§12), ✗ merely noted.
/// Auth, crypto and financial logic is where an AI reviewer misses subtle flaws
/// most expensively.
fn review_brief(args: &Value) -> ToolResponse {
    // code_review §2 · owner of PR size across the repo.
    const LINES_TARGET: usize = 400;
    const LINES_LIMIT: usize = 800;
    const FILES_TARGET: usize = 10;
    const FILES_LIMIT: usize = 20;

    let base = args
        .get("base")
        .and_then(Value::as_str)
        .unwrap_or("origin/main");
    if let Err(e) = super::refuse_git_option("base", base) {
        return err(e);
    }

    let Some(stat) = git_lines(&["diff", "--numstat", &format!("{base}...HEAD")]) else {
        return err(format!(
            "cannot diff against '{base}' · not a git repository, | the ref does not exist"
        ));
    };

    let mut files: Vec<Value> = Vec::new();
    let mut total_lines = 0usize;
    // Generated and lock files are excluded from the size judgement by §2, and
    // listed separately so their absence from the count is visible rather than
    // silent.
    let mut excluded: Vec<String> = Vec::new();
    for line in &stat {
        let cols: Vec<&str> = line.split('\t').collect();
        let [added, removed, path] = cols[..] else {
            continue;
        };
        let changed = added.parse::<usize>().unwrap_or(0) + removed.parse::<usize>().unwrap_or(0);
        if is_size_exempt(path) {
            excluded.push(path.to_owned());
            continue;
        }
        total_lines += changed;
        files.push(json!({ "path": path, "added": added, "removed": removed }));
    }

    let sensitive: Vec<&str> = files
        .iter()
        .filter_map(|f| f["path"].as_str())
        .filter(|p| is_sensitive(p))
        .collect();

    let mut size_notes: Vec<String> = Vec::new();
    if total_lines > LINES_LIMIT {
        size_notes.push(format!(
            "{total_lines} lines changed · over the {LINES_LIMIT} hard limit · split before review"
        ));
    } else if total_lines > LINES_TARGET {
        size_notes.push(format!(
            "{total_lines} lines changed · over the {LINES_TARGET} target · review will be slower and worse"
        ));
    }
    if files.len() > FILES_LIMIT {
        size_notes.push(format!(
            "{} files · over the {FILES_LIMIT} hard limit",
            files.len()
        ));
    } else if files.len() > FILES_TARGET {
        size_notes.push(format!(
            "{} files · over the {FILES_TARGET} target",
            files.len()
        ));
    }

    let commits = git_lines(&["rev-list", "--count", &format!("{base}..HEAD")])
        .and_then(|l| l.first().and_then(|n| n.parse::<usize>().ok()));
    if commits.is_some_and(|c| c > 10) {
        size_notes.push(format!(
            "{} commits · over the 10 limit · each commit should be one logical unit",
            commits.unwrap_or_default()
        ));
    }

    ToolResponse {
        ok: true,
        data: json!({
            "base": base,
            "files": files,
            "files_changed": files.len(),
            "lines_changed": total_lines,
            "commits": commits,
            "size_exempt": excluded,
            "size_notes": size_notes,
            // ! Named, ✗ reviewed. §12 puts these outside an AI reviewer's remit.
            "human_only_paths": sensitive,
            "checklist": "pipeline_standards.checklist · 15 rows mapped to their owning standard",
            "note": "material for a review, ✗ a review · findings are the reviewer's to make (code_review §12)",
        }),
        next_suggested: vec!["pipeline_standards.checklist".into()],
        memory_refs: vec![],
        error: None,
    }
}

/// Paths excluded from the size judgement by `code_review` §2.
fn is_size_exempt(path: &str) -> bool {
    const LOCKS: [&str; 6] = [
        "Cargo.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "poetry.lock",
        "uv.lock",
    ];
    LOCKS.iter().any(|l| path.ends_with(l))
        || path.contains("/generated/")
        || path.contains("/migrations/")
        || std::path::Path::new(path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("snap"))
}

/// Code where `code_review` §12 requires a human reviewer.
///
/// ! Deliberately broad. A false positive costs one human read; a false
/// negative sends auth or crypto through AI-only review, which is the outcome
/// §12 exists to prevent.
fn is_sensitive(path: &str) -> bool {
    const MARKERS: [&str; 12] = [
        "auth",
        "oauth",
        "token",
        "secret",
        "crypt",
        "password",
        "session",
        "permission",
        "payment",
        "billing",
        "invoice",
        "ledger",
    ];
    let lower = path.to_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Run a git command, returning its stdout lines · `None` outside a repo.
fn git_lines(args: &[&str]) -> Option<Vec<String>> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn days_since(ts: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some((chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_days())
}

#[cfg(test)]
mod config_tests {
    use super::*;

    /// A config with the two things a surgical editor must not destroy: comments
    /// and a block list.
    const YAML: &str = "# Pipeline config for vera · hand written, keep it that way\nproject: vera\nversion: 0.1.0\n\nstack:\n  runtime: rust\n  services:\n    - postgres:16\n    - redis:7\n\n# The gate the runner enforces\ngates:\n  coverage: 70\n  image_size_mb: 200\n\nstandards:\n  languages: [rust]\n";

    fn set(text: &str, key: &str, value: &Value) -> Result<String, String> {
        apply_config_set(text, key, value).map(|(t, _)| t)
    }

    #[test]
    fn config_set_changes_the_gate_the_runner_actually_reads() {
        // ! The assertion is on PipelineConfig — the exact type the stage runner
        // loads. Writing a file that nothing parses back was the original defect.
        let out = set(YAML, "gates.coverage", &json!(95)).expect("set");
        let cfg = pipeline_config::PipelineConfig::parse(&out).expect("parses");
        assert_eq!(cfg.gates.coverage, Some(95));
        assert_eq!(cfg.gates.image_size_mb, Some(200), "sibling key untouched");
    }

    #[test]
    fn config_set_preserves_comments() {
        let out = set(YAML, "gates.coverage", &json!(95)).expect("set");
        assert!(out.contains("# Pipeline config for vera · hand written, keep it that way"));
        assert!(out.contains("# The gate the runner enforces"));
        // …and only the one line moved.
        let before = YAML.lines().count();
        assert_eq!(
            out.lines().count(),
            before,
            "no lines added or lost:\n{out}"
        );
        assert!(out.contains("  coverage: 95"));
        assert!(!out.contains("coverage: 70"));
    }

    #[test]
    fn an_unknown_config_key_is_an_error_not_a_new_field() {
        // Regression: any key at all was accepted and merged into a side file, so
        // `gates.covrage` reported ok:true and changed no gate anywhere.
        let e = set(YAML, "gates.covrage", &json!(80)).expect_err("must refuse");
        assert!(e.contains("unknown config key"), "{e}");
        assert!(
            e.contains("gates.coverage"),
            "should suggest the real key: {e}"
        );
        // Nothing was produced to write.
        assert!(set(YAML, "wat", &json!(1)).is_err());
        assert!(set(YAML, "gates.coverage.deep", &json!(1)).is_err());
    }

    #[test]
    fn a_wrongly_typed_value_is_refused_before_the_file_is_touched() {
        assert!(set(YAML, "gates.coverage", &json!("high")).is_err());
        assert!(
            set(YAML, "gates.coverage", &json!(300)).is_err(),
            "u8 overflow"
        );
        assert!(set(YAML, "stack.services", &json!("postgres")).is_err());
        assert!(set(YAML, "maintenance.auto_merge", &json!("yes")).is_err());
    }

    #[test]
    fn replacing_a_block_list_removes_the_old_items() {
        // ! A scalar or flow list written over `- item` lines would otherwise leave
        // them orphaned under the new value.
        let out = set(YAML, "stack.services", &json!(["postgres:17"])).expect("set");
        let cfg = pipeline_config::PipelineConfig::parse(&out).expect("parses");
        assert_eq!(cfg.stack.services, vec!["postgres:17".to_owned()]);
        assert!(!out.contains("redis:7"), "stale list item survived:\n{out}");
    }

    #[test]
    fn a_missing_block_is_created_rather_than_silently_skipped() {
        let out = set(YAML, "maintenance.schedule", &json!("0 9 * * 1")).expect("set");
        let cfg = pipeline_config::PipelineConfig::parse(&out).expect("parses");
        assert_eq!(
            cfg.maintenance.and_then(|m| m.schedule).as_deref(),
            Some("0 9 * * 1")
        );
    }

    #[test]
    fn a_scalar_is_quoted_only_when_the_bare_form_would_mean_something_else() {
        // ! `version: 1.0` is a float, `project: true` is a bool · both would read
        // back as the wrong type and trip the verification guard. Quoting is by
        // necessity, ✗ by default: pipeline.yaml is hand-edited.
        let quoted = set(YAML, "version", &json!("1.0")).expect("set");
        assert!(quoted.contains("version: \"1.0\""), "{quoted}");
        let bare = set(YAML, "stack.runtime", &json!("python-uv")).expect("set");
        assert!(bare.contains("runtime: python-uv"), "{bare}");
        let colon = set(
            YAML,
            "standards.source",
            &json!("git@github.com:me/std.git"),
        )
        .expect("set");
        let cfg = pipeline_config::PipelineConfig::parse(&colon).expect("parses");
        assert_eq!(
            cfg.standards.source.as_deref(),
            Some("git@github.com:me/std.git")
        );
    }

    #[test]
    fn a_top_level_scalar_is_replaced_in_place() {
        let out = set(YAML, "project", &json!("vera-core")).expect("set");
        let cfg = pipeline_config::PipelineConfig::parse(&out).expect("parses");
        assert_eq!(cfg.project, "vera-core");
        assert_eq!(out.matches("project:").count(), 1, "must not duplicate");
    }

    #[test]
    fn setting_reports_the_value_it_replaced() {
        let (_, previous) = apply_config_set(YAML, "gates.coverage", &json!(95)).expect("set");
        assert_eq!(previous, json!(70));
    }

    #[test]
    fn every_settable_key_round_trips_through_the_schema() {
        // ! The allowlist and the schema must not drift: a key listed here but not
        // present on PipelineConfig would fail the read-back guard at runtime, which
        // is a refusal the agent cannot act on.
        let samples: &[(&str, Value)] = &[
            ("project", json!("p")),
            ("version", json!("9.9.9")),
            ("stack.runtime", json!("python-uv")),
            ("stack.services", json!(["redis:7"])),
            ("stages.fast", json!(["static", "unit"])),
            ("stages.full", json!(["static"])),
            ("stages.preflight", json!(["security"])),
            ("gates.coverage", json!(81)),
            ("gates.image_size_mb", json!(512)),
            ("gates.critical_vulns", json!(0)),
            ("deploy.registry", json!("ghcr.io/azzindani")),
            ("maintenance.schedule", json!("@weekly")),
            ("maintenance.auto_merge", json!(true)),
            ("maintenance.notify_on_fail", json!(false)),
            ("standards.source", json!("/opt/Standards")),
            ("standards.pin", json!("abc1234")),
            ("standards.project_type", json!("MCP server")),
            ("standards.surfaces", json!(["Command line"])),
            ("standards.languages", json!(["rust", "go"])),
        ];
        assert_eq!(samples.len(), SETTABLE.len(), "a key lost its coverage");
        for (key, value) in samples {
            let out =
                set(YAML, key, value).unwrap_or_else(|e| panic!("set '{key}' = {value}: {e}"));
            let cfg = pipeline_config::PipelineConfig::parse(&out)
                .unwrap_or_else(|e| panic!("'{key}' produced unparseable yaml: {e}"));
            let blob = serde_json::to_value(&cfg).expect("serialize");
            assert_eq!(&lookup(&blob, key), value, "'{key}' did not land");
        }
    }
}

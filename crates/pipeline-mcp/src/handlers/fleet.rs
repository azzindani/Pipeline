//! Fleet health · one pass over every registered repo.
//!
//! kind: component
//!
//! ! The reason this exists is maintenance of repos Pipeline did NOT scaffold.
//! Checking ten existing projects one at a time costs ten `health` calls, ten
//! `audit` calls and ten `report.maturity` calls, each needing the server to be
//! sitting inside that repo. This walks the registry instead and reports every
//! repo from wherever the server runs.
//!
//! ! GATHERS · ✗ judges, exactly as `pipeline_meta.audit` does. `attention` is
//! an ordering so the caller reads the loudest repo first — ✗ a score, ✗ a
//! grade, ✗ a claim that a repo at the bottom is fine.
//!
//! ! Every per-repo number comes from the SAME functions the single-project
//! actions use: [`crate::handlers::meta::audit_findings`] and
//! [`crate::handlers::report::maturity_of`]. ✗ reimplement a rule here. A fleet
//! view that grades a tree differently from `pipeline_meta.audit` on that tree
//! is worse than no fleet view.

use crate::handlers::repo::RegistryEntry;
use crate::tools::ToolResponse;
use serde_json::{Value, json};
use std::path::Path;

/// Run history depth · matches `pipeline_meta.health`, so the two agree on what
/// "consecutive failures" means.
const RUN_DEPTH: i64 = 20;

/// A run older than this describes a tree that has since moved on.
const STALE_DAYS: i64 = 7;

/// Row states · one field, three mutually exclusive values.
///
/// ! `missing` and `unmanaged` are different problems with different fixes —
/// a wrong path in the registry versus a project that has not been adopted yet.
/// ✗ collapse them back into one boolean.
const STATE_MISSING: &str = "missing";
const STATE_UNMANAGED: &str = "unmanaged";
const STATE_MANAGED: &str = "managed";

pub(crate) async fn fleet_health(repos: Vec<RegistryEntry>) -> ToolResponse {
    if repos.is_empty() {
        return ToolResponse {
            ok: true,
            data: json!({
                "repos": [],
                "total": 0,
                "note": "no repos registered · call pipeline_repo.register first",
            }),
            next_suggested: vec!["pipeline_repo.register".into()],
            memory_refs: vec![],
            error: None,
        };
    }

    let mut rows: Vec<Value> = Vec::new();
    for entry in &repos {
        rows.push(scan(entry).await);
    }

    // Loudest first. Ties keep registry order, which is insertion order, so a
    // repeat call returns a stable list.
    rows.sort_by(|a, b| {
        let rank = |v: &Value| v["attention"].as_array().map_or(0, Vec::len);
        rank(b).cmp(&rank(a))
    });

    let needs_attention = rows
        .iter()
        .filter(|r| r["attention"].as_array().is_some_and(|a| !a.is_empty()))
        .count();
    // ! Counted off `state`, ✗ off a `managed` boolean. A repo that is missing
    // from disk is also not managed, so a single boolean folded the two
    // together and reported a registry typo as an un-adopted project.
    let count = |s: &str| rows.iter().filter(|r| r["state"] == s).count();

    ToolResponse {
        ok: true,
        data: json!({
            "repos": rows,
            "total": repos.len(),
            "needs_attention": needs_attention,
            "unmanaged": count(STATE_UNMANAGED),
            "missing": count(STATE_MISSING),
            "note": "gathered, ✗ graded · attention orders reading, it does not rank quality",
        }),
        next_suggested: vec![
            "pipeline_meta.audit".into(),
            "pipeline_run.execute".into(),
            "pipeline_project.init".into(),
        ],
        memory_refs: vec![],
        error: None,
    }
}

/// Scan one repo. Never fails the whole pass — a repo that cannot be read
/// reports why, in its own row.
///
/// ! Absent is reported as absent, ✗ omitted. A row with `last_run: null` and
/// `maturity: null` says "nothing here has been measured", which is the single
/// most useful thing a fleet view can tell you about an adopted repo.
async fn scan(entry: &RegistryEntry) -> Value {
    let root = crate::handlers::repo::repo_root_of(entry);
    let mut attention: Vec<String> = Vec::new();

    if !root.is_dir() {
        attention.push(format!(
            "working tree missing at {} · registered but never cloned",
            root.display()
        ));
        return json!({
            "alias": entry.alias,
            "root": root.to_string_lossy(),
            "state": STATE_MISSING,
            "attention": attention,
        });
    }

    let cfg = pipeline_config::PipelineConfig::load(root.join("pipeline.yaml")).ok();
    let Some(cfg) = cfg else {
        // ! An unmanaged repo is the normal starting state for an existing
        // project, ✗ an error. It is reported with the one action that changes
        // it, and the scan stops there — there is no memory to read.
        attention.push(
            "no pipeline.yaml · unmanaged · pipeline_project.init with adopt=true brings it in"
                .into(),
        );
        return json!({
            "alias": entry.alias,
            "root": root.to_string_lossy(),
            "state": STATE_UNMANAGED,
            "git": git_state(&root),
            "attention": attention,
        });
    };

    let db = root.join(".pipeline").join("memory.db");
    let mem = if db.is_file() {
        pipeline_memory::Memory::open(&db).await.ok()
    } else {
        None
    };

    let Some(mem) = mem else {
        attention
            .push("no memory database · nothing recorded · run the fast profile to start".into());
        return json!({
            "alias": entry.alias,
            "root": root.to_string_lossy(),
            "state": STATE_MANAGED,
            "project": cfg.project,
            "stack": cfg.stack.runtime,
            "git": git_state(&root),
            "last_run": Value::Null,
            "maturity": Value::Null,
            "attention": attention,
        });
    };

    scan_recorded(entry, &root, &cfg, &mem, attention).await
}

/// The full row for a managed repo that has a memory database.
///
/// ! Split off `scan` for length only. Every signal here is read from that
/// repo's OWN memory and tree — ✗ from the server's working directory, which is
/// the whole reason a fleet view can exist.
async fn scan_recorded(
    entry: &RegistryEntry,
    root: &Path,
    cfg: &pipeline_config::PipelineConfig,
    mem: &pipeline_memory::Memory,
    mut attention: Vec<String>,
) -> Value {
    let runs = mem
        .run_history(&cfg.project, RUN_DEPTH)
        .await
        .unwrap_or_default();
    let last = runs.first();
    let age_days = last.and_then(|r| days_since(&r.created_at));
    let consecutive_failures = runs.iter().take_while(|r| r.status != "pass").count();

    match last {
        None => attention.push("no run recorded · nothing here has been verified".into()),
        Some(r) if r.status != "pass" => {
            attention.push(format!("last run failed at stage '{}'", r.stage));
        }
        _ => {}
    }
    if age_days.is_some_and(|d| d >= STALE_DAYS) {
        attention.push(format!(
            "last run is {} days old · its result describes an older tree",
            age_days.unwrap_or_default()
        ));
    }
    if consecutive_failures >= 3 {
        attention.push(format!(
            "{consecutive_failures} consecutive failures · the loop is not converging"
        ));
    }

    let findings = crate::handlers::meta::audit_findings(cfg, mem).await;
    let high = findings.iter().filter(|f| f["severity"] == "high").count();
    if high > 0 {
        attention.push(format!("{high} high-severity audit finding(s)"));
    }

    let maturity = crate::handlers::report::maturity_of(mem, &cfg.project, root)
        .await
        .ok();

    let tasks = task_counts(mem, &cfg.project).await;
    if tasks.blocked > 0 {
        attention.push(format!(
            "{} blocked task(s) · nobody unblocks what nobody reads",
            tasks.blocked
        ));
    }

    let git = git_state(root);
    if git.dirty > 0 {
        attention.push(format!(
            "{} uncommitted file(s) · a run measures the tree, ✗ the commit",
            git.dirty
        ));
    }

    json!({
        "alias": entry.alias,
        "root": root.to_string_lossy(),
        "state": STATE_MANAGED,
        "project": cfg.project,
        "stack": cfg.stack.runtime,
        "git": git,
        "last_run": last.map(|r| json!({
            "profile": r.profile,
            "stage": r.stage,
            "status": r.status,
            "age_days": age_days,
        })),
        "consecutive_failures": consecutive_failures,
        "maturity": maturity,
        "findings": { "high": high, "total": findings.len() },
        "tasks": { "open": tasks.open, "in_progress": tasks.in_progress, "blocked": tasks.blocked },
        "attention": attention,
    })
}

#[derive(Default)]
struct TaskCounts {
    open: usize,
    in_progress: usize,
    blocked: usize,
}

/// Task counts by status · written by `pipeline_plan.task_add` / `task_update`.
///
/// ! Statuses are matched against the same four `pipeline_plan` writes, so a
/// task whose status is none of them counts nowhere rather than silently
/// inflating `open`.
async fn task_counts(mem: &pipeline_memory::Memory, project: &str) -> TaskCounts {
    let Ok(Some(raw)) = mem.recall(project, "task", "list").await else {
        return TaskCounts::default();
    };
    let Ok(tasks) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return TaskCounts::default();
    };
    let mut c = TaskCounts::default();
    for t in &tasks {
        match t["status"].as_str() {
            Some("open") => c.open += 1,
            Some("in_progress") => c.in_progress += 1,
            Some("blocked") => c.blocked += 1,
            _ => {}
        }
    }
    c
}

#[derive(serde::Serialize)]
struct GitState {
    branch: Option<String>,
    dirty: usize,
    /// False when the directory is not a git working tree at all.
    tracked: bool,
}

/// Git facts for a repo the server is not sitting inside.
///
/// ! `-C <root>` rather than changing the process directory. The MCP server is
/// one process serving concurrent calls; a chdir here would move every other
/// in-flight handler's idea of the project out from under it.
fn git_state(root: &Path) -> GitState {
    let branch = git_in(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .and_then(|out| out.lines().next().map(str::to_owned));
    let status = git_in(root, &["status", "--porcelain"]);
    GitState {
        tracked: branch.is_some(),
        dirty: status
            .as_deref()
            .map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count()),
        branch,
    }
}

fn git_in(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn days_since(ts: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some((chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_days())
}

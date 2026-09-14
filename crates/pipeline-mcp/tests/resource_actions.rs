//! `pipeline_observe` resource actions, driven the way an agent would.
//!
//! ! The gap these close: the comparison logic in `pipeline-core` had tests and
//! no caller, so it was green and unreachable. A green test on unwired logic is
//! the most flattering kind of incomplete.
//!
//! ! One `#[test]` for the same reason `mcp_workflow` has one: handlers resolve
//! the project from the process working directory, so every chdir here is
//! process-global. Two parallel tests in this binary race, and the race shows
//! up only under the full workspace run — which is exactly how it was found.

use pipeline_mcp::{ServerState, ToolRequest, call_tool};
use serde_json::{Value, json};
use std::sync::Arc;

const PROJECT_YAML: &str = "project: resource-test
version: 0.0.1
stack:
  runtime: rust
  services: []
stages:
  fast: [static, unit]
gates:
  coverage: 70
";

async fn call(state: &Arc<ServerState>, tool: &str, action: &str, args: Value) -> Value {
    let req = ToolRequest {
        action: action.to_owned(),
        args,
    };
    let resp = call_tool(tool, req, state.clone()).await;
    assert!(
        resp.ok,
        "{tool}.{action} failed: {:?} · data {}",
        resp.error, resp.data
    );
    resp.data
}

fn enter_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pipeline.yaml"), PROJECT_YAML).expect("write config");
    std::env::set_current_dir(dir.path()).expect("chdir");
    dir
}

#[tokio::test]
async fn the_resource_actions_are_reachable_and_honest() {
    resource_measurement_records_real_numbers_and_compares_them().await;
    throttling_refuses_rather_than_mislabelling_an_unconstrained_run().await;
    maturity_is_derived_from_run_history().await;
    devtools_are_hosted_and_destructive_ones_dry_run().await;
    mode_set_records_the_project_mode().await;
    tasks_are_tracked_with_a_verifiable_done_condition().await;
    health_and_audit_report_gaps_without_grading().await;
    review_brief_gathers_material_and_defers_the_judgement().await;
    fleet_health_reports_every_registered_repo_from_one_call().await;
}

/// Resource measurement, driven the way an agent would.
///
/// ! The gap this closes: the comparison logic had tests and no caller, so it
/// was green and unreachable. This drives the actions through `call_tool` and
/// asserts real numbers come back and a second sample produces a verdict.
async fn resource_measurement_records_real_numbers_and_compares_them() {
    let _dir = enter_project();
    let state = Arc::new(ServerState::new());

    let first = call(
        &state,
        "pipeline_observe",
        "resource_measure",
        json!({"command": "sleep 0.2", "label": "probe", "work_units": 100}),
    )
    .await;

    let wall = first["record"]["wall_time_ms"].as_u64().expect("wall time");
    assert!(
        wall >= 180,
        "measured {wall} ms for a command that sleeps 200 ms"
    );
    assert_eq!(first["exit_code"].as_i64(), Some(0));
    assert_eq!(
        first["stored"].as_bool(),
        Some(true),
        "the record did not persist, so no later run can compare against it"
    );

    // One sample is not a comparison · the report must say so rather than
    // inventing a verdict.
    let lonely = call(
        &state,
        "pipeline_observe",
        "efficiency_report",
        json!({"label": "probe"}),
    )
    .await;
    assert_eq!(
        lonely["verdict"].as_str(),
        Some("insufficient-history"),
        "a single sample must not yield a comparison verdict: {lonely}"
    );

    // Second sample → a real verdict.
    call(
        &state,
        "pipeline_observe",
        "resource_measure",
        json!({"command": "sleep 0.2", "label": "probe", "work_units": 100}),
    )
    .await;

    let report = call(
        &state,
        "pipeline_observe",
        "efficiency_report",
        json!({"label": "probe"}),
    )
    .await;
    assert_eq!(report["samples"].as_u64(), Some(2));
    assert!(
        report["verdict"].is_string() || report["verdict"].is_object(),
        "expected a verdict, got {report}"
    );
}

async fn throttling_refuses_rather_than_mislabelling_an_unconstrained_run() {
    // ! A mislabelled baseline is worse than a missing one: every later
    // comparison inherits it and nothing signals the error.
    let _dir = enter_project();
    let state = Arc::new(ServerState::new());
    let resp = call_tool(
        "pipeline_observe",
        ToolRequest {
            action: "throttle_test".to_owned(),
            args: json!({"command": "true", "profile": "cpu-50pct"}),
        },
        state,
    )
    .await;

    if resp.ok {
        // cgroup v2 present · the run is genuinely constrained and labelled.
        assert_eq!(
            resp.data["record"]["constraint"].as_str(),
            Some("cpu-50pct"),
            "a successful throttle run must carry its profile"
        );
    } else {
        let why = resp.error.unwrap_or_default();
        assert!(
            why.contains("cgroup"),
            "refusal must name the missing mechanism: {why}"
        );
    }
}

/// Maturity computed from real runs, ✗ asserted.
///
/// ! Folded into this binary rather than a third one: it chdirs like the rest,
/// and a separate binary would only add another process doing the same thing.
async fn maturity_is_derived_from_run_history() {
    let _dir = enter_project();
    let state = Arc::new(ServerState::new());

    // A bare project has no evidence and must sit below level 0 — ✗ default to
    // a level because nothing has failed yet.
    let bare = call(&state, "pipeline_report", "maturity", json!({})).await;
    assert_eq!(bare["level"].as_u64(), Some(0));
    assert_eq!(bare["level_name"].as_str(), Some("below level 0"));
    assert!(
        bare["missing_for_next"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "a bare project must name what is missing: {bare}"
    );

    // Run the fast profile for real · static + unit now carry outcomes.
    // The temp project has no Cargo.toml, so the rust stages fail — which is the
    // case worth asserting: a recorded FAILURE must not become evidence.
    // `call` is bypassed because a failing run is a legitimate outcome here,
    // ✗ a broken call.
    let run = call_tool(
        "pipeline_run",
        ToolRequest {
            action: "stage".to_owned(),
            args: json!({"profile": "fast"}),
        },
        state.clone(),
    )
    .await;
    assert_eq!(
        run.data["overall"].as_str(),
        Some("fail"),
        "expected the stages to fail in a project with no manifest: {}",
        run.data
    );

    let after = call(&state, "pipeline_report", "maturity", json!({})).await;
    assert!(
        after["runs_examined"].as_u64().is_some_and(|n| n > 0),
        "no runs reached the evidence map: {after}"
    );
    assert_eq!(
        after["level"].as_u64(),
        Some(0),
        "failed stages must not raise the level: {after}"
    );
    let present = after["evidence_present"].as_array().expect("array");
    assert!(
        present.is_empty(),
        "a failing run produced positive evidence: {present:?}"
    );
}

/// Project tools · hosted, ✗ generated.
async fn devtools_are_hosted_and_destructive_ones_dry_run() {
    let _dir = enter_project();
    let state = Arc::new(ServerState::new());

    let empty = call(&state, "pipeline_project", "devtool_list", json!({})).await;
    assert_eq!(empty["count"].as_u64(), Some(0));

    // ! `destructive` is required. Omitting it must fail rather than default —
    // a tool that rewrites source and never said so is the one case where a
    // wrong default does damage.
    let undeclared = call_tool(
        "pipeline_project",
        ToolRequest {
            action: "devtool_add".to_owned(),
            args: json!({"name": "fmt", "entry": "echo formatted"}),
        },
        state.clone(),
    )
    .await;
    assert!(!undeclared.ok, "destructive must be required");

    call(
        &state,
        "pipeline_project",
        "devtool_add",
        json!({"name": "safe", "entry": "echo ran", "destructive": false}),
    )
    .await;
    call(
        &state,
        "pipeline_project",
        "devtool_add",
        json!({"name": "rewrite", "entry": "echo rewrote", "destructive": true}),
    )
    .await;

    let listed = call(&state, "pipeline_project", "devtool_list", json!({})).await;
    assert_eq!(listed["count"].as_u64(), Some(2));

    // Safe tool runs.
    let ran = call(
        &state,
        "pipeline_project",
        "devtool_run",
        json!({"name": "safe"}),
    )
    .await;
    assert_eq!(ran["dry_run"].as_bool(), Some(false));
    assert_eq!(ran["exit_code"].as_i64(), Some(0));
    assert!(ran["stdout"].as_str().is_some_and(|o| o.contains("ran")));

    // Destructive tool dry-runs unless confirmed.
    let dry = call(
        &state,
        "pipeline_project",
        "devtool_run",
        json!({"name": "rewrite"}),
    )
    .await;
    assert_eq!(
        dry["dry_run"].as_bool(),
        Some(true),
        "a destructive tool executed without confirmation: {dry}"
    );

    let confirmed = call(
        &state,
        "pipeline_project",
        "devtool_run",
        json!({"name": "rewrite", "confirm": true}),
    )
    .await;
    assert_eq!(confirmed["dry_run"].as_bool(), Some(false));

    // An unknown tool names what is registered rather than failing bare.
    let missing = call_tool(
        "pipeline_project",
        ToolRequest {
            action: "devtool_run".to_owned(),
            args: json!({"name": "nope"}),
        },
        state,
    )
    .await;
    let why = missing.error.unwrap_or_default();
    assert!(why.contains("safe"), "error must list known tools: {why}");
}

/// Mode decides which gates apply.
async fn mode_set_records_the_project_mode() {
    let _dir = enter_project();
    let state = Arc::new(ServerState::new());

    let built = call(
        &state,
        "pipeline_plan",
        "mode_set",
        json!({"mode": "build"}),
    )
    .await;
    assert_eq!(built["mode"].as_str(), Some("build"));

    let maintained = call(
        &state,
        "pipeline_plan",
        "mode_set",
        json!({"mode": "maintain"}),
    )
    .await;
    assert_eq!(maintained["previous"].as_str(), Some("build"));
    assert_eq!(
        maintained["gates"]["motion_compare"].as_str(),
        Some("required"),
        "maintain mode must require motion comparison: {maintained}"
    );

    let bogus = call_tool(
        "pipeline_plan",
        ToolRequest {
            action: "mode_set".to_owned(),
            args: json!({"mode": "vibes"}),
        },
        state,
    )
    .await;
    assert!(!bogus.ok, "an unknown mode must be refused");
}

/// Task tracking · durable, ✗ a session list.
async fn tasks_are_tracked_with_a_verifiable_done_condition() {
    let _dir = enter_project();
    let state = Arc::new(ServerState::new());

    // ! Acceptance is required. A task nobody can verify finished never is, so
    // omitting it must fail rather than default to empty.
    let no_acceptance = call_tool(
        "pipeline_plan",
        ToolRequest {
            action: "task_add".to_owned(),
            args: json!({"title": "do a thing"}),
        },
        state.clone(),
    )
    .await;
    assert!(!no_acceptance.ok, "acceptance must be required");

    let added = call(
        &state,
        "pipeline_plan",
        "task_add",
        json!({
            "title": "wire the coverage gate",
            "acceptance": "pipeline run fast reports a coverage line",
            "priority": "P1"
        }),
    )
    .await;
    let id = added["task"]["id"].as_str().expect("id").to_owned();
    assert_eq!(added["task"]["status"].as_str(), Some("open"));

    // Priority order: P0 sorts above P2.
    call(
        &state,
        "pipeline_plan",
        "task_add",
        json!({"title": "urgent", "acceptance": "it stops failing", "priority": "P0"}),
    )
    .await;
    let listed = call(&state, "pipeline_plan", "task_list", json!({})).await;
    assert_eq!(listed["total"].as_u64(), Some(2));
    assert_eq!(
        listed["tasks"][0]["priority"].as_str(),
        Some("P0"),
        "P0 must sort first: {listed}"
    );

    // ! Blocked without a reason is untrackable — nobody can unblock what
    // nobody named.
    let nameless = call_tool(
        "pipeline_plan",
        ToolRequest {
            action: "task_update".to_owned(),
            args: json!({"id": id, "status": "blocked"}),
        },
        state.clone(),
    )
    .await;
    assert!(!nameless.ok, "blocked must require a blocker");

    let blocked = call(
        &state,
        "pipeline_plan",
        "task_update",
        json!({"id": id, "status": "blocked", "blocker": "registry 403"}),
    )
    .await;
    assert_eq!(blocked["task"]["blocker"].as_str(), Some("registry 403"));

    // Leaving blocked clears the blocker · a stale reason reads as a live one.
    let unblocked = call(
        &state,
        "pipeline_plan",
        "task_update",
        json!({"id": id, "status": "in_progress"}),
    )
    .await;
    assert!(
        unblocked["task"]["blocker"].is_null(),
        "moving off blocked must clear the blocker: {unblocked}"
    );

    // Survives a reconnect · the whole point of durable tracking.
    let fresh = Arc::new(ServerState::new());
    let reread = call(&fresh, "pipeline_plan", "task_list", json!({})).await;
    assert_eq!(
        reread["total"].as_u64(),
        Some(2),
        "tasks did not survive: {reread}"
    );
}

/// Health and audit gather, ✗ judge.
async fn health_and_audit_report_gaps_without_grading() {
    let _dir = enter_project();
    let state = Arc::new(ServerState::new());

    let health = call(&state, "pipeline_meta", "health", json!({})).await;
    assert_eq!(health["project"].as_str(), Some("resource-test"));
    // A project with no runs must say so rather than report healthy silence.
    assert!(
        health["concerns"].as_array().is_some_and(|c| c
            .iter()
            .any(|x| x.as_str().is_some_and(|s| s.contains("no run")))),
        "a project with no runs must name that: {health}"
    );

    let audit = call(&state, "pipeline_meta", "audit", json!({})).await;
    let findings = audit["findings"].as_array().expect("findings");
    assert!(!findings.is_empty(), "a bare project has gaps: {audit}");
    // ! Every finding carries a severity so attention can be ranked — but the
    // response says plainly that severity ranks attention, ✗ acceptability.
    assert!(
        findings.iter().all(|f| f["severity"].is_string()),
        "every finding needs a severity: {findings:?}"
    );
    assert!(
        audit["note"]
            .as_str()
            .is_some_and(|n| n.contains("✗ a quality verdict")),
        "audit must not present itself as a verdict: {audit}"
    );
}

/// Review brief gathers · ✗ reviews.
async fn review_brief_gathers_material_and_defers_the_judgement() {
    // ! Builds its own two-commit repo rather than diffing the host checkout:
    // the earlier helpers leave the process in a temp directory, and a test that
    // depended on the surrounding repository would pass or fail by accident.
    let dir = enter_project();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("git");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.test"]);
    git(&["config", "user.name", "test"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    std::fs::write(dir.path().join("src_auth.rs"), "// token handling\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "add auth"]);

    let state = Arc::new(ServerState::new());
    let resp = call_tool(
        "pipeline_meta",
        ToolRequest {
            action: "review_brief".to_owned(),
            args: json!({"base": "HEAD~1"}),
        },
        state,
    )
    .await;

    assert!(resp.ok, "review_brief failed: {:?}", resp.error);
    let d = resp.data;

    // ! The load-bearing assertion. code_review §12 forbids an AI reviewer
    // returning verdicts, so this must never carry findings — only material.
    assert!(
        d.get("findings").is_none(),
        "review_brief returned findings · §12 makes those the reviewer's: {d}"
    );
    assert!(
        d["note"].as_str().is_some_and(|n| n.contains("✗ a review")),
        "the brief must say what it is not: {d}"
    );
    assert!(d["files_changed"].is_number());
    assert!(d["lines_changed"].is_number());
    let human_only = d["human_only_paths"].as_array().expect("array");
    assert!(
        human_only.iter().any(|p| p.as_str() == Some("src_auth.rs")),
        "a path naming auth must be flagged human-only (§12): {d}"
    );
}

/// Fleet health across registered repos · the maintenance view.
///
/// ! The gap this closes: every other health action reads the process working
/// directory, so maintaining N existing repos meant N chdirs. This asserts one
/// call reports all of them — including the two states an adopted repo starts
/// in, unmanaged and never-cloned, which are the ones a fleet view exists to
/// surface.
async fn fleet_health_reports_every_registered_repo_from_one_call() {
    let (_keep, data) = fleet_fixture().await;

    assert_eq!(
        data["total"], 3,
        "every registered repo is reported: {data}"
    );
    assert_eq!(data["unmanaged"], 1, "the repo without a config: {data}");
    assert_eq!(data["missing"], 1, "the repo with no tree on disk: {data}");

    let rows = data["repos"].as_array().expect("repos array");
    let row = |alias: &str| -> &Value {
        rows.iter()
            .find(|r| r["alias"] == alias)
            .unwrap_or_else(|| panic!("no row for '{alias}' in {data}"))
    };

    // ! A repo registered but never cloned reports exists:false, ✗ an empty
    // healthy row. Absent is reported as absent.
    let missing = row("never-cloned");
    assert_eq!(missing["state"], "missing", "{missing}");
    assert!(
        missing["attention"]
            .as_array()
            .expect("attention")
            .iter()
            .any(|a| a.as_str().is_some_and(|s| s.contains("never cloned"))),
        "the missing tree is named: {missing}"
    );

    let un = row("unmanaged");
    assert_eq!(un["state"], "unmanaged", "{un}");
    assert!(
        un["attention"]
            .as_array()
            .expect("attention")
            .iter()
            .any(|a| a.as_str().is_some_and(|s| s.contains("adopt=true"))),
        "the action that fixes it is named: {un}"
    );

    let m = row("managed");
    assert_eq!(m["state"], "managed", "{m}");
    assert_eq!(
        m["project"], "resource-test",
        "read from its own config: {m}"
    );
    // Nothing has ever run there, so the run and maturity views are honest
    // about it rather than reporting a level nobody earned.
    assert!(m["last_run"].is_null(), "no run recorded: {m}");
    assert_eq!(m["maturity"]["level"], 0, "below every level: {m}");
    assert_eq!(m["tasks"]["blocked"], 1, "blocked task counted: {m}");
    assert!(
        m["findings"]["high"].as_u64().expect("high count") > 0,
        "the audit rules ran against this repo: {m}"
    );

    // Loudest first · the caller reads the list in order and does not sort.
    let counts: Vec<usize> = rows
        .iter()
        .map(|r| r["attention"].as_array().map_or(0, Vec::len))
        .collect();
    assert!(
        counts.windows(2).all(|w| w[0] >= w[1]),
        "attention descending, got {counts:?}"
    );
    assert_eq!(
        data["needs_attention"], 3,
        "each of the three has something to say: {data}"
    );
}

/// Three registered repos in the three states an adopted fleet contains ·
/// returns the tempdirs (kept alive by the caller) and the fleet payload.
async fn fleet_fixture() -> (Vec<tempfile::TempDir>, Value) {
    // A managed repo with real recorded state.
    let managed = tempfile::tempdir().expect("tempdir");
    std::fs::write(managed.path().join("pipeline.yaml"), PROJECT_YAML).expect("write config");
    std::env::set_current_dir(managed.path()).expect("chdir managed");
    {
        let state = Arc::new(ServerState::new());
        let added = call(
            &state,
            "pipeline_plan",
            "task_add",
            json!({
                "title": "upgrade the toolchain",
                "acceptance": "cargo build passes on the new pin",
            }),
        )
        .await;
        let id = added["task"]["id"].as_str().expect("task id").to_owned();
        call(
            &state,
            "pipeline_plan",
            "task_update",
            json!({
                "id": id,
                "status": "blocked",
                "blocker": "waiting on the upstream release",
            }),
        )
        .await;
    }

    // A repo Pipeline has never been told about beyond its path.
    let unmanaged = tempfile::tempdir().expect("tempdir");
    std::fs::write(unmanaged.path().join("README.md"), "hello").expect("write readme");

    // The hub the fleet is read from · deliberately NOT one of the repos.
    let hub = tempfile::tempdir().expect("tempdir");
    let registry = json!({"repos": [
        {"alias": "managed", "url": managed.path().to_string_lossy(),
         "kind": "local", "added_at": "2026-01-01T00:00:00Z", "cloned": true},
        {"alias": "unmanaged", "url": unmanaged.path().to_string_lossy(),
         "kind": "local", "added_at": "2026-01-01T00:00:00Z", "cloned": true},
        {"alias": "never-cloned", "url": "https://github.com/example/never-cloned",
         "kind": "git", "added_at": "2026-01-01T00:00:00Z", "cloned": false},
    ]});
    std::fs::create_dir_all(hub.path().join(".pipeline/repos")).expect("mkdir registry");
    std::fs::write(
        hub.path().join(".pipeline/repos/registry.json"),
        serde_json::to_string_pretty(&registry).expect("serialize"),
    )
    .expect("write registry");
    std::env::set_current_dir(hub.path()).expect("chdir hub");

    let state = Arc::new(ServerState::new());
    let data = call(&state, "pipeline_repo", "fleet_health", json!({})).await;
    (vec![managed, unmanaged, hub], data)
}

//! The Standards ↔ Pipeline seam, driven the way an agent drives it.
//!
//! ! The gaps these close, all found by driving the real corpus through the MCP
//! server rather than by reading the code:
//!
//! 1. `index.json` had gone stale by 1,224 lines upstream, so standards added to
//!    the repo were invisible to routing. Nothing on either side noticed.
//! 2. A hollow-but-parseable index bound zero standards and reported `ok: true`
//!    with `total: 0` — "no standards apply" as an answer, ✗ as a broken corpus.
//! 3. `check` returned `ok: false` with `error: null`, so a caller reading
//!    `error` on failure got nothing.
//! 4. `check` shipped every checklist item for every bound standard — ~14k
//!    tokens to answer a yes/no question.
//!
//! ! One `#[tokio::test]` per binary: `PIPELINE_STANDARDS_DIR` and the working
//! directory are both process-global, so parallel tests in this file would race
//! exactly like the chdir tests in `resource_actions`.

use pipeline_mcp::{ServerState, ToolRequest, call_tool};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// ! `standards.source` points at the fixture corpus rather than exporting
/// `PIPELINE_STANDARDS_DIR`. The workspace forbids `unsafe_code` and
/// `set_var` is unsafe on this edition, but the config route is the better
/// test anyway — it exercises the cascade entry a real project uses.
fn project_yaml(corpus: &Path) -> String {
    format!(
        "project: seam-test
version: 0.0.1
stack:
  runtime: rust
  services: []
stages:
  fast: [static, unit]
standards:
  source: {}
  project_type: MCP server
  languages: [rust]
  surfaces:
    - Command line
",
        corpus.display()
    )
}

/// A corpus with the invariants a real Standards repo always has.
const GOOD_INDEX: &str = r#"{
  "schema": 1,
  "tier_order": ["Foundation", "Core", "Language"],
  "always_on": ["architecture", "testing"],
  "routes": {
    "by_type": { "MCP server": { "add": ["local_mcp"] } },
    "by_surface": { "Command line": { "add": ["cli"] } }
  },
  "standards": [
    {"id":"architecture","domain":"architecture","path":"architecture/STANDARDS.md",
     "title":"Architecture Standards","purpose":"layers","tier":"Foundation",
     "checklist":["Dependency graph is a DAG"]},
    {"id":"testing","domain":"testing","path":"testing/STANDARDS.md",
     "title":"Testing Standards","purpose":"pyramid","tier":"Core",
     "checklist":["Coverage gate enforced","Flake budget declared"]},
    {"id":"local_mcp","domain":"local_mcp","path":"local_mcp/STANDARDS.md",
     "title":"MCP Standards","purpose":"mcp","tier":"Domain",
     "checklist":["Tool surface budgeted"]},
    {"id":"cli","domain":"cli","path":"cli/STANDARDS.md",
     "title":"CLI Standards","purpose":"cli","tier":"Interface",
     "checklist":["Exit codes documented"]},
    {"id":"rust","domain":"rust","path":"rust/STANDARDS.md",
     "title":"Rust Standards","purpose":"rust","tier":"Language",
     "checklist":["No unwrap in library code"]}
  ]
}"#;

/// `check` answers "is the binding sound" — a verdict plus a reason. It used to
/// ship every checklist item for every bound standard to do it, ~14k tokens,
/// which is most of a small agent's headroom spent on one yes/no.
const CHECK_BUDGET_BYTES: usize = 4_000;

async fn call(state: &Arc<ServerState>, action: &str, args: Value) -> pipeline_mcp::ToolResponse {
    call_tool(
        "pipeline_standards",
        ToolRequest {
            action: action.to_owned(),
            args,
        },
        state.clone(),
    )
    .await
}

/// Write a corpus and make it a git repo · resolution reads the sha from git,
/// so a plain directory never gets as far as the index.
fn corpus(dir: &Path, index_json: &str) {
    std::fs::write(dir.join("index.json"), index_json).expect("write index");
    for (sub, name) in [
        ("architecture", "Architecture"),
        ("testing", "Testing"),
        ("local_mcp", "MCP"),
        ("cli", "CLI"),
        ("rust", "Rust"),
    ] {
        std::fs::create_dir_all(dir.join(sub)).expect("mkdir");
        std::fs::write(
            dir.join(sub).join("STANDARDS.md"),
            format!("# {name} Standards\n\n> Purpose.\n\n## 1. Checklist\n\n- [ ] item\n"),
        )
        .expect("write standard");
    }
    for args in [
        vec!["init", "-q", "."],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "corpus"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(&args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?} failed");
    }
}

fn enter(index_json: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let std_dir = tempfile::tempdir().expect("tempdir");
    corpus(std_dir.path(), index_json);

    let project = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        project.path().join("pipeline.yaml"),
        project_yaml(std_dir.path()),
    )
    .expect("write config");
    std::env::set_current_dir(project.path()).expect("chdir");
    (project, std_dir)
}

#[tokio::test]
async fn the_standards_seam_holds() {
    every_read_action_resolves_against_a_real_corpus().await;
    a_hollow_corpus_is_refused_by_every_action().await;
    check_states_its_reason_and_stays_cheap().await;
    show_reaches_every_standard_the_index_lists().await;
}

/// The read surface an agent actually calls, end to end.
async fn every_read_action_resolves_against_a_real_corpus() {
    let (_p, _s) = enter(GOOD_INDEX);
    let state = Arc::new(ServerState::new());

    for action in ["brief", "list", "route", "checklist"] {
        let r = call(&state, action, json!({})).await;
        assert!(r.ok, "{action} failed: {:?}", r.error);
    }

    let routed = call(&state, "route", json!({})).await;
    let ids: Vec<&str> = routed.data["routed"]["ids"]
        .as_array()
        .expect("ids")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // always-on ∪ language ∪ project type ∪ surface — each for its own reason.
    for expected in ["architecture", "testing", "local_mcp", "cli", "rust"] {
        assert!(ids.contains(&expected), "{expected} not routed: {ids:?}");
    }

    let listed = call(&state, "list", json!({})).await;
    assert_eq!(listed.data["total"], 5, "catalog size: {}", listed.data);
    assert_eq!(listed.data["bound"], 5, "bound count: {}", listed.data);
}

/// ! `ok: true` with nothing bound is the failure this locks out. A Standards
/// repo always has standards, a tier order, and an always-on set — absence of
/// any of them is a malformed corpus, ✗ a project with no obligations.
async fn a_hollow_corpus_is_refused_by_every_action() {
    let (_p, _s) = enter(r#"{"schema":1}"#);
    let state = Arc::new(ServerState::new());

    for action in ["brief", "list", "route", "checklist", "check"] {
        let r = call(&state, action, json!({})).await;
        assert!(
            !r.ok,
            "{action} accepted a corpus that binds nothing: {}",
            r.data
        );
        let why = r.error.unwrap_or_default();
        assert!(
            why.contains("standards") && why.contains("index.json"),
            "{action} refusal must name the corpus and what it lacks: {why}"
        );
    }
}

/// A refused call states why in `error`, and the verdict call stays small
/// enough to make on every loop.
async fn check_states_its_reason_and_stays_cheap() {
    let (_p, _s) = enter(GOOD_INDEX);
    let state = Arc::new(ServerState::new());

    // No pin in this fixture's pipeline.yaml → unpinned is blocking.
    let blocked = call(&state, "check", json!({})).await;
    assert!(
        !blocked.ok,
        "unpinned corpus must not pass: {}",
        blocked.data
    );
    let why = blocked.error.clone().unwrap_or_default();
    assert!(
        !why.is_empty(),
        "ok:false with no error leaves the caller nothing to act on"
    );
    assert!(
        why.contains("pin"),
        "the refusal must name the blocker: {why}"
    );
    assert_eq!(
        blocked.data["blocking"]
            .as_array()
            .expect("blocking array")
            .len(),
        1,
        "one blocker expected: {}",
        blocked.data
    );

    // ! Budget, ✗ a preference. See CHECK_BUDGET_BYTES.
    let size = serde_json::to_string(&blocked.data)
        .expect("serialize")
        .len();
    assert!(
        size < CHECK_BUDGET_BYTES,
        "check payload {size}B exceeds the {CHECK_BUDGET_BYTES}B budget · \
         items belong in pipeline_standards.checklist"
    );

    // The items are still reachable — moved, ✗ dropped.
    let items = call(&state, "checklist", json!({})).await;
    assert!(items.ok, "checklist failed: {:?}", items.error);
    assert!(
        items.data["total_items"].as_u64().is_some_and(|n| n > 0),
        "checklist must carry the obligations check no longer ships: {}",
        items.data
    );
}

/// Every standard the index lists is fetchable by id · a catalog entry that
/// cannot be shown is a broken link between the two repos.
async fn show_reaches_every_standard_the_index_lists() {
    let (_p, _s) = enter(GOOD_INDEX);
    let state = Arc::new(ServerState::new());

    let listed = call(&state, "list", json!({})).await;
    let ids: Vec<String> = listed.data["standards"]
        .as_array()
        .expect("standards")
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_owned))
        .collect();
    assert!(!ids.is_empty(), "catalog came back empty");

    for id in &ids {
        let r = call(&state, "show", json!({ "id": id })).await;
        assert!(r.ok, "show '{id}' failed: {:?}", r.error);
    }

    let missing = call(&state, "show", json!({"id": "no-such-standard"})).await;
    assert!(!missing.ok, "an unknown id must be refused");
    assert!(
        missing
            .error
            .unwrap_or_default()
            .contains("unknown standard"),
        "refusal must name the problem"
    );
}

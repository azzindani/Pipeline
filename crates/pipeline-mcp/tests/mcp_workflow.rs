//! End-to-end workflow · one agent session driven through the real dispatch path.
//!
//! Every other test in this crate exercises a handler in isolation. That leaves
//! the most important claim unproven: that an agent can connect, do a unit of
//! work, and find that work still there afterwards. The failure mode this
//! catches is a handler that passes its own unit test while writing nothing an
//! agent could later read back.
//!
//! Route is deliberately the production one — `call_tool`, so schema validation
//! and the `Planned` refusal in `dispatch` both apply — against a real `SQLite`
//! database in a temp project, ✗ a mock.
//!
//! ! Single `#[test]` in its own binary on purpose: handlers resolve the project
//! from the process working directory, so changing it is process-global and
//! would race any sibling test sharing the binary.

use pipeline_mcp::{ServerState, ToolRequest, call_tool};
use serde_json::{Value, json};
use std::sync::Arc;

const PROJECT_YAML: &str = r"project: workflow-test
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

/// Advance the tracker · the thread a context reset loses.
async fn advance_the_progress_tracker(state: &Arc<ServerState>) {
    // The progress tracker is the thread a reset loses. Advance it, then prove
    // a fresh connection resumes at the right step rather than the first one.
    call(
        state,
        "pipeline_session",
        "progress",
        json!({"goal": "prove the tracker survives a reset",
                "remaining": ["step one", "step two", "step three"]}),
    )
    .await;
    let advanced = call(
        state,
        "pipeline_session",
        "progress",
        json!({"completed": "step one", "blocker": "waiting on registry access"}),
    )
    .await;
    assert_eq!(
        advanced.get("next_step").and_then(Value::as_str),
        Some("step two"),
        "completing a step must advance next_step: {advanced}"
    );
}

/// A reconnecting agent must resume mid-thread, ✗ at the first step.
async fn assert_resumes_mid_thread(state: &Arc<ServerState>) {
    let resumed = call(state, "pipeline_session", "progress", json!({})).await;
    assert_eq!(
        resumed.get("next_step").and_then(Value::as_str),
        Some("step two"),
        "a reconnecting agent resumed at the wrong step: {resumed}"
    );
    assert_eq!(
        resumed.get("blocker").and_then(Value::as_str),
        Some("waiting on registry access"),
        "the blocker did not survive the reconnect: {resumed}"
    );
    assert!(
        serde_json::to_string(&resumed)
            .expect("serialize")
            .contains("step one"),
        "completed work was lost — the agent will redo it: {resumed}"
    );
}

#[tokio::test]
async fn an_agent_session_survives_its_own_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pipeline.yaml"), PROJECT_YAML).expect("write config");
    std::env::set_current_dir(dir.path()).expect("chdir");

    let state = Arc::new(ServerState::new());

    // Identify, then take the project lock. Both are prerequisites the rest of
    // the surface assumes — a handler that silently works without them would be
    // hiding a missing check.
    call(
        &state,
        "pipeline_session",
        "agent_register",
        json!({"agent_id": "workflow-test-agent"}),
    )
    .await;
    let locked = call(&state, "pipeline_session", "lock", json!({})).await;
    let session_id = locked
        .get("session_id")
        .and_then(Value::as_str)
        .expect("lock returns a session_id")
        .to_owned();

    // Do work an agent would actually do: state a goal, record a decision,
    // remember a fact.
    call(
        &state,
        "pipeline_plan",
        "idea_capture",
        json!({"text": "prove the workflow round-trips"}),
    )
    .await;
    call(
        &state,
        "pipeline_memory",
        "remember",
        json!({"key": "workflow_probe", "value": "round-trip marker", "scope": "plan"}),
    )
    .await;
    call(
        &state,
        "pipeline_session",
        "checkpoint",
        json!({"note": "work recorded"}),
    )
    .await;

    // The claim under test: the work is readable back through a different tool
    // than the one that wrote it.
    let recalled = call(
        &state,
        "pipeline_memory",
        "recall",
        json!({"key": "workflow_probe", "scope": "plan"}),
    )
    .await;
    assert!(
        serde_json::to_string(&recalled)
            .expect("serialize")
            .contains("round-trip marker"),
        "memory.recall did not return what memory.remember stored: {recalled}"
    );

    advance_the_progress_tracker(&state).await;

    // Handover is what a cold agent reads first. It must reflect this session.
    let handover = call(&state, "pipeline_session", "handover", json!({})).await;
    let rendered = serde_json::to_string(&handover).expect("serialize");
    assert!(
        rendered.contains("workflow-test"),
        "handover packet does not name the project it was built from: {rendered}"
    );

    // Close cleanly · the lock must actually release, ✗ merely report success.
    call(
        &state,
        "pipeline_session",
        "end",
        json!({"session_id": session_id, "outcome": "ok", "summary": "round trip"}),
    )
    .await;

    let relock = call(&state, "pipeline_session", "lock", json!({})).await;
    assert!(
        relock.get("session_id").and_then(Value::as_str).is_some(),
        "lock could not be reacquired after end — the previous lock leaked: {relock}"
    );

    // Persistence is the point: a fresh state handle reads the same database,
    // the way a reconnecting agent would.
    let fresh = Arc::new(ServerState::new());
    assert_resumes_mid_thread(&fresh).await;

    let reread = call(
        &fresh,
        "pipeline_memory",
        "recall",
        json!({"key": "workflow_probe", "scope": "plan"}),
    )
    .await;
    assert!(
        serde_json::to_string(&reread)
            .expect("serialize")
            .contains("round-trip marker"),
        "work did not survive a new connection: {reread}"
    );
}

#[tokio::test]
async fn a_planned_action_is_refused_before_its_handler_runs() {
    // The fidelity contract's load-bearing half, checked through the same path
    // an agent uses. `e2e.record` spawns an interactive tool with no timeout —
    // if dispatch ever stopped refusing it, this test hangs rather than fails,
    // which is itself the signal.
    let state = Arc::new(ServerState::new());
    let resp = call_tool(
        "pipeline_e2e",
        ToolRequest {
            action: "record".to_owned(),
            args: json!({}),
        },
        state,
    )
    .await;

    assert!(!resp.ok, "a Planned action returned ok: {:?}", resp.data);
    assert_eq!(
        resp.data.get("fidelity").and_then(Value::as_str),
        Some("planned"),
        "refusal did not identify itself as a fidelity refusal: {}",
        resp.data
    );
}

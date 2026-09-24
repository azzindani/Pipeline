//! `init` roots a stdio server at the project it produced.
//!
//! Handlers resolve the project from the process cwd, and `init` writes into
//! `<parent>/<name>`. A server started in a directory of projects — a harness's
//! shared `/workspace` — kept reading `<cwd>/pipeline.yaml` after init, so every call
//! init suggested next failed. Reproduced live from a Claude Code harness before the fix.
//!
//! ! Single `#[test]` in its own binary on purpose: the behaviour under test moves the
//! process working directory, which would race any sibling test sharing the binary.

use pipeline_mcp::{ServerState, ToolRequest, ToolResponse, call_tool};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

async fn call(state: &Arc<ServerState>, tool: &str, action: &str, args: Value) -> ToolResponse {
    let req = ToolRequest {
        action: action.to_owned(),
        args,
    };
    call_tool(tool, req, state.clone()).await
}

async fn ok(state: &Arc<ServerState>, tool: &str, action: &str, args: Value) -> Value {
    let resp = call(state, tool, action, args).await;
    assert!(
        resp.ok,
        "{tool}.{action} failed: {:?} · data {}",
        resp.error, resp.data
    );
    resp.data
}

fn cwd() -> std::path::PathBuf {
    std::env::current_dir().expect("cwd")
}

fn canon(p: &Path) -> String {
    p.canonicalize()
        .expect("canonicalize")
        .display()
        .to_string()
}

#[tokio::test]
async fn init_roots_a_stdio_server_at_the_project_it_created() {
    let shared = tempfile::tempdir().expect("tempdir");
    std::env::set_current_dir(shared.path()).expect("chdir");
    let state = Arc::new(ServerState::stdio());

    // Rooted at a directory of projects · the miss names the root and the way out.
    let miss = call(
        &state,
        "pipeline_memory",
        "remember",
        json!({"key": "k", "value": "x", "scope": "project"}),
    )
    .await;
    let e = miss.error.unwrap_or_default();
    assert!(!miss.ok);
    assert!(e.contains("rooted at"), "must name the root: {e}");
    assert!(
        e.contains("pipeline_project.init"),
        "must name the fix: {e}"
    );

    // Create project a · the server follows it, and says it did.
    let a = ok(
        &state,
        "pipeline_project",
        "init",
        json!({"name": "a", "stack": "python-uv"}),
    )
    .await;
    let a_root = shared.path().join("a");
    assert_eq!(a["server_rooted"], json!(true), "{a}");
    assert_eq!(a["server_root"], json!(canon(&a_root)), "{a}");
    assert_eq!(a["server_moved_from"], json!(canon(shared.path())), "{a}");
    assert_eq!(canon(&cwd()), canon(&a_root));

    // The call init suggested now reaches the new project.
    ok(
        &state,
        "pipeline_memory",
        "remember",
        json!({"key": "k", "value": "a", "scope": "project"}),
    )
    .await;

    // Create b as a sibling · the cached memory handle must not follow a's database.
    ok(
        &state,
        "pipeline_project",
        "init",
        json!({"name": "b", "stack": "python-uv", "parent": shared.path()}),
    )
    .await;
    ok(
        &state,
        "pipeline_memory",
        "remember",
        json!({"key": "k", "value": "b", "scope": "project"}),
    )
    .await;
    assert!(shared.path().join("b/.pipeline/memory.db").is_file());

    // Adopt a back · its memory still holds a's value, ✗ b's write.
    let back = ok(
        &state,
        "pipeline_project",
        "init",
        json!({"name": "a", "stack": "python-uv", "parent": shared.path(), "adopt": true}),
    )
    .await;
    assert_eq!(back["server_root"], json!(canon(&a_root)), "{back}");
    let recalled = ok(
        &state,
        "pipeline_memory",
        "recall",
        json!({"key": "k", "scope": "project"}),
    )
    .await;
    assert_eq!(
        recalled["value"],
        json!("a"),
        "b's write leaked into a: {recalled}"
    );

    // An open session pins the root · moving would orphan its lock.
    ok(&state, "pipeline_session", "lock", json!({})).await;
    let pinned = ok(
        &state,
        "pipeline_project",
        "init",
        json!({"name": "c", "stack": "python-uv", "parent": shared.path()}),
    )
    .await;
    assert_eq!(pinned["server_rooted"], json!(false), "{pinned}");
    assert_eq!(canon(&cwd()), canon(&a_root), "moved away from a held lock");

    // A shared (HTTP) server never moves · every principal shares its cwd.
    let shared_state = Arc::new(ServerState::new());
    let resp = call(
        &shared_state,
        "pipeline_project",
        "init",
        json!({"name": "d", "stack": "python-uv", "parent": shared.path()}),
    )
    .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data["server_rooted"], json!(false), "{}", resp.data);
    assert!(
        resp.next_suggested.is_empty(),
        "suggestions assumed a move that did not happen: {:?}",
        resp.next_suggested
    );
    assert_eq!(canon(&cwd()), canon(&a_root));

    // Leave the tempdir before it is removed.
    std::env::set_current_dir(std::env::temp_dir()).expect("chdir out");
}

//! The learning loop, end to end.
//!
//! CLAUDE.md claims: run 1 fails → agent fixes → Pipeline stores (error, fix,
//! outcome) → run 2 hits a similar error → Pipeline surfaces the previous fix.
//!
//! ! This test exists because the loop was open. `fix_applied` and `fix_worked`
//! were read in three places and written in none, so the filter selecting prior
//! fixes could never match — and returned zero, which is also what a genuinely
//! new error looks like. The failure was invisible by construction.

use pipeline_memory::{Memory, NewFailure, NewRun};

async fn memory() -> Memory {
    let mem = Memory::open_in_memory().await.expect("open");
    mem.upsert_project("proj", "proj", "rust")
        .await
        .expect("project");
    mem
}

async fn failing_run(mem: &Memory, error: &str) -> String {
    let run_id = mem
        .log_run(&NewRun {
            project_id: "proj",
            session_id: None,
            profile: "fast",
            stage: "unit",
            status: "fail",
            duration_ms: 10,
            triggered_by: None,
            commit_sha: None,
            stdout: None,
            stderr: None,
            failure_json: None,
        })
        .await
        .expect("log run");
    mem.log_failure(&NewFailure {
        run_id: &run_id,
        stage: "unit",
        error_message: error,
        file: None,
        line: None,
    })
    .await
    .expect("log failure")
}

#[tokio::test]
async fn a_recorded_fix_is_surfaced_on_the_next_similar_failure() {
    let mem = memory().await;

    let first = failing_run(&mem, "test_auth failed: JWT_SECRET not set").await;
    mem.record_fix(&first, "set JWT_SECRET in .env.test", true)
        .await
        .expect("record fix");

    // Run 2 · a similar error arrives.
    failing_run(&mem, "test_auth failed: JWT_SECRET not set").await;

    let similar = mem
        .find_similar_failures("proj", "test_auth failed: JWT_SECRET not set", 5)
        .await
        .expect("find similar");

    let prior: Vec<_> = similar
        .iter()
        .filter(|f| f.fix_worked == Some(1) && f.fix_applied.is_some())
        .collect();

    assert!(
        !prior.is_empty(),
        "the loop is open — a fix was recorded but no prior fix surfaced: {similar:#?}"
    );
    assert_eq!(
        prior[0].fix_applied.as_deref(),
        Some("set JWT_SECRET in .env.test")
    );
}

#[tokio::test]
async fn a_fix_that_did_not_work_is_kept_but_never_suggested() {
    // ! Knowing what failed is the half that stops an agent retrying it. It must
    // be stored, and it must not be offered as a prior fix.
    let mem = memory().await;

    let id = failing_run(&mem, "connection refused on redis").await;
    mem.record_fix(&id, "restarted the test runner", false)
        .await
        .expect("record fix");

    let similar = mem
        .find_similar_failures("proj", "connection refused on redis", 5)
        .await
        .expect("find similar");

    let recorded = similar
        .iter()
        .find(|f| f.id == id)
        .expect("the failure is still there");
    assert_eq!(
        recorded.fix_applied.as_deref(),
        Some("restarted the test runner"),
        "a failed attempt must be remembered"
    );
    assert_eq!(recorded.fix_worked, Some(0));

    let suggested: Vec<_> = similar
        .iter()
        .filter(|f| f.fix_worked == Some(1) && f.fix_applied.is_some())
        .collect();
    assert!(
        suggested.is_empty(),
        "a fix that did not work was offered as a prior fix: {suggested:#?}"
    );
}

#[tokio::test]
async fn recording_against_an_unknown_failure_reports_it() {
    // Silently succeeding would let an agent believe it had taught the system
    // something it had not.
    let mem = memory().await;
    let applied = mem
        .record_fix("no-such-failure", "something", true)
        .await
        .expect("query runs");
    assert!(
        !applied,
        "recording against a missing failure claimed success"
    );
}

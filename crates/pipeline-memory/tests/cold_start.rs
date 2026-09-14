//! Handover from a genuinely cold start.
//!
//! CLAUDE.md: "Pipeline builds this entirely from its own structured data. No
//! agent input required. Any agent connects and immediately knows where things
//! stand."
//!
//! ! The claim is only meaningful across a process boundary. A test that reuses
//! the handle that wrote the data proves the cache works, ✗ that the packet is
//! reconstructible — so this one closes the database and opens it again from
//! disk, which is what a new agent on a new connection actually does.

use pipeline_memory::{Memory, NewFailure, NewRun, Progress};

#[tokio::test]
async fn a_new_process_reconstructs_the_whole_packet_from_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("memory.db");

    // ── Session one · do work, then drop every handle.
    {
        let mem = Memory::open(&db).await.expect("open");
        mem.upsert_project("proj", "proj", "rust")
            .await
            .expect("project");

        let run_id = mem
            .log_run(&NewRun {
                project_id: "proj",
                session_id: None,
                profile: "full",
                stage: "integration",
                status: "fail",
                duration_ms: 4200,
                triggered_by: Some("agent"),
                commit_sha: Some("abc123"),
                stdout: None,
                stderr: None,
                failure_json: None,
            })
            .await
            .expect("run");

        let failure = mem
            .log_failure(&NewFailure {
                run_id: &run_id,
                stage: "integration",
                error_message: "test_login failed: JWT_SECRET not set",
                file: Some("tests/test_auth.py"),
                line: Some(42),
            })
            .await
            .expect("failure");
        mem.record_fix(&failure, "set JWT_SECRET in .env.test", true)
            .await
            .expect("fix");

        mem.set_progress(
            "proj",
            &Progress {
                goal: Some("fix JWT authentication flow".to_owned()),
                completed: vec!["reproduced the failure".to_owned()],
                remaining: vec![
                    "set JWT_SECRET in .env.test".to_owned(),
                    "rerun integration".to_owned(),
                ],
                blocker: Some("JWT_SECRET missing in the test environment".to_owned()),
                updated_at: Some(pipeline_memory::now_rfc3339()),
            },
        )
        .await
        .expect("progress");
    }

    // ── Session two · a cold agent, nothing but the file on disk.
    let cold = Memory::open(&db).await.expect("reopen");
    let packet = cold.handover("proj").await.expect("handover");

    assert_eq!(packet.project.id, "proj");

    let last = packet
        .last_run
        .expect("a cold agent must learn what last ran");
    assert_eq!(last.status, "fail");
    assert_eq!(last.stage, "integration");

    assert!(
        !packet.recent_failures.is_empty(),
        "the packet names no failure — the agent would not know what broke"
    );
    let known = &packet.recent_failures[0];
    assert_eq!(known.file.as_deref(), Some("tests/test_auth.py"));
    assert_eq!(known.line, Some(42));
    assert_eq!(
        known.fix_applied.as_deref(),
        Some("set JWT_SECRET in .env.test"),
        "the fix that worked did not survive — the loop's whole point is that it does"
    );

    // The thread, not just the plan.
    assert_eq!(
        packet.progress.goal.as_deref(),
        Some("fix JWT authentication flow")
    );
    assert_eq!(
        packet.progress.remaining.first().map(String::as_str),
        Some("set JWT_SECRET in .env.test"),
        "a cold agent must resume at the next step, ✗ the first"
    );
    assert_eq!(
        packet.progress.blocker.as_deref(),
        Some("JWT_SECRET missing in the test environment")
    );
}

#[tokio::test]
async fn a_bare_project_still_answers() {
    // ! Handover must answer on a project that has done nothing. Erroring here
    // would make the first call of every new project a failure.
    let dir = tempfile::tempdir().expect("tempdir");
    let mem = Memory::open(&dir.path().join("memory.db"))
        .await
        .expect("open");
    mem.upsert_project("fresh", "fresh", "rust")
        .await
        .expect("project");

    let packet = mem
        .handover("fresh")
        .await
        .expect("handover on a bare project");
    assert!(packet.last_run.is_none());
    assert!(packet.recent_failures.is_empty());
    assert!(packet.progress.goal.is_none());
    assert!(
        packet.progress.remaining.is_empty(),
        "an empty tracker must be empty, ✗ absent — absent is indistinguishable from unset"
    );
}

//! CLI surface contract · what a caller and a script can rely on.
//!
//! ! Drives the built binary, ✗ internal functions. The CLI's contract is exit
//! codes, stdout shape, and stderr separation — none of which a unit test on an
//! internal helper can observe.
//!
//! The stdout/stderr split is load-bearing beyond tidiness: on the stdio MCP
//! transport stdout carries JSON-RPC framing, one message per line. A single
//! log line on stdout corrupts the stream and the client's next parse fails.

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> PathBuf {
    // target/debug/deps/<test> → target/debug/pipeline
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("pipeline")
}

fn run(args: &[&str], cwd: &Path) -> (bool, String, String) {
    let out = Command::new(binary())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn pipeline");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const MINIMAL: &str = "project: cli-test
version: 0.0.1
stack:
  runtime: rust
  services: []
stages:
  fast: [static, unit]
gates:
  coverage: 70
";

#[test]
fn an_unknown_profile_fails_and_names_the_valid_ones() {
    // A caller that mistypes must learn the options, ✗ get a bare non-zero.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pipeline.yaml"), MINIMAL).expect("config");

    let (ok, _, stderr) = run(&["run", "nonsense"], dir.path());
    assert!(!ok, "an unknown profile must exit non-zero");
    for expected in ["fast", "full", "preflight", "confirm"] {
        assert!(
            stderr.contains(expected),
            "error names no valid profile '{expected}': {stderr}"
        );
    }
}

#[test]
fn config_prints_to_stdout_and_logs_stay_off_it() {
    // ! The stdio MCP transport shares this discipline — one stray log line on
    // stdout breaks a client's JSON-RPC parse.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pipeline.yaml"), MINIMAL).expect("config");

    let (ok, stdout, _) = run(&["config"], dir.path());
    assert!(ok, "config on a valid project must succeed");
    assert!(
        stdout.contains("cli-test"),
        "config did not print the project: {stdout}"
    );
    assert!(
        !stdout.contains("INFO") && !stdout.contains("WARN"),
        "a log line reached stdout: {stdout}"
    );
}

#[test]
fn a_missing_config_fails_with_a_reason_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ok, _, stderr) = run(&["config"], dir.path());
    assert!(!ok, "no pipeline.yaml must not succeed");
    assert!(
        !stderr.contains("panicked"),
        "a missing config panicked instead of reporting: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("pipeline.yaml") || stderr.to_lowercase().contains("config"),
        "the error does not say what is missing: {stderr}"
    );
}

#[test]
fn project_flag_rejects_a_path_that_is_not_a_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("a-file");
    std::fs::write(&file, "not a directory").expect("write");

    // `--project` is scoped to `mcp`, ✗ global · it chdirs before the server
    // binds, so the rejection lands before anything starts listening.
    let (ok, _, stderr) = run(
        &["mcp", "--project", file.to_str().expect("utf8")],
        dir.path(),
    );
    assert!(!ok, "a file passed as --project must fail");
    assert!(
        stderr.contains("not a directory"),
        "the error does not explain the rejection: {stderr}"
    );
}

#[test]
fn help_lists_every_documented_subcommand() {
    // CLAUDE.md §"CLI commands" is a contract with the agent reading it.
    let dir = tempfile::tempdir().expect("tempdir");
    let (ok, stdout, _) = run(&["--help"], dir.path());
    assert!(ok, "--help must succeed");
    for cmd in [
        "mcp",
        "run",
        "dev",
        "watch",
        "init",
        "report",
        "config",
        "standards",
    ] {
        assert!(stdout.contains(cmd), "--help omits '{cmd}': {stdout}");
    }
}

#[test]
fn version_is_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ok, stdout, _) = run(&["--version"], dir.path());
    assert!(ok, "--version must succeed");
    assert!(
        stdout.chars().any(|c| c.is_ascii_digit()),
        "--version printed no version: {stdout}"
    );
}

/// ! CI depends on this exit code. `pipeline standards check` is the gate that
/// makes an improvement in the Standards repo reach this one: when the pin
/// falls behind the corpus, or no route binds, the build must go red. A gate
/// that reports a problem on stdout and exits 0 is not a gate — which is how
/// the binding drifted for weeks while `check` described the drift correctly on
/// every call.
#[test]
fn standards_check_exits_non_zero_when_the_binding_is_not_sound() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pipeline.yaml"), MINIMAL).expect("write config");

    // No corpus reachable · no cache, no source, cloning disabled for `check`.
    let (ok, _stdout, stderr) = run(&["standards", "check"], dir.path());
    assert!(
        !ok,
        "an unresolvable corpus must fail the build, ✗ pass quietly"
    );
    assert!(
        stderr.contains("standards"),
        "the failure must name what went wrong: {stderr}"
    );
}

#[test]
fn standards_subcommands_are_discoverable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ok, stdout, _) = run(&["standards", "--help"], dir.path());
    assert!(ok, "standards --help must succeed");
    for action in ["fetch", "check", "route", "list", "pin"] {
        assert!(
            stdout.contains(action),
            "standards --help omits '{action}': {stdout}"
        );
    }
}

//! The stdio transport's stdout must carry JSON-RPC and nothing else.
//!
//! ! Regression: `init_tracing` wrote to stdout, so the first tool call that
//! logged interleaved `INFO` lines with the framing and every real MCP client
//! failed its next parse. Unit tests never caught it — they call handlers
//! directly and never look at the process's stdout.

use std::io::Write;
use std::process::{Command, Stdio};

/// A project that is deliberately NOT a cargo workspace: the stage runner logs
/// its checks, then the checks fail fast. Logging is what this test is about.
const CONFIG: &str = "project: probe\nversion: 0.0.1\nstack:\n  runtime: rust\n  services: []\n\
                      \nstages:\n  fast:\n    - static\n    - unit\n";

const REQUESTS: &str = concat!(
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
    "\n",
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"pipeline_run","arguments":{"action":"stage","args":{"profile":"fast"}}}}"#,
    "\n",
);

#[test]
fn every_stdout_line_is_json_even_while_stages_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pipeline.yaml"), CONFIG).expect("write config");

    let mut child = Command::new(env!("CARGO_BIN_EXE_pipeline"))
        .args(["mcp", "--transport", "stdio", "--project"])
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pipeline");

    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(REQUESTS.as_bytes())
        .expect("write requests");

    let out = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let mut seen = 0;
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        seen += 1;
        serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
            panic!("stdout line {seen} is not JSON-RPC ({e}):\n{line}\n\nfull stdout:\n{stdout}")
        });
    }
    assert!(seen >= 2, "expected a response per request, got {seen}");

    // The logs must still exist — they were moved, not silenced.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stage") || stderr.contains("INFO"),
        "stage logging should land on stderr, got:\n{stderr}"
    );
}

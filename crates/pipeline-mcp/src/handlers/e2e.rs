//! `pipeline_e2e` handler · Playwright in Docker.
//!
//! Day-6 wires: run, record. Other 8 actions return not_implemented.
//! Browser_launch/close need persistent containers · MVP work.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

const PLAYWRIGHT_IMAGE: &str = "mcr.microsoft.com/playwright:v1.49.0-noble";

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "run" => run(&req.args).await,
        "record" => record(&req.args).await,
        "browser_launch" | "browser_close" | "trace" | "screenshot" | "visual_regression"
        | "a11y_check" | "against_env" | "devtools_eval" => {
            ToolResponse::not_implemented(ToolName::E2e, &req.action)
        }
        other => err(format!("unknown action 'pipeline_e2e.{other}'")),
    }
}

async fn run(args: &Value) -> ToolResponse {
    let suite = args.get("suite").and_then(Value::as_str).unwrap_or("");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let mut docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "test".into(),
    ];
    if !suite.is_empty() {
        docker_args.push(suite.into());
    }
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "e2e_run").await
}

async fn record(args: &Value) -> ToolResponse {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_owned(),
        None => return err("missing 'url'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "codegen".into(),
        url,
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "e2e_record").await
}

async fn capture(program: &str, args: &[&str], cwd: &std::path::Path, label: &str) -> ToolResponse {
    let output = match Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("{program} spawn: {e}")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "command": label,
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": truncate(&String::from_utf8_lossy(&output.stdout), 8_000),
            "stderr": truncate(&String::from_utf8_lossy(&output.stderr), 8_000),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "{label} exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!(
            "{}\n... [truncated · {} more bytes]",
            &s[..max],
            s.len() - max
        )
    }
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

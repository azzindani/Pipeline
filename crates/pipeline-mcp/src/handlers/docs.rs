//! `pipeline_docs` handler · generate · changelog · diagram.
//!
//! Day-6 wires: generate, changelog. Other 4 actions return not_implemented.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "generate" => generate(&req.args).await,
        "changelog" => changelog(&req.args).await,
        "update_from_code" | "diagram" | "publish" | "spec_generate" => {
            ToolResponse::not_implemented(ToolName::Docs, &req.action)
        }
        other => err(format!("unknown action 'pipeline_docs.{other}'")),
    }
}

async fn generate(args: &Value) -> ToolResponse {
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("readme");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (path, content) = match kind {
        "readme" => (cwd.join("README.md"), README_TEMPLATE.to_owned()),
        "runbook" => (cwd.join("docs/RUNBOOK.md"), RUNBOOK_TEMPLATE.to_owned()),
        "onboarding" => (
            cwd.join("docs/ONBOARDING.md"),
            ONBOARDING_TEMPLATE.to_owned(),
        ),
        "api" => (cwd.join("docs/API.md"), API_TEMPLATE.to_owned()),
        "arch" => (cwd.join("docs/ARCHITECTURE.md"), ARCH_TEMPLATE.to_owned()),
        other => {
            return err(format!(
                "unknown kind '{other}' · readme|runbook|onboarding|api|arch"
            ));
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Err(e) = tokio::fs::write(&path, content).await {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"kind": kind, "path": path.display().to_string()}))
}

async fn changelog(args: &Value) -> ToolResponse {
    let from = args.get("from").and_then(Value::as_str);
    let to = args.get("to").and_then(Value::as_str).unwrap_or("HEAD");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let range = match from {
        Some(f) => format!("{f}..{to}"),
        None => to.to_owned(),
    };
    let output = match Command::new("git")
        .args(["log", "--oneline", "--no-decorate", &range])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git log: {e}")),
    };
    let log = String::from_utf8_lossy(&output.stdout).into_owned();
    let lines: Vec<&str> = log.lines().collect();
    ToolResponse {
        ok: output.status.success(),
        data: json!({
            "range": range,
            "commit_count": lines.len(),
            "log": log,
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if output.status.success() {
            None
        } else {
            Some("git log failed".into())
        },
    }
}

const README_TEMPLATE: &str = "# Project\n\n## What\n\nOne-paragraph description.\n\n## Run\n\n```\npipeline run fast\n```\n\n## Develop\n\n```\npipeline dev\n```\n";
const RUNBOOK_TEMPLATE: &str =
    "# Runbook\n\n## Common operations\n\n## Common failures\n\n## Escalation\n";
const ONBOARDING_TEMPLATE: &str = "# Onboarding\n\n## Day 1\n\n## Day 7\n\n## Day 30\n";
const API_TEMPLATE: &str = "# API\n\n## Endpoints\n\n## Errors\n\n## Auth\n";
const ARCH_TEMPLATE: &str =
    "# Architecture\n\n## Components\n\n## Data flow\n\n## Trust boundaries\n";

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

//! `pipeline_docs` handler · generate · changelog · diagram.
//!
//! Day-6 wires: generate, changelog. Other 4 actions return not_implemented.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "generate" => generate(&req.args).await,
        "changelog" => changelog(&req.args).await,
        "update_from_code" => update_from_code(&req.args).await,
        "diagram" => diagram(&req.args).await,
        "publish" => publish(&req.args).await,
        "spec_generate" => spec_generate(&req.args).await,
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

async fn update_from_code(_args: &Value) -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Best-effort: regenerate cargo doc · the agent then merges into READMEs.
    let output = match Command::new("cargo")
        .args(["doc", "--workspace", "--no-deps"])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("cargo doc: {e}")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "command": "cargo doc --workspace --no-deps",
            "exit_code": output.status.code().unwrap_or(-1),
            "out_dir": cwd.join("target/doc").display().to_string(),
        }),
        next_suggested: vec!["pipeline_docs.publish".into()],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some("cargo doc failed".into())
        },
    }
}

#[allow(clippy::unused_async)]
async fn diagram(args: &Value) -> ToolResponse {
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("arch");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("docs/diagrams");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join(format!("{kind}.mmd"));
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    let body = match kind {
        "arch" => {
            "graph TD\n    A[Agent] -->|MCP| B[Pipeline]\n    B --> C[Stages]\n    B --> D[Memory]\n    B --> E[Docker]\n"
        }
        "sequence" => {
            "sequenceDiagram\n    Agent->>Pipeline: pipeline_session.lock\n    Pipeline-->>Agent: handover packet\n    Agent->>Pipeline: pipeline_run.stage(fast)\n    Pipeline-->>Agent: result\n"
        }
        "er" => {
            "erDiagram\n    PROJECT ||--o{ SESSION : has\n    SESSION ||--o{ RUN : produces\n    RUN ||--o{ FAILURE : may_have\n"
        }
        "c4" => {
            "C4Context\n    Person(agent, \"Coding Agent\")\n    System(pipeline, \"Pipeline\", \"Local-first CI/CD\")\n    Rel(agent, pipeline, \"MCP\")\n"
        }
        other => return err(format!("unknown kind '{other}' · arch|sequence|er|c4")),
    };
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"kind": kind, "path": path.display().to_string(), "format": "mermaid"}))
}

async fn publish(args: &Value) -> ToolResponse {
    let target = args
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("github-pages");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Try mkdocs · falls back to a helpful note.
    let output = Command::new("mkdocs")
        .args(["build", "-d", "site"])
        .current_dir(&cwd)
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => ToolResponse::ok(json!({
            "target": target,
            "site_dir": cwd.join("site").display().to_string(),
            "next": "publish site/ to your hosting target (Pages, S3, Cloudflare...)",
        })),
        Ok(o) => err(format!(
            "mkdocs build failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(_) => ToolResponse::ok(json!({
            "target": target,
            "note": "mkdocs not installed · install with `pip install mkdocs` or use your preferred publisher",
        })),
    }
}

#[allow(clippy::unused_async)]
async fn spec_generate(args: &Value) -> ToolResponse {
    let format = args
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("openapi");
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("manual");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("specs");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let (path, body) = match format {
        "openapi" => (
            dir.join("openapi.yaml"),
            "openapi: 3.1.0\ninfo:\n  title: Generated API\n  version: 0.0.1\npaths:\n  /health:\n    get:\n      responses:\n        '200':\n          description: ok\n",
        ),
        "jsonschema" => (
            dir.join("schema.json"),
            "{\n  \"$schema\": \"https://json-schema.org/draft/2020-12/schema\",\n  \"type\": \"object\",\n  \"properties\": {}\n}\n",
        ),
        "asyncapi" => (
            dir.join("asyncapi.yaml"),
            "asyncapi: 3.0.0\ninfo:\n  title: Generated AsyncAPI\n  version: 0.0.1\nchannels: {}\n",
        ),
        "protobuf" => (
            dir.join("schema.proto"),
            "syntax = \"proto3\";\npackage app;\n\nmessage Health { string status = 1; }\n",
        ),
        other => return err(format!("unsupported format '{other}'")),
    };
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(
        json!({"format": format, "source": source, "path": path.display().to_string()}),
    )
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

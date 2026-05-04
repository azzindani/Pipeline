//! `pipeline_meta` handler · explain · version · self_check · config get/set.

use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "version" => ToolResponse::ok(json!({
            "pipeline_mcp": crate::VERSION,
            "pipeline_core": pipeline_core::VERSION,
            "pipeline_config": pipeline_config::VERSION,
            "pipeline_memory": pipeline_memory::VERSION,
            "pipeline_stages": pipeline_stages::VERSION,
        })),
        "self_check" => self_check().await,
        "explain" => explain(&req.args),
        "config_get" | "config_set" => ToolResponse::not_implemented(ToolName::Meta, &req.action),
        other => ToolResponse {
            ok: false,
            data: json!({}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some(format!("unknown action 'pipeline_meta.{other}'")),
        },
    }
}

async fn self_check() -> ToolResponse {
    let cargo = which("cargo").await;
    let docker = which("docker").await;
    let git = which("git").await;
    let rustc = which("rustc").await;
    ToolResponse::ok(json!({
        "cargo": cargo,
        "rustc": rustc,
        "docker": docker,
        "git": git,
        "tools_registered": crate::registry().len(),
    }))
}

async fn which(program: &str) -> Value {
    use tokio::process::Command;
    match Command::new(program).arg("--version").output().await {
        Ok(o) if o.status.success() => json!({
            "found": true,
            "version": String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_owned()
        }),
        _ => json!({"found": false}),
    }
}

fn explain(args: &Value) -> ToolResponse {
    let topic = args.get("topic").and_then(Value::as_str).unwrap_or("");
    let text = match topic {
        "" | "pipeline" => {
            "Pipeline is a local-first, MCP-native CI/CD orchestrator. See CLAUDE.md."
        }
        "stages" => {
            "Five stages: static · unit · container · integration · security. Profiles: fast · full · preflight · confirm."
        }
        "memory" => {
            "SQLite at .pipeline/memory.db · projects · sessions · pipeline_runs · failures · memory_kv tables."
        }
        "tools" => "19 super tools dispatching by `action`. See PLAN.md §3.",
        _ => "Unknown topic. Try: pipeline · stages · memory · tools.",
    };
    ToolResponse::ok(json!({"topic": topic, "text": text}))
}

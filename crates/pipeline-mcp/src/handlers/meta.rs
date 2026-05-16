//! `pipeline_meta` handler · explain · version · self_check · config get/set.

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
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
        "config_get" => config_get(&req.args).await,
        "config_set" => config_set(&req.args).await,
        other => ToolResponse {
            ok: false,
            data: json!({}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some(format!("unknown action 'pipeline_meta.{other}'")),
        },
    }
}

async fn config_get(args: &Value) -> ToolResponse {
    let key = args.get("key").and_then(Value::as_str);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join(".pipeline/config.json");
    let blob: Value = if path.exists() {
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => serde_json::from_str(&s).unwrap_or(json!({})),
            Err(e) => return err(format!("read: {e}")),
        }
    } else {
        json!({})
    };
    let data = match key {
        Some(k) => json!({"key": k, "value": blob.get(k).cloned().unwrap_or(Value::Null)}),
        None => json!({"config": blob}),
    };
    ToolResponse::ok(data)
}

async fn config_set(args: &Value) -> ToolResponse {
    let key = match args.get("key").and_then(Value::as_str) {
        Some(k) => k.to_owned(),
        None => return err("missing 'key'".into()),
    };
    let value = match args.get("value") {
        Some(v) => v.clone(),
        None => return err("missing 'value'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join(".pipeline/config.json");
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    let mut blob: Value = if path.exists() {
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => serde_json::from_str(&s).unwrap_or(json!({})),
            Err(_) => json!({}),
        }
    } else {
        json!({})
    };
    blob[&key] = value.clone();
    let pretty = serde_json::to_string_pretty(&blob).unwrap_or_else(|_| "{}".into());
    if let Err(e) = tokio::fs::write(&path, pretty).await {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"key": key, "value": value, "path": path.display().to_string()}))
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

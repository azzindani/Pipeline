//! `pipeline_security` handler · secret_scan · vuln_scan · dep_audit.
//!
//! Day-6 wires all 3 scan actions via Docker-run on standard images.
//! threat_model + compliance_check return not_implemented (need framework
//! catalog and structured gap reports · MVP+ work).

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

const TRUFFLEHOG_IMAGE: &str = "trufflesecurity/trufflehog:latest";
const TRIVY_IMAGE: &str = "aquasec/trivy:latest";

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "secret_scan" => secret_scan(&req.args).await,
        "vuln_scan" => vuln_scan(&req.args).await,
        "dep_audit" => dep_audit(&req.args).await,
        "threat_model" => threat_model(&req.args).await,
        "compliance_check" => compliance_check(&req.args).await,
        other => err(format!("unknown action 'pipeline_security.{other}'")),
    }
}

async fn secret_scan(args: &Value) -> ToolResponse {
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("filesystem");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let docker_args: Vec<&str> = vec![
        "run",
        "--rm",
        "-v",
        &mount,
        TRUFFLEHOG_IMAGE,
        "filesystem",
        "--no-update",
        "/work",
    ];
    capture(
        "docker",
        &docker_args,
        &cwd,
        &format!("secret_scan({scope})"),
    )
    .await
}

async fn vuln_scan(args: &Value) -> ToolResponse {
    let target = args.get("target").and_then(Value::as_str);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let docker_args: Vec<String> = if let Some(image) = target {
        vec![
            "run".into(),
            "--rm".into(),
            "-v".into(),
            "/var/run/docker.sock:/var/run/docker.sock".into(),
            TRIVY_IMAGE.into(),
            "image".into(),
            "--severity".into(),
            "CRITICAL,HIGH".into(),
            image.to_owned(),
        ]
    } else {
        let mount = format!("{}:/work", cwd.display());
        vec![
            "run".into(),
            "--rm".into(),
            "-v".into(),
            mount,
            TRIVY_IMAGE.into(),
            "fs".into(),
            "--severity".into(),
            "CRITICAL,HIGH".into(),
            "/work".into(),
        ]
    };
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    let label = format!("vuln_scan({})", target.unwrap_or("filesystem"));
    capture("docker", &arr, &cwd, &label).await
}

async fn dep_audit(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (program, cmd_args): (&str, Vec<&str>) = match stack {
        "rust" => ("cargo", vec!["audit"]),
        "node" | "ts" | "typescript" => ("npm", vec!["audit", "--audit-level=high"]),
        "bun" => ("bun", vec!["pm", "audit"]),
        "python" | "python-uv" => ("pip-audit", vec![]),
        other => return err(format!("unsupported stack '{other}'")),
    };
    capture(program, &cmd_args, &cwd, &format!("dep_audit({stack})")).await
}

async fn capture(program: &str, args: &[&str], cwd: &std::path::Path, label: &str) -> ToolResponse {
    let output = match Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("{program} spawn: {e} · is it installed?")),
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

#[allow(clippy::unused_async)]
async fn threat_model(args: &Value) -> ToolResponse {
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("application");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("security");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join("threat-model.md");
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    let body = format!(
        "# Threat model · scope = {scope}\n\nGenerated by pipeline_security.threat_model.\n\n## Spoofing\n\n## Tampering\n\n## Repudiation\n\n## Information disclosure\n\n## Denial of service\n\n## Elevation of privilege\n\n## Trust boundaries\n\n## Mitigations\n\n## Open questions\n"
    );
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(
        json!({"scope": scope, "path": path.display().to_string(), "framework": "STRIDE"}),
    )
}

#[allow(clippy::unused_async)]
async fn compliance_check(args: &Value) -> ToolResponse {
    let framework = args
        .get("framework")
        .and_then(Value::as_str)
        .unwrap_or("standards");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Cheap heuristic checks · agent extends.
    let mut checks: Vec<Value> = Vec::new();
    let dockerfile = cwd.join("Dockerfile");
    if dockerfile.exists() {
        let body = std::fs::read_to_string(&dockerfile).unwrap_or_default();
        checks.push(json!({
            "name": "dockerfile_non_root",
            "passed": body.contains("USER ") && !body.contains("USER root"),
        }));
        checks.push(json!({
            "name": "dockerfile_pinned_base",
            "passed": !body.contains(":latest"),
        }));
    }
    let env_file = cwd.join(".env");
    checks.push(
        json!({"name": "no_committed_env", "passed": !env_file.exists() || ignored(".env", &cwd)}),
    );
    let gitignore = cwd.join(".gitignore");
    checks.push(json!({"name": "has_gitignore", "passed": gitignore.exists()}));
    checks.push(json!({"name": "has_pipeline_yaml", "passed": cwd.join("pipeline.yaml").exists()}));

    let passed = checks
        .iter()
        .filter(|c| c.get("passed").and_then(Value::as_bool) == Some(true))
        .count();
    let total = checks.len();
    #[allow(clippy::cast_precision_loss)]
    let score = if total == 0 {
        0.0
    } else {
        (passed as f64 / total as f64) * 100.0
    };
    ToolResponse::ok(json!({
        "framework": framework,
        "checks": checks,
        "score_percent": score,
        "passed": passed,
        "total": total,
    }))
}

fn ignored(needle: &str, cwd: &std::path::Path) -> bool {
    let g = cwd.join(".gitignore");
    std::fs::read_to_string(&g).is_ok_and(|s| s.lines().any(|l| l.trim() == needle))
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

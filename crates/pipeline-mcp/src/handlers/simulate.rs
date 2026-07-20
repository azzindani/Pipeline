//! `pipeline_simulate` handler · personas · journeys · use cases.
//!
//! Day-6 wires: persona_create, journey_define, use_case_define.
//! journey_simulate, load, chaos_inject return not_implemented (need
//! containerized load harness · MVP+).

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "persona_create" => persona_create(&req.args, state).await,
        "journey_define" => journey_define(&req.args, state).await,
        "use_case_define" => use_case_define(&req.args, state).await,
        "journey_simulate" => journey_simulate(&req.args, state).await,
        "load" => load_simulate(&req.args).await,
        "chaos_inject" => chaos_inject(&req.args).await,
        other => err(format!("unknown action 'pipeline_simulate.{other}'")),
    }
}

async fn persona_create(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let role = match args.get("role").and_then(Value::as_str) {
        Some(r) => r.to_owned(),
        None => return err("missing 'role'".into()),
    };
    let goals = args.get("goals").cloned().unwrap_or(json!([]));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let blob =
        json!({"id": id, "role": role, "goals": goals, "ts": pipeline_memory::now_rfc3339()});
    if let Err(e) = mem
        .remember(&project, "persona", &id, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse {
        ok: true,
        data: blob,
        next_suggested: vec!["pipeline_simulate.journey_define".into()],
        memory_refs: vec![format!("persona:{id}")],
        error: None,
    }
}

async fn journey_define(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let persona = args
        .get("persona")
        .and_then(Value::as_str)
        .unwrap_or("anonymous")
        .to_owned();
    let steps = args.get("steps").cloned().unwrap_or(json!([]));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let blob =
        json!({"id": id, "persona": persona, "steps": steps, "ts": pipeline_memory::now_rfc3339()});
    if let Err(e) = mem
        .remember(&project, "journey", &id, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(blob)
}

async fn use_case_define(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let actor = match args.get("actor").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'actor'".into()),
    };
    let intent = args.get("intent").and_then(Value::as_str).unwrap_or("");
    let flow = args.get("flow").cloned().unwrap_or(json!([]));
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let id = Uuid::new_v4().to_string();
    let blob = json!({
        "id": id, "actor": actor, "intent": intent, "flow": flow,
        "ts": pipeline_memory::now_rfc3339()
    });
    if let Err(e) = mem
        .remember(&project, "use_case", &id, &blob.to_string())
        .await
    {
        return err(e.to_string());
    }
    ToolResponse::ok(blob)
}

async fn cfg_project(state: &Arc<ServerState>) -> Result<String, String> {
    if let Some(p) = state.project_id.lock().await.clone() {
        return Ok(p);
    }
    load_config_in_cwd().map(|c| c.project)
}

async fn journey_simulate(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let journey_id = match args.get("journey").and_then(Value::as_str) {
        Some(j) => j.to_owned(),
        None => return err("missing 'journey' (id)".into()),
    };
    let count = args.get("count").and_then(Value::as_u64).unwrap_or(10);
    let project = match cfg_project(&state).await {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(e),
    };
    let blob = match mem.recall(&project, "journey", &journey_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return err(format!("journey '{journey_id}' not found")),
        Err(e) => return err(e.to_string()),
    };
    let journey: Value = serde_json::from_str(&blob).unwrap_or(json!({}));
    let steps = journey
        .get("steps")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    // Dry-run · agent later wires k6/locust scripts. Returns simulated metrics.
    ToolResponse::ok(json!({
        "journey": journey_id,
        "concurrency": count,
        "steps_per_run": steps,
        "simulated_total_steps": count * steps as u64,
        "note": "Day-9 dry-run · k6/locust execution lands MVP+",
    }))
}

async fn load_simulate(args: &Value) -> ToolResponse {
    let target = match args.get("target").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'target' (URL)".into()),
    };
    let profile = args
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or("smoke");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // k6 in Docker · simple baseline script generated inline.
    let script_dir = cwd.join("load");
    if let Err(e) = std::fs::create_dir_all(&script_dir) {
        return err(format!("mkdir: {e}"));
    }
    let script = script_dir.join(format!("{profile}.js"));
    if !script.exists() {
        let (vus, dur) = match profile {
            "smoke" => (1, "10s"),
            "load" => (50, "1m"),
            "stress" => (200, "2m"),
            other => return err(format!("unknown profile '{other}' · smoke|load|stress")),
        };
        let body = format!(
            "import http from 'k6/http';\nimport {{ check, sleep }} from 'k6';\nexport const options = {{ vus: {vus}, duration: '{dur}' }};\nexport default function () {{\n  const r = http.get('{target}');\n  check(r, {{ '200 OK': (res) => res.status === 200 }});\n  sleep(0.5);\n}}\n"
        );
        if let Err(e) = std::fs::write(&script, body) {
            return err(format!("write: {e}"));
        }
    }
    // ! The script must be bind-mounted · passing the mount as an env pair
    // emitted `-e MOUNT=...` and k6 never saw the file.
    let host = cwd.display().to_string();
    let script_rel = format!("/work/load/{profile}.js");
    match pipeline_docker::run_image(
        "grafana/k6:latest",
        &["run", &script_rel],
        &[],
        &[(host.as_str(), "/work")],
    )
    .await
    {
        Ok(out) => ToolResponse {
            ok: out.ok(),
            data: json!({
                "target": target,
                "profile": profile,
                "script": script.display().to_string(),
                "stdout": out.stdout,
                "stderr": out.stderr,
                "exit_code": out.exit_code,
            }),
            next_suggested: vec!["pipeline_observe.perf_compare".into()],
            memory_refs: vec![],
            error: if out.ok() {
                None
            } else {
                Some("k6 run failed".into())
            },
        },
        Err(e) => err(e.to_string()),
    }
}

async fn chaos_inject(args: &Value) -> ToolResponse {
    let service = match args.get("service").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'service' (container name or compose service)".into()),
    };
    let fault = args.get("fault").and_then(Value::as_str).unwrap_or("kill");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Use docker pause/kill/network · simple chaos primitives.
    let cmd_args: Vec<&str> = match fault {
        "kill" => vec!["kill", &service],
        "pause" => vec!["pause", &service],
        "unpause" => vec!["unpause", &service],
        "stop" => vec!["stop", &service],
        other => return err(format!("unknown fault '{other}' · kill|pause|unpause|stop")),
    };
    let output = match tokio::process::Command::new("docker")
        .args(&cmd_args)
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("docker: {e}")),
    };
    ToolResponse {
        ok: output.status.success(),
        data: json!({
            "service": service,
            "fault": fault,
            "exit_code": output.status.code().unwrap_or(-1),
            "stderr": String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        next_suggested: vec!["pipeline_run.status".into()],
        memory_refs: vec![],
        error: if output.status.success() {
            None
        } else {
            Some(format!("chaos {fault} failed"))
        },
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

//! `pipeline_docker` handler · build · run · compose · image ops.
//!
//! Backed by `pipeline-docker` (shell-out wrapper). Day-4b ships the
//! 16-action surface end-to-end except scan/promote which need Trivy
//! and registry login flows respectively (deferred to MVP).

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use pipeline_docker::CommandOutput;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "build" => build(&req.args).await,
        "run" => run_image(&req.args).await,
        "exec" => exec(&req.args).await,
        "logs" => logs(&req.args).await,
        "inspect" => inspect(&req.args).await,
        "rm" => rm(&req.args).await,
        "compose_up" => compose_up(&req.args).await,
        "compose_down" => compose_down(&req.args).await,
        "compose_ps" => compose_ps(&req.args).await,
        "compose_logs" => compose_logs(&req.args).await,
        "image_push" => image_push(&req.args).await,
        "image_pull" => image_pull(&req.args).await,
        "dockerfile_lint" => dockerfile_lint(&req.args).await,
        "image_scan" | "image_promote" | "dockerfile_generate" => {
            ToolResponse::not_implemented(ToolName::Docker, &req.action)
        }
        other => err(format!("unknown action 'pipeline_docker.{other}'")),
    }
}

fn cwd() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| e.to_string())
}

async fn build(args: &Value) -> ToolResponse {
    let tag = match args.get("tag").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'tag'".into()),
    };
    let context = match args
        .get("context")
        .and_then(Value::as_str)
        .map(PathBuf::from)
    {
        Some(p) => p,
        None => match cwd() {
            Ok(p) => p,
            Err(e) => return err(e),
        },
    };
    let dockerfile = args
        .get("dockerfile")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let target = args.get("target").and_then(Value::as_str);

    match pipeline_docker::build(&context, &tag, dockerfile.as_deref(), target).await {
        Ok(out) => command_output_response(&out, &format!("build {tag}")),
        Err(e) => err(e.to_string()),
    }
}

async fn run_image(args: &Value) -> ToolResponse {
    let image = match args.get("image").and_then(Value::as_str) {
        Some(i) => i.to_owned(),
        None => return err("missing 'image'".into()),
    };
    let cmd_vec: Vec<String> = args
        .get("cmd")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let cmd_refs: Vec<&str> = cmd_vec.iter().map(String::as_str).collect();

    let env_pairs: Vec<(String, String)> = args
        .get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default();

    match pipeline_docker::run_image(&image, &cmd_refs, &env_pairs).await {
        Ok(out) => command_output_response(&out, &format!("run {image}")),
        Err(e) => err(e.to_string()),
    }
}

async fn exec(args: &Value) -> ToolResponse {
    let container = match args.get("container").and_then(Value::as_str) {
        Some(c) => c.to_owned(),
        None => return err("missing 'container'".into()),
    };
    let cmd_vec: Vec<String> = args
        .get("cmd")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if cmd_vec.is_empty() {
        return err("missing 'cmd' (array)".into());
    }
    let cmd_refs: Vec<&str> = cmd_vec.iter().map(String::as_str).collect();
    match pipeline_docker::exec(&container, &cmd_refs).await {
        Ok(out) => command_output_response(&out, &format!("exec {container}")),
        Err(e) => err(e.to_string()),
    }
}

async fn logs(args: &Value) -> ToolResponse {
    let container = match args.get("container").and_then(Value::as_str) {
        Some(c) => c.to_owned(),
        None => return err("missing 'container'".into()),
    };
    let tail = args
        .get("tail")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    match pipeline_docker::logs(&container, tail).await {
        Ok(out) => command_output_response(&out, &format!("logs {container}")),
        Err(e) => err(e.to_string()),
    }
}

async fn inspect(args: &Value) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    match pipeline_docker::inspect(&name).await {
        Ok(v) => ToolResponse::ok(json!({"inspect": v})),
        Err(e) => err(e.to_string()),
    }
}

async fn rm(args: &Value) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    match pipeline_docker::rm(&name, force).await {
        Ok(out) => command_output_response(&out, &format!("rm {name}")),
        Err(e) => err(e.to_string()),
    }
}

async fn compose_up(args: &Value) -> ToolResponse {
    let file = compose_file_path(args).map_err(err);
    let file = match file {
        Ok(p) => p,
        Err(r) => return r,
    };
    let services: Vec<String> = args
        .get("services")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    match pipeline_docker::compose_up(&file, &services).await {
        Ok(out) => command_output_response(&out, "compose up"),
        Err(e) => err(e.to_string()),
    }
}

async fn compose_down(args: &Value) -> ToolResponse {
    let file = match compose_file_path(args) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    match pipeline_docker::compose_down(&file).await {
        Ok(out) => command_output_response(&out, "compose down"),
        Err(e) => err(e.to_string()),
    }
}

async fn compose_ps(args: &Value) -> ToolResponse {
    let file = match compose_file_path(args) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    match pipeline_docker::compose_ps(&file).await {
        Ok(out) => command_output_response(&out, "compose ps"),
        Err(e) => err(e.to_string()),
    }
}

async fn compose_logs(args: &Value) -> ToolResponse {
    let file = match compose_file_path(args) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let service = args.get("service").and_then(Value::as_str);
    match pipeline_docker::compose_logs(&file, service).await {
        Ok(out) => command_output_response(&out, "compose logs"),
        Err(e) => err(e.to_string()),
    }
}

async fn image_pull(args: &Value) -> ToolResponse {
    let image = match args.get("image").and_then(Value::as_str) {
        Some(i) => i.to_owned(),
        None => return err("missing 'image'".into()),
    };
    match pipeline_docker::image_pull(&image).await {
        Ok(out) => command_output_response(&out, &format!("pull {image}")),
        Err(e) => err(e.to_string()),
    }
}

async fn image_push(args: &Value) -> ToolResponse {
    let image = match args.get("image").and_then(Value::as_str) {
        Some(i) => i.to_owned(),
        None => return err("missing 'image'".into()),
    };
    match pipeline_docker::image_push(&image).await {
        Ok(out) => command_output_response(&out, &format!("push {image}")),
        Err(e) => err(e.to_string()),
    }
}

async fn dockerfile_lint(args: &Value) -> ToolResponse {
    // hadolint runs as a Docker container · pulls on first invocation.
    let path = args
        .get("dockerfile")
        .and_then(Value::as_str)
        .unwrap_or("Dockerfile");
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mount = format!("{}:/work", cwd.display());
    match pipeline_docker::run_image(
        "hadolint/hadolint:latest",
        &["hadolint", &format!("/work/{path}")],
        &[],
    )
    .await
    {
        Ok(out) => {
            let mut resp = command_output_response(&out, "dockerfile_lint");
            // hadolint exits non-zero on findings; surface them but don't mark as transport error.
            resp.next_suggested = vec!["pipeline_docker.dockerfile_generate".into()];
            resp.data["mount"] = json!(mount);
            resp
        }
        Err(e) => err(e.to_string()),
    }
}

// ---------- helpers ----------

fn compose_file_path(args: &Value) -> Result<PathBuf, String> {
    let name = args
        .get("file")
        .or_else(|| args.get("compose_file"))
        .and_then(Value::as_str)
        .unwrap_or("docker-compose.yml");
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(cwd.join(name))
}

fn command_output_response(out: &CommandOutput, label: &str) -> ToolResponse {
    let ok = out.ok();
    ToolResponse {
        ok,
        data: json!({
            "command": label,
            "exit_code": out.exit_code,
            "duration_ms": out.duration_ms,
            "stdout": truncate(&out.stdout, 16_000),
            "stderr": truncate(&out.stderr, 16_000),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!("docker {label} exit {}", out.exit_code))
        },
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        let mut t = s[..max].to_owned();
        write!(t, "\n... [truncated · {} more bytes]", s.len() - max).ok();
        t
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

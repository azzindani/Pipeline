//! `pipeline_docker` handler · build · run · compose · image ops.
//!
//! Backed by `pipeline-docker` (shell-out wrapper). Day-4b ships the
//! 16-action surface end-to-end except scan/promote which need Trivy
//! and registry login flows respectively (deferred to MVP).

#![allow(clippy::doc_markdown)]

use crate::handlers::security;
use crate::scanners;
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use pipeline_docker::CommandOutput;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

/// In-container mount point for the working tree.
const MOUNT_POINT: &str = "/work";

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
        "image_scan" => image_scan(&req.args).await,
        "image_promote" => image_promote(&req.args).await,
        "dockerfile_generate" => dockerfile_generate(&req.args).await,
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

    match pipeline_docker::run_image(&image, &cmd_refs, &env_pairs, &[]).await {
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
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("Dockerfile");
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    // ! The file must reach the container: without the mount hadolint failed
    // with `withBinaryFile: does not exist` while the response advertised a
    // bind mount that was never created.
    if !cwd.join(path).is_file() {
        return err(format!("no Dockerfile at {}", cwd.join(path).display()));
    }
    let host = cwd.display().to_string();
    let target = format!("{MOUNT_POINT}/{path}");
    let out = match pipeline_docker::run_image(
        scanners::HADOLINT_IMAGE,
        &["hadolint", "--format", "json", &target],
        &[],
        &[(host.as_str(), MOUNT_POINT)],
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return err(e.to_string()),
    };
    hadolint_response(path, &out)
}

/// hadolint exits 1 on findings · that exit is the gate, ✗ a transport error.
fn hadolint_response(path: &str, out: &CommandOutput) -> ToolResponse {
    let findings: Vec<Value> = serde_json::from_str(out.stdout.trim()).unwrap_or_default();
    // No parseable report and a nonzero exit means hadolint never linted · say
    // so rather than let an empty finding list read as a clean Dockerfile.
    let ran = !out.stdout.trim().is_empty() || out.ok();
    let ok = out.ok() && findings.is_empty();
    let error = if ok {
        None
    } else if ran {
        Some(format!(
            "dockerfile_lint({path}) · {} finding(s)",
            findings.len()
        ))
    } else {
        Some(format!(
            "dockerfile_lint({path}) did not run · UNKNOWN, ✗ clean · exit {} · {}",
            out.exit_code,
            scanners::tail(&out.stderr, 400)
        ))
    };
    ToolResponse {
        ok,
        data: json!({
            "command": format!("dockerfile_lint({path})"),
            "path": path,
            "linter": "hadolint",
            "status": lint_status(ok, ran),
            "exit_code": out.exit_code,
            "findings_count": findings.len(),
            "findings": findings,
            "stderr": truncate(&out.stderr, 4_000),
        }),
        next_suggested: vec!["pipeline_docker.dockerfile_generate".into()],
        memory_refs: vec![],
        error,
    }
}

fn lint_status(ok: bool, ran: bool) -> &'static str {
    if ok {
        "clean"
    } else if ran {
        "findings"
    } else {
        "linter_unavailable"
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

async fn image_scan(args: &Value) -> ToolResponse {
    let image = match args.get("image").and_then(Value::as_str) {
        Some(i) => i.to_owned(),
        None => return err("missing 'image'".into()),
    };
    let severity = match scanners::normalise_severity(
        args.get("severity")
            .and_then(Value::as_str)
            .unwrap_or("CRITICAL,HIGH"),
    ) {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    // ! The socket is what lets trivy resolve a locally-built tag · without it
    // every scan of an unpublished image dies as "unable to find the image".
    security::run_trivy(
        scanners::TrivyTarget::Image(&image),
        &severity,
        &[("/var/run/docker.sock", "/var/run/docker.sock")],
        &format!("image_scan({image})"),
    )
    .await
}

async fn image_promote(args: &Value) -> ToolResponse {
    let image = match args.get("image").and_then(Value::as_str) {
        Some(i) => i.to_owned(),
        None => return err("missing 'image'".into()),
    };
    let registry = match args.get("registry").and_then(Value::as_str) {
        Some(r) => r.to_owned(),
        None => return err("missing 'registry' (e.g. ghcr.io/owner)".into()),
    };
    let tag = args.get("tag").and_then(Value::as_str).unwrap_or("latest");
    // docker tag <image> <registry>/<image_basename>:<tag> · then push.
    let basename = image.rsplit('/').next().unwrap_or(&image);
    let stripped_tag = basename.split(':').next().unwrap_or(basename);
    let target = format!("{registry}/{stripped_tag}:{tag}");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let tag_out = match tokio::process::Command::new("docker")
        .args(["tag", &image, &target])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("docker tag: {e}")),
    };
    if !tag_out.status.success() {
        return err(format!(
            "docker tag failed: {}",
            String::from_utf8_lossy(&tag_out.stderr).trim()
        ));
    }
    match pipeline_docker::image_push(&target).await {
        Ok(out) => command_output_response(&out, &format!("image_promote({target})")),
        Err(e) => err(e.to_string()),
    }
}

async fn dockerfile_generate(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join("Dockerfile");
    if path.exists() {
        return err(format!(
            "Dockerfile exists at {} · refusing to overwrite",
            path.display()
        ));
    }
    let body = match stack {
        "rust" => RUST_DOCKERFILE,
        "node" | "ts" | "typescript" => NODE_DOCKERFILE,
        "bun" => BUN_DOCKERFILE,
        "python" | "python-uv" | "uv" => PYTHON_DOCKERFILE,
        "go" | "golang" => GO_DOCKERFILE,
        other => return err(format!("no template for stack '{other}'")),
    };
    if let Err(e) = tokio::fs::write(&path, body).await {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({
        "stack": stack,
        "path": path.display().to_string(),
        "next": "pipeline_docker.dockerfile_lint",
    }))
}

const RUST_DOCKERFILE: &str = "FROM rust:1.94-slim-bookworm AS builder\nWORKDIR /usr/src/app\nRUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*\nCOPY . .\nRUN cargo build --release\n\nFROM debian:bookworm-slim\nRUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd -r -u 10001 app\nCOPY --from=builder /usr/src/app/target/release/app /usr/local/bin/app\nUSER app\nENTRYPOINT [\"/usr/local/bin/app\"]\n";

const NODE_DOCKERFILE: &str = "FROM node:22-bookworm-slim AS builder\nWORKDIR /app\nCOPY package*.json ./\nRUN npm ci\nCOPY . .\nRUN npm run build\n\nFROM node:22-bookworm-slim\nWORKDIR /app\nRUN useradd -r -u 10001 app\nCOPY --from=builder /app/dist ./dist\nCOPY --from=builder /app/node_modules ./node_modules\nCOPY package.json ./\nUSER app\nCMD [\"node\", \"dist/index.js\"]\n";

const BUN_DOCKERFILE: &str = "FROM oven/bun:1 AS builder\nWORKDIR /app\nCOPY bun.lockb package.json ./\nRUN bun install --frozen-lockfile\nCOPY . .\nRUN bun build src/index.ts --outdir dist\n\nFROM oven/bun:1-slim\nWORKDIR /app\nRUN groupadd -r app && useradd -r -g app -u 10001 app\nCOPY --from=builder /app/dist ./dist\nUSER app\nCMD [\"bun\", \"dist/index.js\"]\n";

const PYTHON_DOCKERFILE: &str = "FROM python:3.13-slim-bookworm AS builder\nWORKDIR /app\nRUN pip install --no-cache-dir uv\nCOPY pyproject.toml uv.lock ./\nRUN uv sync --frozen --no-dev\nCOPY . .\n\nFROM python:3.13-slim-bookworm\nWORKDIR /app\nRUN useradd -r -u 10001 app\nCOPY --from=builder /app /app\nENV PATH=/app/.venv/bin:$PATH\nUSER app\nCMD [\"python\", \"-m\", \"app\"]\n";

const GO_DOCKERFILE: &str = "FROM golang:1.23-bookworm AS builder\nWORKDIR /usr/src/app\nCOPY go.* ./\nRUN go mod download\nCOPY . .\nRUN CGO_ENABLED=0 go build -o /out/app ./...\n\nFROM gcr.io/distroless/static-debian12\nCOPY --from=builder /out/app /app\nUSER 10001:10001\nENTRYPOINT [\"/app\"]\n";

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

#[cfg(test)]
mod scan_and_lint_tests {
    use super::{MOUNT_POINT, hadolint_response};
    use crate::scanners::{TrivyTarget, trivy_cmd};
    use pipeline_docker::{CommandOutput, run_image_args};

    fn out(code: i32, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.into(),
            stderr: stderr.into(),
            exit_code: code,
            duration_ms: 1,
        }
    }

    const HADOLINT_JSON: &str = r#"[{"code":"DL3008","column":1,"file":"/work/Dockerfile","level":"warning","line":20,"message":"Pin versions in apt get install"}]"#;

    #[test]
    fn a_lint_that_finds_problems_fails_the_call() {
        let resp = hadolint_response("Dockerfile", &out(1, HADOLINT_JSON, ""));
        assert!(!resp.ok);
        assert_eq!(resp.data["status"], "findings");
        assert_eq!(resp.data["findings_count"], 1);
        assert_eq!(resp.data["findings"][0]["code"], "DL3008");
    }

    #[test]
    fn a_clean_dockerfile_passes_the_call() {
        let resp = hadolint_response("Dockerfile", &out(0, "[]", ""));
        assert!(resp.ok);
        assert_eq!(resp.data["status"], "clean");
        assert!(resp.error.is_none());
    }

    #[test]
    fn a_linter_that_never_ran_is_unknown_not_clean() {
        // ! Verified live before the mount fix: hadolint died with
        // `withBinaryFile: does not exist` and produced no findings.
        let resp = hadolint_response(
            "Dockerfile",
            &out(
                1,
                "",
                "hadolint: /work/Dockerfile: withBinaryFile: does not exist",
            ),
        );
        assert!(!resp.ok);
        assert_eq!(resp.data["status"], "linter_unavailable");
        assert!(resp.error.unwrap().contains("UNKNOWN"));
    }

    #[test]
    fn the_lint_response_no_longer_advertises_a_mount_it_did_not_create() {
        // Regression: the old response carried a "mount" field asserting a bind
        // mount that run_image never emitted.
        let resp = hadolint_response("Dockerfile", &out(0, "[]", ""));
        assert!(resp.data.get("mount").is_none());
    }

    #[test]
    fn the_lint_command_bind_mounts_the_worktree() {
        let args = run_image_args(
            "hadolint/hadolint:latest",
            &["hadolint", "--format", "json", "/work/Dockerfile"],
            &[],
            &[("/host/repo", MOUNT_POINT)],
        );
        assert!(args.contains(&"/host/repo:/work".to_owned()));
    }

    #[test]
    fn image_scan_mounts_the_docker_socket_and_fails_on_findings() {
        // ! Without the socket trivy cannot resolve a locally-built tag and
        // returns FATAL — a scan that passes everything by never running.
        let cmd = trivy_cmd(TrivyTarget::Image("local:dev"), "CRITICAL,HIGH");
        let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
        let args = run_image_args(
            "aquasec/trivy:latest",
            &refs,
            &[],
            &[("/var/run/docker.sock", "/var/run/docker.sock")],
        );
        assert!(args.contains(&"/var/run/docker.sock:/var/run/docker.sock".to_owned()));
        let i = args.iter().position(|a| a == "--exit-code").expect("flag");
        assert_eq!(args[i + 1], "1");
    }
}

//! `pipeline_env` handler · environment scaffolding · deps · secrets.
//!
//! Day-6 wires: create, deps_install, deps_audit, secrets_setup.
//! Other 6 actions return not_implemented (MVP).

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "create" => create(&req.args).await,
        "deps_install" => deps_install(&req.args).await,
        "deps_audit" => deps_audit().await,
        "deps_update" => deps_update(&req.args).await,
        "deps_lock" => deps_lock(&req.args).await,
        "runtime_provision" => runtime_provision(&req.args).await,
        "tooling_install" => tooling_install(&req.args).await,
        "secrets_setup" => secrets_setup(&req.args).await,
        "secrets_inject" => secrets_inject(&req.args).await,
        "devcontainer_open" => devcontainer_open(&req.args).await,
        other => err(format!("unknown action 'pipeline_env.{other}'")),
    }
}

async fn deps_update(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (program, cmd_args): (&str, Vec<&str>) = match stack {
        "rust" => ("cargo", vec!["update"]),
        "node" | "ts" | "typescript" => ("npm", vec!["update"]),
        "bun" => ("bun", vec!["update"]),
        "python" | "python-uv" | "uv" => ("uv", vec!["lock", "--upgrade"]),
        "go" | "golang" => ("go", vec!["get", "-u", "./..."]),
        other => return err(format!("unsupported stack '{other}'")),
    };
    run_capture(program, &cmd_args, &cwd, &format!("deps_update({stack})")).await
}

async fn deps_lock(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (program, cmd_args): (&str, Vec<&str>) = match stack {
        "rust" => ("cargo", vec!["generate-lockfile"]),
        "node" | "ts" | "typescript" => ("npm", vec!["install", "--package-lock-only"]),
        "bun" => ("bun", vec!["install", "--frozen-lockfile"]),
        "python" | "python-uv" | "uv" => ("uv", vec!["lock"]),
        "go" | "golang" => ("go", vec!["mod", "tidy"]),
        other => return err(format!("unsupported stack '{other}'")),
    };
    run_capture(program, &cmd_args, &cwd, &format!("deps_lock({stack})")).await
}

async fn runtime_provision(args: &Value) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name' (rust|python|node|bun|go)".into()),
    };
    let version = args
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("stable");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Strategy: write a `.tool-versions` file (asdf-compatible) so the
    // runtime is declarable in the repo. Actually fetching it is
    // delegated to the host (asdf, mise, etc.) or to the devcontainer.
    let path = cwd.join(".tool-versions");
    let mut existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let line = format!("{name} {version}\n");
    if !existing.lines().any(|l| l.starts_with(&format!("{name} "))) {
        existing.push_str(&line);
        if let Err(e) = tokio::fs::write(&path, &existing).await {
            return err(format!("write .tool-versions: {e}"));
        }
    }
    ToolResponse::ok(json!({
        "name": name,
        "version": version,
        "path": path.display().to_string(),
        "note": "asdf/mise compatible · run `asdf install` to materialize",
    }))
}

async fn tooling_install(args: &Value) -> ToolResponse {
    let kind = match args.get("kind").and_then(Value::as_str) {
        Some(k) => k.to_owned(),
        None => return err("missing 'kind' (linter|formatter|lsp|coverage)".into()),
    };
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (program, cmd_args): (&str, Vec<&str>) = match (stack, kind.as_str()) {
        ("rust", "linter") => ("rustup", vec!["component", "add", "clippy"]),
        ("rust", "formatter") => ("rustup", vec!["component", "add", "rustfmt"]),
        ("rust", "lsp") => ("rustup", vec!["component", "add", "rust-analyzer"]),
        ("rust", "coverage") => ("cargo", vec!["install", "cargo-llvm-cov"]),
        (_, other) => return err(format!("no install for {stack} kind={other}")),
    };
    run_capture(
        program,
        &cmd_args,
        &cwd,
        &format!("tooling_install({stack},{kind})"),
    )
    .await
}

async fn secrets_inject(args: &Value) -> ToolResponse {
    let stage = args.get("stage").and_then(Value::as_str).unwrap_or("dev");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Read .env.<stage>.example or .env.example, copy keys with placeholder values
    // into a runtime .env (only if it doesn't exist already · never overwrite).
    let staged = cwd.join(format!(".env.{stage}.example"));
    let fallback = cwd.join(".env.example");
    let src = if staged.exists() { staged } else { fallback };
    if !src.exists() {
        return err(format!("no template found at {}", src.display()));
    }
    let target = cwd.join(".env");
    if target.exists() {
        return err("'.env' exists · refusing to overwrite".into());
    }
    if let Err(e) = tokio::fs::copy(&src, &target).await {
        return err(format!("copy: {e}"));
    }
    ToolResponse::ok(json!({
        "stage": stage,
        "source": src.display().to_string(),
        "target": target.display().to_string(),
        "note": "placeholders only · fill in via vault / secret manager before running",
    }))
}

async fn devcontainer_open(args: &Value) -> ToolResponse {
    let editor = args.get("editor").and_then(Value::as_str).unwrap_or("code");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dc = cwd.join(".devcontainer/devcontainer.json");
    if !dc.exists() {
        return err("no .devcontainer/devcontainer.json · run pipeline_env.create first".into());
    }
    // Try to launch the editor with the devcontainer · falls back gracefully if absent.
    let output = match Command::new(editor)
        .args([
            ".",
            "--folder-uri",
            "vscode-remote://dev-container+pipeline/workspace",
        ])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return ToolResponse::ok(json!({
                "editor": editor,
                "devcontainer": dc.display().to_string(),
                "launched": false,
                "note": format!("'{editor}' not found ({e}) · open the folder manually in your IDE"),
            }));
        }
    };
    ToolResponse::ok(json!({
        "editor": editor,
        "devcontainer": dc.display().to_string(),
        "launched": output.status.success(),
    }))
}

async fn create(args: &Value) -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let profile = args
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let mut written: Vec<String> = Vec::new();

    let env_path = cwd.join(".env.example");
    if !env_path.exists() {
        if let Err(e) = tokio::fs::write(&env_path, ENV_EXAMPLE).await {
            return err(format!("write .env.example: {e}"));
        }
        written.push(env_path.display().to_string());
    }

    let dc_dir = cwd.join(".devcontainer");
    if let Err(e) = tokio::fs::create_dir_all(&dc_dir).await {
        return err(format!("mkdir .devcontainer: {e}"));
    }
    let dc_path = dc_dir.join("devcontainer.json");
    if !dc_path.exists() {
        if let Err(e) = tokio::fs::write(&dc_path, DEVCONTAINER_JSON).await {
            return err(format!("write devcontainer.json: {e}"));
        }
        written.push(dc_path.display().to_string());
    }

    ToolResponse {
        ok: true,
        data: json!({"profile": profile, "files_written": written}),
        next_suggested: vec![
            "pipeline_env.deps_install".into(),
            "pipeline_env.secrets_setup".into(),
        ],
        memory_refs: vec![],
        error: None,
    }
}

/// Install dependencies · **add** named packages, or sync what is declared.
///
/// ! `packages` used to be ignored entirely: every stack ran its sync command
/// (`cargo fetch`, `npm install`) and reported `ok` with exit 0, so asking to
/// add a crate was a silent no-op that looked like success. Named packages now
/// run the stack's *add* command, and `manifest` targets one workspace member.
async fn deps_install(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let packages: Vec<String> = args
        .get("packages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // Which member of a workspace · ignored by stacks with a single manifest.
    let manifest = args.get("manifest").and_then(Value::as_str);

    let mut cmd_args: Vec<String> = Vec::new();
    let program = match (stack, packages.is_empty()) {
        ("rust", true) => {
            cmd_args.push("fetch".into());
            "cargo"
        }
        ("rust", false) => {
            cmd_args.push("add".into());
            cmd_args.extend(packages.iter().cloned());
            if let Some(m) = manifest {
                cmd_args.push("--manifest-path".into());
                cmd_args.push(m.to_owned());
            }
            "cargo"
        }
        ("node" | "ts" | "typescript", empty) => {
            cmd_args.push("install".into());
            if !empty {
                cmd_args.extend(packages.iter().cloned());
            }
            "npm"
        }
        ("bun", true) => {
            cmd_args.push("install".into());
            "bun"
        }
        ("bun", false) => {
            cmd_args.push("add".into());
            cmd_args.extend(packages.iter().cloned());
            "bun"
        }
        ("python" | "python-uv" | "uv", true) => {
            cmd_args.push("sync".into());
            "uv"
        }
        ("python" | "python-uv" | "uv", false) => {
            cmd_args.push("add".into());
            cmd_args.extend(packages.iter().cloned());
            "uv"
        }
        ("go" | "golang", true) => {
            cmd_args.extend(["mod".to_owned(), "download".to_owned()]);
            "go"
        }
        ("go" | "golang", false) => {
            cmd_args.push("get".into());
            cmd_args.extend(packages.iter().cloned());
            "go"
        }
        (other, _) => return err(format!("unsupported stack '{other}'")),
    };

    let borrowed: Vec<&str> = cmd_args.iter().map(String::as_str).collect();
    let label = if packages.is_empty() {
        format!("deps_install({stack})")
    } else {
        format!("deps_install({stack}) + {}", packages.join(" "))
    };
    run_capture(program, &borrowed, &cwd, &label).await
}

async fn deps_audit() -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Try cargo audit first · falls back with helpful error if not installed.
    run_capture("cargo", &["audit"], &cwd, "deps_audit").await
}

async fn secrets_setup(args: &Value) -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let names: Vec<String> = match args.get("keys").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        None => vec!["DATABASE_URL".into(), "API_KEY".into()],
    };

    let mut body = String::from("# Generated by pipeline_env.secrets_setup · placeholders only\n");
    for k in &names {
        writeln!(body, "{k}=__SET_ME__").ok();
    }
    let path = cwd.join(".env.example");
    if let Err(e) = tokio::fs::write(&path, body).await {
        return err(format!("write .env.example: {e}"));
    }
    ToolResponse::ok(json!({
        "path": path.display().to_string(),
        "keys": names,
    }))
}

async fn run_capture(
    program: &str,
    args: &[&str],
    cwd: &std::path::Path,
    label: &str,
) -> ToolResponse {
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
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    ToolResponse {
        ok,
        data: json!({
            "command": label,
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": truncate(&stdout, 8_000),
            "stderr": truncate(&stderr, 8_000),
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

const ENV_EXAMPLE: &str = "# Copy to .env and fill in real values · NEVER commit .env.\nDATABASE_URL=postgres://user:pass@localhost:5432/db\nAPI_KEY=__SET_ME__\nLOG_LEVEL=info\n";

const DEVCONTAINER_JSON: &str = "{\n  \"name\": \"pipeline-dev\",\n  \"image\": \"mcr.microsoft.com/devcontainers/base:bookworm\",\n  \"features\": {\n    \"ghcr.io/devcontainers/features/rust:1\": {},\n    \"ghcr.io/devcontainers/features/docker-in-docker:2\": {}\n  },\n  \"postCreateCommand\": \"cargo --version && docker --version\"\n}\n";

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

#[cfg(test)]
mod tests {
    use super::truncate;
    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("hi", 100), "hi");
    }
    #[test]
    fn truncate_marks_long_strings() {
        let out = truncate(&"x".repeat(50), 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.contains("truncated"));
    }
}

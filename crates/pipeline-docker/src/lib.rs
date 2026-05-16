//! Pipeline Docker control · shell-out to `docker` and `docker compose`.
//!
//! Day-4b ships a thin shell-out wrapper. CLAUDE.md design target is the
//! `bollard` crate for the Engine API; migration is mechanical and lands
//! incrementally as we need streaming logs / finer container control.

#![allow(clippy::doc_markdown)]

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::process::Command;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("docker {cmd} exit {code}: {stderr}")]
    NonZero {
        cmd: String,
        code: i32,
        stderr: String,
    },
    #[error("docker daemon unavailable: {0}")]
    Unavailable(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u128,
}

impl CommandOutput {
    pub fn from_parts(status: ExitStatus, stdout: &[u8], stderr: &[u8], dur: Duration) -> Self {
        Self {
            stdout: String::from_utf8_lossy(stdout).into_owned(),
            stderr: String::from_utf8_lossy(stderr).into_owned(),
            exit_code: status.code().unwrap_or(-1),
            duration_ms: dur.as_millis(),
        }
    }

    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }
}

/// Verify the docker daemon is reachable. Returns the daemon version on success.
pub async fn ping() -> Result<String, DockerError> {
    let out = run(
        "docker",
        &["version", "--format", "{{.Server.Version}}"],
        None,
    )
    .await?;
    if !out.ok() {
        return Err(DockerError::Unavailable(out.stderr.trim().to_owned()));
    }
    Ok(out.stdout.trim().to_owned())
}

pub async fn build(
    context: &Path,
    tag: &str,
    dockerfile: Option<&Path>,
    target: Option<&str>,
) -> Result<CommandOutput, DockerError> {
    let mut args: Vec<String> = vec!["build".into(), "-t".into(), tag.into()];
    if let Some(df) = dockerfile {
        args.push("-f".into());
        args.push(df.display().to_string());
    }
    if let Some(t) = target {
        args.push("--target".into());
        args.push(t.into());
    }
    args.push(context.display().to_string());
    let cli: Vec<&str> = args.iter().map(String::as_str).collect();
    run("docker", &cli, None).await
}

pub async fn run_image(
    image: &str,
    cmd: &[&str],
    env: &[(String, String)],
) -> Result<CommandOutput, DockerError> {
    let mut args: Vec<String> = vec!["run".into(), "--rm".into()];
    for (k, v) in env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push(image.into());
    for c in cmd {
        args.push((*c).to_owned());
    }
    let cli: Vec<&str> = args.iter().map(String::as_str).collect();
    run("docker", &cli, None).await
}

pub async fn exec(container: &str, cmd: &[&str]) -> Result<CommandOutput, DockerError> {
    let mut args: Vec<&str> = vec!["exec", container];
    args.extend_from_slice(cmd);
    run("docker", &args, None).await
}

pub async fn logs(container: &str, tail: Option<u32>) -> Result<CommandOutput, DockerError> {
    let tail_str;
    let mut args: Vec<&str> = vec!["logs"];
    if let Some(n) = tail {
        tail_str = n.to_string();
        args.push("--tail");
        args.push(&tail_str);
    }
    args.push(container);
    run("docker", &args, None).await
}

pub async fn inspect(name: &str) -> Result<serde_json::Value, DockerError> {
    let out = run("docker", &["inspect", name], None).await?;
    if !out.ok() {
        return Err(DockerError::NonZero {
            cmd: format!("inspect {name}"),
            code: out.exit_code,
            stderr: out.stderr,
        });
    }
    Ok(serde_json::from_str(&out.stdout).unwrap_or(serde_json::Value::Null))
}

pub async fn rm(name: &str, force: bool) -> Result<CommandOutput, DockerError> {
    let mut args: Vec<&str> = vec!["rm"];
    if force {
        args.push("-f");
    }
    args.push(name);
    run("docker", &args, None).await
}

pub async fn image_pull(image: &str) -> Result<CommandOutput, DockerError> {
    run("docker", &["pull", image], None).await
}

pub async fn image_push(image: &str) -> Result<CommandOutput, DockerError> {
    run("docker", &["push", image], None).await
}

// ---------- compose ----------

pub async fn compose_up(
    compose_file: &Path,
    services: &[String],
) -> Result<CommandOutput, DockerError> {
    let mut args: Vec<String> = vec![
        "compose".into(),
        "-f".into(),
        compose_file.display().to_string(),
        "up".into(),
        "-d".into(),
        "--wait".into(),
    ];
    for s in services {
        args.push(s.clone());
    }
    let cli: Vec<&str> = args.iter().map(String::as_str).collect();
    run("docker", &cli, None).await
}

pub async fn compose_down(compose_file: &Path) -> Result<CommandOutput, DockerError> {
    run(
        "docker",
        &[
            "compose",
            "-f",
            &compose_file.display().to_string(),
            "down",
            "-v",
        ],
        None,
    )
    .await
}

pub async fn compose_ps(compose_file: &Path) -> Result<CommandOutput, DockerError> {
    run(
        "docker",
        &[
            "compose",
            "-f",
            &compose_file.display().to_string(),
            "ps",
            "--format",
            "json",
        ],
        None,
    )
    .await
}

pub async fn compose_logs(
    compose_file: &Path,
    service: Option<&str>,
) -> Result<CommandOutput, DockerError> {
    let path = compose_file.display().to_string();
    let mut args: Vec<&str> = vec!["compose", "-f", &path, "logs", "--no-color", "--tail=200"];
    if let Some(s) = service {
        args.push(s);
    }
    run("docker", &args, None).await
}

// ---------- internals ----------

async fn run(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<CommandOutput, DockerError> {
    let start = std::time::Instant::now();
    tracing::debug!(program, args = ?args, "spawn");
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let output = cmd.output().await?;
    let dur = start.elapsed();
    Ok(CommandOutput::from_parts(
        output.status,
        &output.stdout,
        &output.stderr,
        dur,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_emits_command_output() {
        let out = run("true", &[], None).await.expect("spawn");
        assert!(out.ok());
        assert_eq!(out.exit_code, 0);
    }

    #[tokio::test]
    async fn nonzero_returns_nonzero_exit_code() {
        let out = run("false", &[], None).await.expect("spawn");
        assert!(!out.ok());
    }
}

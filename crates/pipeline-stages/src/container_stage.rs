//! Container stage · `docker build` + image-size gate.
//!
//! Day-4b: builds the project's Dockerfile and reports image size.
//! Trivy vulnerability scan + critical-CVE gate land at MVP.

use async_trait::async_trait;
use pipeline_core::{
    FailureDetail, Stage, StageContext, StageError, StageKind, StageResult, StageStatus,
};
use std::fmt::Write as _;
use std::time::Instant;
use tokio::process::Command;

pub struct ContainerStage;

#[async_trait]
impl Stage for ContainerStage {
    fn kind(&self) -> StageKind {
        StageKind::Container
    }

    async fn run(&self, ctx: &StageContext) -> Result<StageResult, StageError> {
        let start = Instant::now();
        let dockerfile = ctx.project_root.join("Dockerfile");
        if !dockerfile.exists() {
            return Ok(StageResult {
                stage: StageKind::Container,
                status: StageStatus::Skipped,
                duration: start.elapsed(),
                stdout: String::new(),
                stderr: format!(
                    "no Dockerfile at {} · container stage skipped",
                    dockerfile.display()
                ),
                failure: None,
            });
        }

        // Ping daemon first · gives a clean error rather than a build failure.
        let ping = Command::new("docker")
            .args(["version", "--format", "{{.Server.Version}}"])
            .output()
            .await
            .map_err(StageError::Io)?;
        if !ping.status.success() {
            return Ok(StageResult {
                stage: StageKind::Container,
                status: StageStatus::Skipped,
                duration: start.elapsed(),
                stdout: String::new(),
                stderr: "docker daemon unreachable · container stage skipped".into(),
                failure: None,
            });
        }

        let tag = format!(
            "{}:pipeline-{}",
            ctx.config.project,
            short_hash(&ctx.config.version)
        );
        let build = Command::new("docker")
            .args([
                "build",
                "-t",
                &tag,
                "-f",
                &dockerfile.display().to_string(),
                ".",
            ])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(StageError::Io)?;

        let stdout = String::from_utf8_lossy(&build.stdout).into_owned();
        let mut stderr = String::from_utf8_lossy(&build.stderr).into_owned();

        if !build.status.success() {
            return Ok(StageResult {
                stage: StageKind::Container,
                status: StageStatus::Fail,
                duration: start.elapsed(),
                stdout,
                stderr,
                failure: Some(FailureDetail {
                    message: format!("docker build exit {}", build.status.code().unwrap_or(-1)),
                    file: Some(dockerfile.display().to_string()),
                    line: None,
                }),
            });
        }

        // Image size gate.
        let size_bytes = inspect_size(&tag).await.unwrap_or(0);
        let size_mb = size_bytes / 1_048_576;
        let cap_mb = ctx.config.gates.image_size_mb.unwrap_or(u64::MAX);
        let mut failure = None;
        let mut status = StageStatus::Pass;
        writeln!(
            stderr,
            "\n--- gate ---\nimage_size_mb={size_mb} cap={cap_mb}"
        )
        .ok();
        if size_mb > cap_mb {
            status = StageStatus::Fail;
            failure = Some(FailureDetail {
                message: format!("image_size_mb {size_mb} > cap {cap_mb}"),
                file: Some(dockerfile.display().to_string()),
                line: None,
            });
        }

        Ok(StageResult {
            stage: StageKind::Container,
            status,
            duration: start.elapsed(),
            stdout,
            stderr,
            failure,
        })
    }
}

async fn inspect_size(tag: &str) -> Option<u64> {
    let out = Command::new("docker")
        .args(["inspect", "--format", "{{.Size}}", tag])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .ok()
}

/// Stable 8-char tag suffix derived from the project version string.
fn short_hash(input: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset
    for byte in input.as_bytes() {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")[..8].to_owned()
}

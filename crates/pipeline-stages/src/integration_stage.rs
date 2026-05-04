//! Integration stage · `docker compose up --wait` → smoke checks → `compose down`.
//!
//! Day-4b ships compose lifecycle. Health-check polling and contract
//! tests (schemathesis) land at MVP week 5.

use async_trait::async_trait;
use pipeline_core::{
    FailureDetail, Stage, StageContext, StageError, StageKind, StageResult, StageStatus,
};
use std::time::Instant;
use tokio::process::Command;

pub struct IntegrationStage;

#[async_trait]
impl Stage for IntegrationStage {
    fn kind(&self) -> StageKind {
        StageKind::Integration
    }

    async fn run(&self, ctx: &StageContext) -> Result<StageResult, StageError> {
        let start = Instant::now();
        let compose = ctx.project_root.join("docker-compose.yml");
        if !compose.exists() {
            return Ok(StageResult {
                stage: StageKind::Integration,
                status: StageStatus::Skipped,
                duration: start.elapsed(),
                stdout: String::new(),
                stderr: format!(
                    "no docker-compose.yml at {} · integration stage skipped",
                    compose.display()
                ),
                failure: None,
            });
        }

        let mut combined_stdout = String::new();
        let mut combined_stderr = String::new();

        let up = Command::new("docker")
            .args([
                "compose",
                "-f",
                &compose.display().to_string(),
                "up",
                "-d",
                "--wait",
            ])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(StageError::Io)?;
        combined_stdout.push_str("--- compose up ---\n");
        combined_stdout.push_str(&String::from_utf8_lossy(&up.stdout));
        combined_stderr.push_str("--- compose up ---\n");
        combined_stderr.push_str(&String::from_utf8_lossy(&up.stderr));

        let mut failure: Option<FailureDetail> = None;
        if !up.status.success() {
            failure = Some(FailureDetail {
                message: format!("compose up exit {}", up.status.code().unwrap_or(-1)),
                file: Some(compose.display().to_string()),
                line: None,
            });
        }

        // Always tear down · even on failure · prevents resource leaks.
        let down = Command::new("docker")
            .args([
                "compose",
                "-f",
                &compose.display().to_string(),
                "down",
                "-v",
            ])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(StageError::Io)?;
        combined_stdout.push_str("\n--- compose down ---\n");
        combined_stdout.push_str(&String::from_utf8_lossy(&down.stdout));
        combined_stderr.push_str("\n--- compose down ---\n");
        combined_stderr.push_str(&String::from_utf8_lossy(&down.stderr));

        Ok(StageResult {
            stage: StageKind::Integration,
            status: if failure.is_some() {
                StageStatus::Fail
            } else {
                StageStatus::Pass
            },
            duration: start.elapsed(),
            stdout: combined_stdout,
            stderr: combined_stderr,
            failure,
        })
    }
}

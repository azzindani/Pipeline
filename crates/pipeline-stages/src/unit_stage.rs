//! Unit stage · `cargo test --workspace --no-fail-fast`.
//!
//! Coverage threshold enforcement lands at MVP via `cargo-llvm-cov`.

use async_trait::async_trait;
use pipeline_core::{
    FailureDetail, Stage, StageContext, StageError, StageKind, StageResult, StageStatus,
};
use std::time::Instant;
use tokio::process::Command;

pub struct UnitStage;

#[async_trait]
impl Stage for UnitStage {
    fn kind(&self) -> StageKind {
        StageKind::Unit
    }

    async fn run(&self, ctx: &StageContext) -> Result<StageResult, StageError> {
        let start = Instant::now();
        tracing::info!(stage = "unit", "cargo test --workspace");

        let output = Command::new("cargo")
            .args(["test", "--workspace", "--no-fail-fast"])
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(StageError::Io)?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        let status = if output.status.success() {
            StageStatus::Pass
        } else {
            StageStatus::Fail
        };

        let failure = if output.status.success() {
            None
        } else {
            Some(FailureDetail {
                message: format!("cargo test exit {}", output.status.code().unwrap_or(-1)),
                file: None,
                line: None,
            })
        };

        Ok(StageResult {
            stage: StageKind::Unit,
            status,
            duration: start.elapsed(),
            stdout,
            stderr,
            failure,
        })
    }
}

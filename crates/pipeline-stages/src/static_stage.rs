//! Static stage · `cargo fmt --check` + `cargo clippy -D warnings`.

use async_trait::async_trait;
use pipeline_core::{
    FailureDetail, Stage, StageContext, StageError, StageKind, StageResult, StageStatus,
};
use std::fmt::Write as _;
use std::time::Instant;
use tokio::process::Command;

pub struct StaticStage;

#[async_trait]
impl Stage for StaticStage {
    fn kind(&self) -> StageKind {
        StageKind::Static
    }

    async fn run(&self, ctx: &StageContext) -> Result<StageResult, StageError> {
        let start = Instant::now();
        let mut combined_stdout = String::new();
        let mut combined_stderr = String::new();
        let mut failure: Option<FailureDetail> = None;

        let checks: &[(&str, &[&str])] = &[
            ("cargo", &["fmt", "--all", "--", "--check"]),
            (
                "cargo",
                &[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            ),
        ];

        for (program, args) in checks {
            let label = format!("{program} {}", args.join(" "));
            tracing::info!(stage = "static", check = %label, "run");
            let output = Command::new(program)
                .args(*args)
                .current_dir(&ctx.project_root)
                .output()
                .await
                .map_err(StageError::Io)?;

            writeln!(combined_stdout, "--- {label} ---").ok();
            combined_stdout.push_str(&String::from_utf8_lossy(&output.stdout));
            writeln!(combined_stderr, "--- {label} ---").ok();
            combined_stderr.push_str(&String::from_utf8_lossy(&output.stderr));

            if !output.status.success() && failure.is_none() {
                failure = Some(FailureDetail {
                    message: format!("{label} exit {}", output.status.code().unwrap_or(-1)),
                    file: None,
                    line: None,
                });
            }
        }

        Ok(StageResult {
            stage: StageKind::Static,
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

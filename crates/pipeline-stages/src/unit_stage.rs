//! Unit stage · `cargo test --workspace --no-fail-fast`, then coverage.
//!
//! kind: component
//!
//! ! `cargo-llvm-cov` RUNS the suite, ✗ re-runs it. Measuring by running the
//! tests a second time under instrumentation would double the inner loop an
//! agent sits in all day, so when the tool is present it replaces `cargo test`
//! rather than following it.
//!
//! Absent tool → plain `cargo test` and no number. A missing tool is a missing
//! measurement: a contributor without it still gets a usable unit stage, and
//! the gap surfaces in the maturity report as absent evidence rather than as a
//! red build for a reason unrelated to their change.

use async_trait::async_trait;
use pipeline_core::{
    FailureDetail, Stage, StageContext, StageError, StageKind, StageResult, StageStatus,
};
use std::fmt::Write as _;
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
        let with_coverage = coverage_tool_available(&ctx.project_root).await;

        let args: &[&str] = if with_coverage {
            &[
                "llvm-cov",
                "--workspace",
                "--summary-only",
                "--no-fail-fast",
            ]
        } else {
            &["test", "--workspace", "--no-fail-fast"]
        };
        tracing::info!(
            stage = "unit",
            coverage = with_coverage,
            "cargo {}",
            args[0]
        );

        let output = Command::new("cargo")
            .args(args)
            .current_dir(&ctx.project_root)
            .output()
            .await
            .map_err(StageError::Io)?;

        let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        let mut status = if output.status.success() {
            StageStatus::Pass
        } else {
            StageStatus::Fail
        };
        let mut failure = if output.status.success() {
            None
        } else {
            Some(FailureDetail {
                message: format!(
                    "cargo {} exit {}",
                    args[0],
                    output.status.code().unwrap_or(-1)
                ),
                file: None,
                line: None,
            })
        };

        // ! Gate the number only when the suite passed. A coverage figure from a
        // red suite is derived from tests that did not run.
        if status == StageStatus::Pass {
            if with_coverage {
                let combined = format!("{stdout}{stderr}");
                match parse_total_line_coverage(&combined) {
                    Some(percent) => {
                        let _ = write!(stdout, "\n--- coverage ---\nline coverage {percent:.2}%");
                        if let Some(gate) = ctx.config.gates.coverage {
                            let _ = write!(stdout, " · gate {gate}%");
                            if percent + f64::EPSILON < f64::from(gate) {
                                status = StageStatus::Fail;
                                failure = Some(FailureDetail {
                                    message: format!("coverage {percent:.2}% below gate {gate}%"),
                                    file: None,
                                    line: None,
                                });
                            }
                        }
                    }
                    None => stdout.push_str(
                        "\n--- coverage ---\nunmeasured · no TOTAL row in llvm-cov output",
                    ),
                }
            } else {
                stdout.push_str(
                    "\n--- coverage ---\nskipped · cargo-llvm-cov not installed · \
                     `cargo install cargo-llvm-cov` to measure",
                );
            }
        }

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

/// Is `cargo-llvm-cov` installed? Cheap probe · ✗ run the suite to find out.
async fn coverage_tool_available(project_root: &std::path::Path) -> bool {
    Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .current_dir(project_root)
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

/// Line coverage from llvm-cov's TOTAL row.
///
/// ! The row is `TOTAL <regions> <missed> <cover%> <functions> <missed> <cover%>
/// <lines> <missed> <cover%> ...` — LINE coverage is the third percentage, ✗ the
/// first. Taking the first reports region coverage under a line-coverage gate,
/// which reads plausible and is wrong by a few points in either direction.
fn parse_total_line_coverage(text: &str) -> Option<f64> {
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("TOTAL"))?;
    let percents: Vec<f64> = line
        .split_whitespace()
        .filter_map(|f| f.strip_suffix('%'))
        .filter_map(|f| f.parse().ok())
        .collect();
    percents.get(2).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Filename  Regions Missed Cover Functions Missed Cover Lines Missed Cover Branches Missed Cover
a.rs          10      2 80.00%         3      0 100.00%    20      5 75.00%        0      0 -
TOTAL      38648  15849 58.99%      2548    900 64.68%  21720   8395 61.35%        0      0 -
";

    #[test]
    fn line_coverage_is_the_third_percentage_not_the_first() {
        // ! Region coverage (58.99) sits before line coverage (61.35) on the
        // TOTAL row. Reading the first is the mistake this pins down.
        assert_eq!(parse_total_line_coverage(SAMPLE), Some(61.35));
    }

    #[test]
    fn output_without_a_total_row_yields_none_not_zero() {
        // A zero would fail every gate and read as "measured, and terrible".
        assert_eq!(
            parse_total_line_coverage("error: something went wrong"),
            None
        );
        assert_eq!(parse_total_line_coverage(""), None);
    }

    #[test]
    fn the_last_total_row_wins() {
        let doubled = format!("{SAMPLE}\nTOTAL 1 0 10.00% 1 0 20.00% 1 0 30.00% 0 0 -");
        assert_eq!(parse_total_line_coverage(&doubled), Some(30.0));
    }
}

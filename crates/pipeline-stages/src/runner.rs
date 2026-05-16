//! Profile runner · executes every stage in the profile in order.

use pipeline_core::{StageContext, StageProfile, StageResult, StageStatus};

pub struct Runner;

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunnerSummary {
    pub profile: String,
    pub overall: StageStatus,
    pub results: Vec<StageResult>,
    pub total_duration_ms: u128,
}

impl Runner {
    /// Execute every stage in `profile` against `ctx`. Stops on first stage error
    /// (process spawn failed); a `Fail` status does NOT stop the runner — every
    /// stage still gets to report so the agent sees the full picture.
    pub async fn run_profile(profile: StageProfile, ctx: &StageContext) -> RunnerSummary {
        let mut results = Vec::new();
        let mut overall = StageStatus::Pass;
        let mut total: u128 = 0;

        for kind in profile.stages() {
            let Some(stage) = crate::stage_for(*kind) else {
                results.push(StageResult {
                    stage: *kind,
                    status: StageStatus::Skipped,
                    duration: std::time::Duration::ZERO,
                    stdout: String::new(),
                    stderr: format!("{} stage not yet implemented · POC week 2+", kind.as_str()),
                    failure: None,
                });
                continue;
            };

            let result = match stage.run(ctx).await {
                Ok(r) => r,
                Err(e) => StageResult {
                    stage: *kind,
                    status: StageStatus::Error,
                    duration: std::time::Duration::ZERO,
                    stdout: String::new(),
                    stderr: e.to_string(),
                    failure: Some(pipeline_core::FailureDetail {
                        message: e.to_string(),
                        file: None,
                        line: None,
                    }),
                },
            };

            total += result.duration.as_millis();
            match result.status {
                StageStatus::Pass | StageStatus::Skipped => {}
                StageStatus::Fail | StageStatus::Error => overall = StageStatus::Fail,
            }
            results.push(result);
        }

        RunnerSummary {
            profile: profile_label(profile).into(),
            overall,
            results,
            total_duration_ms: total,
        }
    }
}

const fn profile_label(p: StageProfile) -> &'static str {
    match p {
        StageProfile::Fast => "fast",
        StageProfile::Full => "full",
        StageProfile::Preflight => "preflight",
        StageProfile::Confirm => "confirm",
    }
}

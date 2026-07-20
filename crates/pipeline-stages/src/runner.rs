//! Profile runner · executes every stage in the profile in order.
//!
//! ! `Skipped` used to fold into `Pass` when computing `overall`. That made the
//! pre-push gate structurally incapable of failing for the reason it exists:
//! on a Docker-less host `preflight` skipped container · integration ·
//! security and still reported green after running only fmt · clippy · test.
//!
//! Two rules close it:
//! - **strict profiles** (`preflight`) fail on any skip · the pre-push gate
//!   means *every* stage genuinely executed.
//! - **every profile** surfaces skips in the summary with the reason, so no
//!   caller can read "green" without also reading "3 stages did not run".

use pipeline_core::{StageContext, StageKind, StageProfile, StageResult, StageStatus};
use std::fmt::Write as _;

pub struct Runner;

/// A stage that produced no verdict · `reason` tells an agent whether the skip
/// is fixable (start docker, add a Dockerfile) or structural (not implemented).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkippedStage {
    pub stage: StageKind,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunnerSummary {
    pub profile: String,
    /// Strict → a skipped stage fails the run. `preflight` is strict.
    pub strict: bool,
    pub overall: StageStatus,
    pub results: Vec<StageResult>,
    pub stages_planned: usize,
    /// Stages that actually produced a verdict · ! compare against
    /// `stages_planned` before trusting `overall == pass`.
    pub stages_executed: usize,
    pub skipped: Vec<SkippedStage>,
    /// One-line gate statement · always mentions skips when there are any.
    pub gate_note: String,
    pub total_duration_ms: u128,
}

impl RunnerSummary {
    /// True only when every planned stage executed and passed.
    ///
    /// ! Use this, ✗ `overall == Pass` alone, when deciding to push.
    pub fn fully_verified(&self) -> bool {
        self.overall == StageStatus::Pass && self.skipped.is_empty()
    }
}

impl Runner {
    /// Execute every stage in `profile` against `ctx`. A `Fail` does NOT stop
    /// the runner — every stage still reports so the agent sees the full
    /// picture.
    pub async fn run_profile(profile: StageProfile, ctx: &StageContext) -> RunnerSummary {
        let mut results = Vec::new();
        for kind in profile.stages() {
            results.push(run_one(*kind, ctx).await);
        }
        summarize(profile, results)
    }
}

async fn run_one(kind: StageKind, ctx: &StageContext) -> StageResult {
    let stage = crate::stage_for(kind);
    match stage.run(ctx).await {
        Ok(r) => r,
        Err(e) => StageResult {
            stage: kind,
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
    }
}

/// Fold stage results into a verdict.
///
/// Pure so the gate invariant is testable without spawning cargo or docker.
fn summarize(profile: StageProfile, results: Vec<StageResult>) -> RunnerSummary {
    let strict = profile.is_strict();
    let mut overall = StageStatus::Pass;
    let mut skipped = Vec::new();
    let mut total: u128 = 0;

    for r in &results {
        total += r.duration.as_millis();
        match r.status {
            StageStatus::Pass => {}
            StageStatus::Skipped => {
                skipped.push(SkippedStage {
                    stage: r.stage,
                    reason: skip_reason(r),
                });
                // ! A strict profile claims every stage ran. A skip breaks that
                // claim, so it breaks the verdict — ✗ silently absorbed.
                if strict {
                    overall = StageStatus::Fail;
                }
            }
            StageStatus::Fail | StageStatus::Error => overall = StageStatus::Fail,
        }
    }

    let planned = results.len();
    let executed = planned - skipped.len();
    RunnerSummary {
        gate_note: gate_note(profile_label(profile), strict, planned, executed, &skipped),
        profile: profile_label(profile).into(),
        strict,
        overall,
        results,
        stages_planned: planned,
        stages_executed: executed,
        skipped,
        total_duration_ms: total,
    }
}

/// Stages record why they skipped in `stderr` · keep it verbatim, an agent
/// needs "docker daemon unreachable" to be actionable.
fn skip_reason(r: &StageResult) -> String {
    let reason = r.stderr.trim();
    if reason.is_empty() {
        "no reason recorded".to_owned()
    } else {
        reason.to_owned()
    }
}

fn gate_note(
    label: &str,
    strict: bool,
    planned: usize,
    executed: usize,
    skipped: &[SkippedStage],
) -> String {
    let mut note = String::new();
    if skipped.is_empty() {
        write!(note, "{label} · {executed}/{planned} stages executed").ok();
        return note;
    }
    let names: Vec<&str> = skipped.iter().map(|s| s.stage.as_str()).collect();
    write!(
        note,
        "! {} of {planned} stages did not run ({}) · only {executed} stage(s) were verified",
        skipped.len(),
        names.join(", ")
    )
    .ok();
    if strict {
        write!(
            note,
            " · {label} is a strict gate → overall FAIL, ✗ green on an unrun stage"
        )
        .ok();
    } else {
        write!(
            note,
            " · {label} is not strict → a pass here covers the executed stages only"
        )
        .ok();
    }
    note
}

const fn profile_label(p: StageProfile) -> &'static str {
    match p {
        StageProfile::Fast => "fast",
        StageProfile::Full => "full",
        StageProfile::Preflight => "preflight",
        StageProfile::Confirm => "confirm",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn result(stage: StageKind, status: StageStatus, stderr: &str) -> StageResult {
        StageResult {
            stage,
            status,
            duration: Duration::from_millis(5),
            stdout: String::new(),
            stderr: stderr.to_owned(),
            failure: None,
        }
    }

    fn preflight_results(security: StageStatus) -> Vec<StageResult> {
        vec![
            result(StageKind::Static, StageStatus::Pass, ""),
            result(StageKind::Unit, StageStatus::Pass, ""),
            result(StageKind::Container, StageStatus::Pass, ""),
            result(StageKind::Integration, StageStatus::Pass, ""),
            result(StageKind::Security, security, "no scanner"),
        ]
    }

    #[test]
    fn a_skipped_stage_does_not_pass_a_strict_profile() {
        // ! The defect this fixes: preflight ran fmt/clippy/test, skipped
        // container + integration + security, and reported Pass — so the
        // documented "all green → push allowed" gate allowed every push.
        let s = summarize(
            StageProfile::Preflight,
            preflight_results(StageStatus::Skipped),
        );
        assert_eq!(s.overall, StageStatus::Fail, "{}", s.gate_note);
        assert!(!s.fully_verified());
        assert_eq!(s.stages_executed, 4);
        assert_eq!(s.stages_planned, 5);
    }

    #[test]
    fn a_strict_profile_passes_only_when_every_stage_executed_and_passed() {
        let s = summarize(
            StageProfile::Preflight,
            preflight_results(StageStatus::Pass),
        );
        assert_eq!(s.overall, StageStatus::Pass);
        assert!(s.fully_verified());
        assert_eq!(s.stages_executed, s.stages_planned);
        assert!(s.skipped.is_empty());
    }

    #[test]
    fn a_docker_less_host_cannot_produce_a_green_preflight() {
        // Container + integration self-skip with no daemon · security needs
        // docker for trufflehog. Three skips, and the gate must be red.
        let s = summarize(
            StageProfile::Preflight,
            vec![
                result(StageKind::Static, StageStatus::Pass, ""),
                result(StageKind::Unit, StageStatus::Pass, ""),
                result(
                    StageKind::Container,
                    StageStatus::Skipped,
                    "docker daemon unreachable · container stage skipped",
                ),
                result(
                    StageKind::Integration,
                    StageStatus::Skipped,
                    "no docker-compose.yml",
                ),
                result(StageKind::Security, StageStatus::Skipped, "docker spawn"),
            ],
        );
        assert_eq!(s.overall, StageStatus::Fail);
        assert_eq!(s.skipped.len(), 3);
        assert!(
            s.gate_note.contains("3 of 5 stages did not run"),
            "{}",
            s.gate_note
        );
    }

    #[test]
    fn a_skip_on_a_non_strict_profile_stays_green_but_is_surfaced_prominently() {
        // fast/full must not turn red just because docker is absent · but the
        // caller must never read "green" without reading "did not run".
        let s = summarize(
            StageProfile::Full,
            vec![
                result(StageKind::Static, StageStatus::Pass, ""),
                result(StageKind::Unit, StageStatus::Pass, ""),
                result(StageKind::Container, StageStatus::Skipped, "no Dockerfile"),
                result(
                    StageKind::Integration,
                    StageStatus::Skipped,
                    "no docker-compose.yml",
                ),
            ],
        );
        assert_eq!(s.overall, StageStatus::Pass);
        assert!(!s.strict);
        // ! Green, but not fully verified · the distinction is the whole point.
        assert!(!s.fully_verified());
        assert_eq!(s.stages_executed, 2);
        assert!(s.gate_note.starts_with('!'), "{}", s.gate_note);
        assert!(s.gate_note.contains("2 of 4 stages did not run"));
    }

    #[test]
    fn skip_reasons_are_preserved_so_an_agent_knows_what_to_fix() {
        let s = summarize(
            StageProfile::Full,
            vec![
                result(StageKind::Static, StageStatus::Pass, ""),
                result(StageKind::Unit, StageStatus::Pass, ""),
                result(
                    StageKind::Container,
                    StageStatus::Skipped,
                    "docker daemon unreachable · container stage skipped",
                ),
                result(StageKind::Integration, StageStatus::Skipped, ""),
            ],
        );
        assert_eq!(s.skipped[0].stage, StageKind::Container);
        assert!(s.skipped[0].reason.contains("daemon unreachable"));
        // An empty stderr must still yield something readable, ✗ a blank line.
        assert_eq!(s.skipped[1].reason, "no reason recorded");
    }

    #[test]
    fn a_failing_stage_fails_every_profile() {
        let s = summarize(
            StageProfile::Fast,
            vec![
                result(StageKind::Static, StageStatus::Fail, ""),
                result(StageKind::Unit, StageStatus::Pass, ""),
            ],
        );
        assert_eq!(s.overall, StageStatus::Fail);
    }

    #[test]
    fn an_errored_stage_fails_the_run() {
        let s = summarize(
            StageProfile::Fast,
            vec![
                result(StageKind::Static, StageStatus::Error, "spawn"),
                result(StageKind::Unit, StageStatus::Pass, ""),
            ],
        );
        assert_eq!(s.overall, StageStatus::Fail);
        // An Error is not a skip · it executed and blew up.
        assert!(s.skipped.is_empty());
        assert_eq!(s.stages_executed, 2);
    }

    #[test]
    fn a_clean_run_reports_full_coverage_of_the_profile() {
        let s = summarize(
            StageProfile::Fast,
            vec![
                result(StageKind::Static, StageStatus::Pass, ""),
                result(StageKind::Unit, StageStatus::Pass, ""),
            ],
        );
        assert!(s.fully_verified());
        assert_eq!(s.gate_note, "fast · 2/2 stages executed");
    }
}

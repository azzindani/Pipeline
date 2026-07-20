//! Security stage · trufflehog secret scan · `cargo audit` dependency audit.
//!
//! ! This stage is the only thing that makes `preflight` different from `full`.
//! While it was unimplemented the runner emitted `Skipped` and `Skipped` never
//! degraded `overall`, so the documented pre-push gate reported green after
//! running fmt · clippy · test and nothing else.
//!
//! ! Three states, never merged: `Clean` is a determination, `Findings` is a
//! determination, `ScannerUnavailable` is the *absence* of one. In a gate the
//! absence of a verdict fails — a pre-push security gate that passes because
//! trufflehog is not installed is the defect, ✗ the fix.
//!
//! ! Command construction is duplicated from `pipeline-mcp::scanners` on
//! purpose: `pipeline-stages` must not depend on `pipeline-mcp` (that inverts
//! the layering). Flags are kept identical — `--fail` for trufflehog is what
//! turns findings into a nonzero exit, ✗ an option a caller may drop.

use async_trait::async_trait;
use pipeline_core::{
    FailureDetail, Stage, StageContext, StageError, StageKind, StageResult, StageStatus,
};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;
use tokio::process::Command;

pub const TRUFFLEHOG_IMAGE: &str = "trufflesecurity/trufflehog:latest";

/// trufflehog `--fail` exits with this code when results are found ·
/// any other nonzero is a scanner fault, ✗ a finding.
pub const TRUFFLEHOG_FINDINGS_EXIT: i32 = 183;

/// In-container path the worktree is bound to.
pub const MOUNT_POINT: &str = "/work";

/// Cap on reported secret rows · overflow is stated, ✗ silently cut.
pub const MAX_FINDING_ROWS: usize = 100;

/// Verdict of one scanner. Three states, always distinguishable by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    /// Scanner ran to completion and found nothing.
    Clean,
    /// Scanner ran and found something · gate must fail.
    Findings,
    /// Scanner produced no verdict · UNKNOWN, ✗ pass and ✗ fail on its own —
    /// but a *gate* has to pick, and an unchecked gate is not a gate.
    ScannerUnavailable,
}

impl ScanStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Findings => "findings",
            Self::ScannerUnavailable => "scanner_unavailable",
        }
    }

    /// Only a completed clean scan is ok · UNKNOWN must never read as pass.
    pub fn is_ok(self) -> bool {
        self == Self::Clean
    }

    /// Whether the scanner actually reached a conclusion. Lets an agent tell
    /// "found nothing" from "could not check" without string matching.
    pub fn determined(self) -> bool {
        self != Self::ScannerUnavailable
    }
}

/// One secret hit · location only. ✗ the secret value: found secrets are
/// flagged and discarded, never stored or echoed back to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    pub detector: String,
    pub file: String,
    pub line: i64,
}

/// Result of a single scanner inside the stage.
#[derive(Debug, Clone)]
struct Check {
    /// Name of the check as reported to the agent.
    name: &'static str,
    /// Binary/image that was supposed to produce the verdict.
    scanner: String,
    status: ScanStatus,
    exit_code: i32,
    /// Human detail · findings locations or the reason there is no verdict.
    detail: String,
}

pub struct SecurityStage;

#[async_trait]
impl Stage for SecurityStage {
    fn kind(&self) -> StageKind {
        StageKind::Security
    }

    async fn run(&self, ctx: &StageContext) -> Result<StageResult, StageError> {
        let start = Instant::now();
        let checks = vec![
            scan_secrets(&ctx.project_root).await,
            audit_dependencies(&ctx.project_root).await,
        ];

        let stdout = render_report(&checks);
        let failure = gate_verdict(&checks);
        let status = if failure.is_some() {
            StageStatus::Fail
        } else {
            StageStatus::Pass
        };

        Ok(StageResult {
            stage: StageKind::Security,
            status,
            duration: start.elapsed(),
            stdout,
            stderr: String::new(),
            failure,
        })
    }
}

// ---------- trufflehog ----------

/// Full `docker run` argv for the secret scan.
///
/// ! `--fail` is not optional: without it trufflehog exits 0 holding live
/// credentials, and a gate keyed on the exit code lets the push through.
pub fn trufflehog_docker_args(project_root: &Path) -> Vec<String> {
    vec![
        "run".into(),
        "--rm".into(),
        "-v".into(),
        format!("{}:{MOUNT_POINT}", project_root.display()),
        TRUFFLEHOG_IMAGE.into(),
        "--json".into(),
        "--no-update".into(),
        // ! without --fail trufflehog exits 0 with live keys in hand.
        "--fail".into(),
        "filesystem".into(),
        MOUNT_POINT.into(),
    ]
}

/// Parse trufflehog's JSON-lines stdout · non-JSON lines (logs) are skipped.
pub fn parse_secret_findings(stdout: &str) -> Vec<SecretFinding> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter_map(|v| secret_finding_from(&v))
        .collect()
}

fn secret_finding_from(v: &Value) -> Option<SecretFinding> {
    let detector = v.get("DetectorName").and_then(Value::as_str)?.to_owned();
    // SourceMetadata.Data holds exactly one source-shaped object
    // (Filesystem | Git | ...) · read location generically.
    let loc = v
        .pointer("/SourceMetadata/Data")
        .and_then(Value::as_object)
        .and_then(|m| m.values().next());
    Some(SecretFinding {
        detector,
        file: loc
            .and_then(|l| l.get("file"))
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_owned(),
        line: loc
            .and_then(|l| l.get("line"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

/// Classify a trufflehog run. An empty finding list is only `Clean` when the
/// scanner actually exited cleanly · ! never report clean for a scan that
/// never ran.
pub fn classify_secret_scan(exit_code: i32, findings: usize) -> ScanStatus {
    if findings > 0 || exit_code == TRUFFLEHOG_FINDINGS_EXIT {
        ScanStatus::Findings
    } else if exit_code == 0 {
        ScanStatus::Clean
    } else {
        ScanStatus::ScannerUnavailable
    }
}

async fn scan_secrets(project_root: &Path) -> Check {
    let args = trufflehog_docker_args(project_root);
    let output = Command::new("docker")
        .args(&args)
        .current_dir(project_root)
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        // ! docker absent → no verdict. ✗ clean: nothing was looked at.
        Err(e) => {
            return Check {
                name: "secret_scan",
                scanner: TRUFFLEHOG_IMAGE.into(),
                status: ScanStatus::ScannerUnavailable,
                exit_code: -1,
                detail: format!("docker spawn failed: {e}"),
            };
        }
    };

    let code = output.status.code().unwrap_or(-1);
    let findings = parse_secret_findings(&String::from_utf8_lossy(&output.stdout));
    let status = classify_secret_scan(code, findings.len());
    Check {
        name: "secret_scan",
        scanner: TRUFFLEHOG_IMAGE.into(),
        status,
        exit_code: code,
        detail: match status {
            ScanStatus::Clean => "no secrets found".into(),
            ScanStatus::Findings => render_findings(&findings),
            ScanStatus::ScannerUnavailable => {
                format!(
                    "scanner produced no verdict · {}",
                    tail(&String::from_utf8_lossy(&output.stderr), 600)
                )
            }
        },
    }
}

/// Locations only · ✗ the matched secret. Echoing `Raw` would relocate the
/// credential into memory.db and the agent transcript.
fn render_findings(findings: &[SecretFinding]) -> String {
    let shown = findings.len().min(MAX_FINDING_ROWS);
    let mut out = format!("{} secret(s) · locations only:", findings.len());
    for f in &findings[..shown] {
        write!(out, "\n  {} · {}:{}", f.detector, f.file, f.line).ok();
    }
    if findings.len() > shown {
        write!(out, "\n  ... {} more not shown", findings.len() - shown).ok();
    }
    out
}

// ---------- cargo audit ----------

/// Dependency audit argv · this project's stack is Rust.
pub const fn cargo_audit_args() -> (&'static str, [&'static str; 1]) {
    ("cargo", ["audit"])
}

/// A present launcher with an absent subcommand (`cargo` without cargo-audit
/// installed) is still an absent scanner · the exit code alone cannot tell
/// that from an advisory, so the stderr shape decides.
pub fn classify_dep_audit(exit_code: i32, stderr: &str) -> ScanStatus {
    let s = stderr.to_lowercase();
    let missing = s.contains("no such command")
        || s.contains("command not found")
        || s.contains("not recognized")
        || s.contains("unknown command");
    if missing {
        ScanStatus::ScannerUnavailable
    } else if exit_code == 0 {
        ScanStatus::Clean
    } else {
        ScanStatus::Findings
    }
}

async fn audit_dependencies(project_root: &Path) -> Check {
    let (program, args) = cargo_audit_args();
    let output = Command::new(program)
        .args(args)
        .current_dir(project_root)
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        // ! Binary absent → UNKNOWN. Calling it a failed audit is as wrong as
        // calling it clean: neither says whether an advisory exists.
        Err(e) => {
            return Check {
                name: "dep_audit",
                scanner: "cargo audit".into(),
                status: ScanStatus::ScannerUnavailable,
                exit_code: -1,
                detail: format!("spawn failed: {e} · install with: cargo install cargo-audit"),
            };
        }
    };

    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = classify_dep_audit(code, &stderr);
    Check {
        name: "dep_audit",
        scanner: "cargo audit".into(),
        status,
        exit_code: code,
        detail: match status {
            ScanStatus::Clean => "no advisories".into(),
            ScanStatus::Findings => tail(&String::from_utf8_lossy(&output.stdout), 4_000),
            ScanStatus::ScannerUnavailable => format!(
                "cargo-audit not installed · install with: cargo install cargo-audit · {}",
                tail(&stderr, 600)
            ),
        },
    }
}

// ---------- gate ----------

/// Render every check so an agent can tell "found nothing" from
/// "could not check" without parsing prose.
fn render_report(checks: &[Check]) -> String {
    let mut out = String::new();
    for c in checks {
        writeln!(out, "--- {} ({}) ---", c.name, c.scanner).ok();
        writeln!(
            out,
            "status={} determined={} exit={}",
            c.status.as_str(),
            c.status.determined(),
            c.exit_code
        )
        .ok();
        writeln!(out, "{}", c.detail).ok();
    }
    out
}

/// Gate decision. `None` → every scanner reached a clean verdict.
///
/// ! UNKNOWN fails. A gate that passes because the scanner is missing is
/// exactly the defect this stage exists to close.
fn gate_verdict(checks: &[Check]) -> Option<FailureDetail> {
    let findings: Vec<&str> = checks
        .iter()
        .filter(|c| c.status == ScanStatus::Findings)
        .map(|c| c.name)
        .collect();
    let unknown: Vec<&str> = checks
        .iter()
        .filter(|c| c.status == ScanStatus::ScannerUnavailable)
        .map(|c| c.name)
        .collect();
    if findings.is_empty() && unknown.is_empty() {
        return None;
    }
    let mut message = String::from("security gate failed ·");
    if !findings.is_empty() {
        write!(message, " findings: {}", findings.join(", ")).ok();
    }
    if !unknown.is_empty() {
        write!(
            message,
            " UNKNOWN (scanner unavailable, ✗ clean): {}",
            unknown.join(", ")
        )
        .ok();
    }
    Some(FailureDetail {
        message,
        file: None,
        line: None,
    })
}

/// Last `max` bytes of scanner output · enough to explain a failed spawn
/// without dragging a full log into the result.
fn tail(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.len() <= max {
        return t.to_owned();
    }
    let start = (t.len() - max..t.len())
        .find(|i| t.is_char_boundary(*i))
        .unwrap_or(t.len());
    format!("...{}", &t[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &'static str, status: ScanStatus) -> Check {
        Check {
            name,
            scanner: "s".into(),
            status,
            exit_code: 0,
            detail: String::new(),
        }
    }

    #[test]
    fn a_missing_scanner_fails_the_gate_rather_than_passing_it() {
        // ! The whole point of the stage. An absent trufflehog produces zero
        // findings; treating that as clean is how a preflight gate goes green
        // while nothing was scanned.
        let checks = vec![
            check("secret_scan", ScanStatus::ScannerUnavailable),
            check("dep_audit", ScanStatus::Clean),
        ];
        let verdict = gate_verdict(&checks).expect("unavailable scanner must fail the gate");
        assert!(verdict.message.contains("UNKNOWN"), "{}", verdict.message);
        assert!(verdict.message.contains("secret_scan"));
    }

    #[test]
    fn an_unavailable_scanner_is_distinguishable_from_a_clean_one_in_the_output() {
        // An agent must be able to tell "found nothing" from "could not check".
        let report = render_report(&[
            check("secret_scan", ScanStatus::ScannerUnavailable),
            check("dep_audit", ScanStatus::Clean),
        ]);
        assert!(report.contains("status=scanner_unavailable determined=false"));
        assert!(report.contains("status=clean determined=true"));
    }

    #[test]
    fn only_a_fully_clean_run_passes_the_gate() {
        assert!(
            gate_verdict(&[
                check("secret_scan", ScanStatus::Clean),
                check("dep_audit", ScanStatus::Clean),
            ])
            .is_none()
        );
        assert!(gate_verdict(&[check("secret_scan", ScanStatus::Findings)]).is_some());
    }

    #[test]
    fn the_secret_scan_passes_the_flag_that_makes_findings_fail() {
        // ! Regression guard: without --fail trufflehog exits 0 on findings.
        let args = trufflehog_docker_args(Path::new("/proj"));
        assert!(args.contains(&"--fail".to_owned()), "{args:?}");
        assert!(args.contains(&"--json".to_owned()));
        assert!(args.contains(&"--no-update".to_owned()));
        assert!(args.contains(&"filesystem".to_owned()));
        assert!(args.contains(&format!("/proj:{MOUNT_POINT}")));
        assert!(args.contains(&TRUFFLEHOG_IMAGE.to_owned()));
    }

    const SAMPLE: &str = r#"{"SourceMetadata":{"Data":{"Filesystem":{"file":"/work/creds.txt","line":7}}},"DetectorName":"AWS","Verified":false,"Raw":"AKIAZ3MFXQ7RB2KLWPQD"}"#;

    #[test]
    fn findings_carry_detector_file_and_line() {
        let f = parse_secret_findings(SAMPLE);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].detector, "AWS");
        assert_eq!(f[0].file, "/work/creds.txt");
        assert_eq!(f[0].line, 7);
    }

    #[test]
    fn a_secret_value_is_never_echoed_back() {
        // ! Secrets found during scanning are flagged and discarded. Rendering
        // Raw would relocate the credential into memory.db and the transcript.
        let rendered = render_findings(&parse_secret_findings(SAMPLE));
        assert!(!rendered.contains("AKIAZ3MFXQ7RB2KLWPQD"), "{rendered}");
        assert!(!rendered.contains("Raw"));
        assert!(rendered.contains("AWS · /work/creds.txt:7"));
    }

    #[test]
    fn non_json_log_lines_are_skipped() {
        let out = format!("time=... level=info msg=scanning\n{SAMPLE}\n");
        assert_eq!(parse_secret_findings(&out).len(), 1);
    }

    #[test]
    fn the_three_secret_scan_states_are_distinguishable_by_status_alone() {
        assert_eq!(classify_secret_scan(0, 0), ScanStatus::Clean);
        assert_eq!(
            classify_secret_scan(TRUFFLEHOG_FINDINGS_EXIT, 0),
            ScanStatus::Findings
        );
        assert_eq!(classify_secret_scan(0, 3), ScanStatus::Findings);
        // exit 125 = docker could not start the scanner · nothing was looked at.
        assert_eq!(classify_secret_scan(125, 0), ScanStatus::ScannerUnavailable);
    }

    #[test]
    fn a_missing_cargo_audit_subcommand_is_unknown_not_clean() {
        // `cargo` exists, `cargo audit` does not · exit code alone cannot tell
        // that from an advisory.
        assert_eq!(
            classify_dep_audit(101, "error: no such command: `audit`"),
            ScanStatus::ScannerUnavailable
        );
        assert_eq!(classify_dep_audit(0, ""), ScanStatus::Clean);
        assert_eq!(
            classify_dep_audit(1, "2 vulnerabilities found"),
            ScanStatus::Findings
        );
    }

    #[test]
    fn scan_status_ok_is_reserved_for_a_completed_clean_scan() {
        assert!(ScanStatus::Clean.is_ok());
        assert!(!ScanStatus::Findings.is_ok());
        assert!(!ScanStatus::ScannerUnavailable.is_ok());
    }

    #[test]
    fn overflowing_findings_are_reported_rather_than_silently_cut() {
        let findings: Vec<SecretFinding> = (0..MAX_FINDING_ROWS + 3)
            .map(|i| SecretFinding {
                detector: format!("D{i}"),
                file: "f".into(),
                line: 1,
            })
            .collect();
        let rendered = render_findings(&findings);
        assert!(rendered.contains("3 more not shown"), "{rendered}");
    }
}

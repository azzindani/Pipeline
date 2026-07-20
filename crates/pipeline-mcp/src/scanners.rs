//! Scanner command construction · finding parsers · three-state verdicts.
//!
//! ! A gate that cannot fail is worse than no gate: an agent gating a push on
//! `ok` pushes the secrets. Scanners default to exit 0 even when they find
//! live credentials or CRITICAL CVEs, so the failing flag (`--fail` ·
//! `--exit-code 1`) is part of the command here, ✗ an option a caller may drop.
//!
//! ! "No findings" and "scanner never ran" are different answers. Both are
//! reported, never merged: `Clean` is a determination, `ScannerUnavailable` is
//! the absence of one, and a gate must be free to treat them differently.

use crate::tools::ToolResponse;
use serde_json::{Value, json};

pub const TRUFFLEHOG_IMAGE: &str = "trufflesecurity/trufflehog:latest";
pub const TRIVY_IMAGE: &str = "aquasec/trivy:latest";
pub const HADOLINT_IMAGE: &str = "hadolint/hadolint:latest";

/// trufflehog `--fail` exits with this code when results are found ·
/// any other nonzero is a scanner fault, ✗ a finding.
pub const TRUFFLEHOG_FINDINGS_EXIT: i32 = 183;

/// Cap on returned vulnerability rows · overflow is reported, ✗ silently cut.
pub const MAX_VULN_ROWS: usize = 100;

/// Verdict of a scan. Three states, always distinguishable by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    /// Scanner ran to completion and found nothing.
    Clean,
    /// Scanner ran and found something · gate must fail.
    Findings,
    /// Scanner did not produce a verdict · UNKNOWN, ✗ pass and ✗ fail.
    ScannerUnavailable,
}

impl ScanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Findings => "findings",
            Self::ScannerUnavailable => "scanner_unavailable",
        }
    }

    /// Only a completed clean scan is `ok` · UNKNOWN must never read as pass.
    pub fn is_ok(self) -> bool {
        self == Self::Clean
    }
}

// ---------- trufflehog ----------

/// Where trufflehog looks · `git` walks history, `filesystem` only the worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretScope {
    Filesystem,
    Git,
}

impl SecretScope {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "filesystem" | "fs" => Ok(Self::Filesystem),
            "git" => Ok(Self::Git),
            other => Err(format!("unknown scope '{other}' · filesystem | git")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Git => "git",
        }
    }
}

/// trufflehog argv after the image · `--fail` is what makes findings nonzero.
/// `mount_point` is the in-container path the worktree is bound to.
pub fn trufflehog_cmd(scope: SecretScope, mount_point: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--json".into(),
        "--no-update".into(),
        // ! without --fail trufflehog exits 0 with live credentials in hand.
        "--fail".into(),
    ];
    match scope {
        SecretScope::Filesystem => {
            args.push("filesystem".into());
            args.push(mount_point.to_owned());
        }
        SecretScope::Git => {
            // git subcommand takes a repo URI · file:// scans local history.
            args.push("git".into());
            args.push(format!("file://{mount_point}"));
        }
    }
    args
}

/// One secret hit · location only. ✗ the secret value: found secrets are
/// flagged and discarded, never stored or echoed back to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    pub detector: String,
    pub file: String,
    pub line: i64,
    pub verified: bool,
}

impl SecretFinding {
    fn to_json(&self) -> Value {
        json!({
            "detector": self.detector,
            "file": self.file,
            "line": self.line,
            "verified": self.verified,
        })
    }
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
    let data = v.pointer("/SourceMetadata/Data").and_then(Value::as_object);
    let loc = data.and_then(|m| m.values().next());
    let file = loc
        .and_then(|l| l.get("file"))
        .and_then(Value::as_str)
        .unwrap_or("<unknown>")
        .to_owned();
    let line = loc
        .and_then(|l| l.get("line"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some(SecretFinding {
        detector,
        file,
        line,
        verified: v.get("Verified").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Classify a trufflehog run. An empty finding list is only `Clean` when the
/// scanner actually exited cleanly · ! never report "clean" for a scan that
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

/// Build the secret-scan response · `stderr_tail` explains an unavailable scanner.
pub fn secret_scan_response(
    scope: SecretScope,
    exit_code: i32,
    findings: &[SecretFinding],
    stderr_tail: &str,
) -> ToolResponse {
    let status = classify_secret_scan(exit_code, findings.len());
    let rows: Vec<Value> = findings.iter().map(SecretFinding::to_json).collect();
    let error = match status {
        ScanStatus::Clean => None,
        ScanStatus::Findings => Some(format!(
            "secret_scan({}) found {} secret(s) · ✗ push",
            scope.as_str(),
            findings.len()
        )),
        ScanStatus::ScannerUnavailable => Some(format!(
            "secret_scan({}) did not run · UNKNOWN, ✗ clean · exit {exit_code} · {stderr_tail}",
            scope.as_str()
        )),
    };
    ToolResponse {
        ok: status.is_ok(),
        data: json!({
            "scope": scope.as_str(),
            "status": status.as_str(),
            "scanner": "trufflehog",
            "exit_code": exit_code,
            "findings_count": findings.len(),
            "findings": rows,
            "note": "locations only · secret values are discarded, never returned",
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error,
    }
}

// ---------- trivy ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrivyTarget<'a> {
    /// Scan a built image by ref · needs the docker socket bound in.
    Image(&'a str),
    /// Scan a directory bound into the container.
    Filesystem(&'a str),
}

const SEVERITIES: [&str; 5] = ["UNKNOWN", "LOW", "MEDIUM", "HIGH", "CRITICAL"];

/// Normalise a caller-supplied severity list · rejects typos rather than
/// silently scanning for a level that does not exist.
pub fn normalise_severity(raw: &str) -> Result<String, String> {
    let mut out: Vec<String> = Vec::new();
    for tok in raw.split(',') {
        let t = tok.trim().to_uppercase();
        if t.is_empty() {
            continue;
        }
        if !SEVERITIES.contains(&t.as_str()) {
            return Err(format!(
                "unknown severity '{}' · {}",
                tok.trim(),
                SEVERITIES.join(" | ")
            ));
        }
        out.push(t);
    }
    if out.is_empty() {
        return Err("empty severity list".into());
    }
    Ok(out.join(","))
}

/// Trivy argv after the image · `--exit-code 1` is what makes findings nonzero.
pub fn trivy_cmd(target: TrivyTarget<'_>, severity: &str) -> Vec<String> {
    let (mode, subject) = match target {
        TrivyTarget::Image(i) => ("image", i),
        TrivyTarget::Filesystem(p) => ("fs", p),
    };
    vec![
        mode.into(),
        // ! without --exit-code trivy reports CRITICAL CVEs and exits 0.
        "--exit-code".into(),
        "1".into(),
        "--severity".into(),
        severity.to_owned(),
        "--format".into(),
        "json".into(),
        "--no-progress".into(),
        subject.to_owned(),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vuln {
    pub vulnerability_id: String,
    pub pkg_name: String,
    pub installed_version: String,
    pub fixed_version: String,
    pub severity: String,
}

impl Vuln {
    fn to_json(&self) -> Value {
        json!({
            "vulnerability_id": self.vulnerability_id,
            "pkg_name": self.pkg_name,
            "installed_version": self.installed_version,
            "fixed_version": self.fixed_version,
            "severity": self.severity,
        })
    }
}

/// Structured Trivy result · absence of this value means the scan produced no
/// parseable report, i.e. the scanner did not run.
#[derive(Debug, Clone, Default)]
pub struct VulnReport {
    pub total: usize,
    pub by_severity: Vec<(String, usize)>,
    pub vulns: Vec<Vuln>,
}

/// Parse Trivy's `--format json` report. `None` → not a Trivy report, so the
/// run is a scanner fault, ✗ a clean result.
pub fn parse_trivy_report(stdout: &str) -> Option<VulnReport> {
    let doc: Value = serde_json::from_str(stdout.trim()).ok()?;
    // SchemaVersion is present on every Trivy report · its absence means we are
    // looking at an error dump, not a verdict.
    doc.get("SchemaVersion")?;
    let mut rep = VulnReport::default();
    let results = doc.get("Results").and_then(Value::as_array);
    for r in results.into_iter().flatten() {
        let list = r.get("Vulnerabilities").and_then(Value::as_array);
        for v in list.into_iter().flatten() {
            rep.vulns.push(vuln_from(v));
        }
    }
    rep.total = rep.vulns.len();
    for sev in SEVERITIES.iter().rev() {
        let n = rep.vulns.iter().filter(|v| v.severity == *sev).count();
        if n > 0 {
            rep.by_severity.push(((*sev).to_owned(), n));
        }
    }
    Some(rep)
}

fn vuln_from(v: &Value) -> Vuln {
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Vuln {
        vulnerability_id: s("VulnerabilityID"),
        pkg_name: s("PkgName"),
        installed_version: s("InstalledVersion"),
        fixed_version: v
            .get("FixedVersion")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_owned(),
        severity: s("Severity"),
    }
}

/// Classify a Trivy run · an unparseable report is UNKNOWN regardless of exit.
pub fn classify_vuln_scan(exit_code: i32, report: Option<&VulnReport>) -> ScanStatus {
    match report {
        None => ScanStatus::ScannerUnavailable,
        Some(r) if r.total > 0 || exit_code != 0 => ScanStatus::Findings,
        Some(_) => ScanStatus::Clean,
    }
}

/// Build the response shared by `security.vuln_scan` and `docker.image_scan`.
pub fn vuln_scan_response(
    label: &str,
    severity: &str,
    exit_code: i32,
    report: Option<&VulnReport>,
    stderr_tail: &str,
) -> ToolResponse {
    let status = classify_vuln_scan(exit_code, report);
    let empty = VulnReport::default();
    let r = report.unwrap_or(&empty);
    let shown = r.vulns.len().min(MAX_VULN_ROWS);
    let dropped = r.vulns.len() - shown;
    let rows: Vec<Value> = r.vulns[..shown].iter().map(Vuln::to_json).collect();
    let by_sev: serde_json::Map<String, Value> = r
        .by_severity
        .iter()
        .map(|(k, n)| (k.clone(), json!(n)))
        .collect();
    let error = match status {
        ScanStatus::Clean => None,
        ScanStatus::Findings => Some(format!(
            "{label} found {} vulnerabilit(ies) at {severity}",
            r.total
        )),
        ScanStatus::ScannerUnavailable => Some(format!(
            "{label} produced no report · UNKNOWN, ✗ clean · exit {exit_code} · {stderr_tail}"
        )),
    };
    ToolResponse {
        ok: status.is_ok(),
        data: json!({
            "command": label,
            "status": status.as_str(),
            "scanner": "trivy",
            "severity": severity,
            "exit_code": exit_code,
            "total": r.total,
            "by_severity": by_sev,
            "vulnerabilities": rows,
            "truncated": dropped > 0,
            "dropped": dropped,
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error,
    }
}

/// Last `max` bytes of scanner stderr · enough to explain a failed spawn
/// without dragging a full log into the response.
pub fn tail(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.len() <= max {
        return t.to_owned();
    }
    let start = t.len() - max;
    let start = (start..t.len())
        .find(|i| t.is_char_boundary(*i))
        .unwrap_or(t.len());
    format!("...{}", &t[start..])
}

#[cfg(test)]
mod trufflehog_tests {
    use super::*;

    #[test]
    fn every_secret_scan_passes_the_flag_that_makes_findings_fail() {
        // ! Regression: without --fail trufflehog exits 0 holding live keys and
        // the gate reports ok:true.
        for scope in [SecretScope::Filesystem, SecretScope::Git] {
            let args = trufflehog_cmd(scope, "/work");
            assert!(
                args.contains(&"--fail".to_owned()),
                "{scope:?} must pass --fail"
            );
        }
    }

    #[test]
    fn git_scope_uses_the_git_subcommand_not_filesystem() {
        // Regression: scope was accepted, labelled in the response, and ignored
        // — so scope="git" claimed to scan history and scanned the worktree.
        let args = trufflehog_cmd(SecretScope::Git, "/work");
        assert!(args.contains(&"git".to_owned()));
        assert!(!args.contains(&"filesystem".to_owned()));
        assert!(args.contains(&"file:///work".to_owned()));
    }

    #[test]
    fn filesystem_scope_uses_the_filesystem_subcommand() {
        let args = trufflehog_cmd(SecretScope::Filesystem, "/work");
        assert!(args.contains(&"filesystem".to_owned()));
        assert!(!args.contains(&"git".to_owned()));
    }

    #[test]
    fn an_unknown_scope_is_rejected_rather_than_defaulted() {
        assert!(SecretScope::parse("history").is_err());
    }

    const SAMPLE: &str = r#"{"SourceMetadata":{"Data":{"Filesystem":{"file":"/work/creds.txt","line":1}}},"DetectorName":"AWS","Verified":false,"Raw":"AKIAZ3MFXQ7RB2KLWPQD","RawV2":"AKIAZ3MFXQ7RB2KLWPQD:h8Kd0pQ"}"#;

    #[test]
    fn findings_carry_location_only() {
        let f = parse_secret_findings(SAMPLE);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].detector, "AWS");
        assert_eq!(f[0].file, "/work/creds.txt");
        assert_eq!(f[0].line, 1);
    }

    #[test]
    fn a_secret_value_is_never_echoed_back() {
        // ! The audit rule: secrets found during scanning are flagged and
        // discarded. Leaking Raw into the response would relocate the secret
        // into memory.db and the agent transcript.
        let resp = secret_scan_response(
            SecretScope::Filesystem,
            TRUFFLEHOG_FINDINGS_EXIT,
            &parse_secret_findings(SAMPLE),
            "",
        );
        let body = serde_json::to_string(&resp.data).unwrap();
        assert!(
            !body.contains("AKIAZ3MFXQ7RB2KLWPQD"),
            "secret leaked: {body}"
        );
        assert!(!body.contains("Raw"));
    }

    #[test]
    fn non_json_log_lines_are_skipped() {
        let out = format!("garbage\n{SAMPLE}\n");
        assert_eq!(parse_secret_findings(&out).len(), 1);
    }

    #[test]
    fn a_scan_that_finds_secrets_fails_the_call() {
        let f = parse_secret_findings(SAMPLE);
        let resp = secret_scan_response(SecretScope::Filesystem, TRUFFLEHOG_FINDINGS_EXIT, &f, "");
        assert!(!resp.ok);
        assert!(resp.error.is_some());
        assert_eq!(resp.data["status"], "findings");
    }

    #[test]
    fn a_clean_scan_passes_the_call() {
        let resp = secret_scan_response(SecretScope::Filesystem, 0, &[], "");
        assert!(resp.ok);
        assert!(resp.error.is_none());
        assert_eq!(resp.data["status"], "clean");
    }

    #[test]
    fn an_empty_finding_list_from_a_scanner_that_never_ran_is_not_clean() {
        // ! The three-state invariant. Exit 125 = docker could not start the
        // scanner; zero findings then means nothing was looked at.
        let resp = secret_scan_response(SecretScope::Filesystem, 125, &[], "no such image");
        assert!(!resp.ok, "unavailable scanner must never read as pass");
        assert_eq!(resp.data["status"], "scanner_unavailable");
        assert!(resp.error.unwrap().contains("UNKNOWN"));
    }

    #[test]
    fn the_three_states_are_distinguishable_by_status_alone() {
        let s = |code, n| classify_secret_scan(code, n);
        assert_eq!(s(0, 0), ScanStatus::Clean);
        assert_eq!(s(TRUFFLEHOG_FINDINGS_EXIT, 3), ScanStatus::Findings);
        assert_eq!(s(1, 0), ScanStatus::ScannerUnavailable);
    }
}

#[cfg(test)]
mod trivy_tests {
    use super::*;

    #[test]
    fn every_trivy_mode_passes_the_flag_that_makes_findings_fail() {
        // ! Verified live: `trivy image debian:11` reports 24 HIGH/CRITICAL and
        // exits 0 without --exit-code.
        for t in [
            TrivyTarget::Image("app:latest"),
            TrivyTarget::Filesystem("/work"),
        ] {
            let args = trivy_cmd(t, "CRITICAL,HIGH");
            let i = args.iter().position(|a| a == "--exit-code").expect("flag");
            assert_eq!(args[i + 1], "1", "{t:?} must fail on findings");
        }
    }

    #[test]
    fn severity_reaches_the_command_rather_than_being_hardcoded() {
        let args = trivy_cmd(TrivyTarget::Image("app:latest"), "MEDIUM");
        let i = args.iter().position(|a| a == "--severity").expect("flag");
        assert_eq!(args[i + 1], "MEDIUM");
    }

    #[test]
    fn image_mode_scans_the_image_and_filesystem_mode_the_path() {
        assert_eq!(trivy_cmd(TrivyTarget::Image("a:1"), "HIGH")[0], "image");
        assert_eq!(trivy_cmd(TrivyTarget::Filesystem("/work"), "HIGH")[0], "fs");
    }

    #[test]
    fn severity_is_normalised_and_typos_are_rejected() {
        assert_eq!(
            normalise_severity("high, critical").unwrap(),
            "HIGH,CRITICAL"
        );
        assert!(normalise_severity("SEVERE").is_err());
        assert!(normalise_severity(" ").is_err());
    }

    const REPORT: &str = r#"{"SchemaVersion":2,"ArtifactName":"debian:11","Results":[{"Target":"debian:11","Vulnerabilities":[
      {"VulnerabilityID":"CVE-2023-1","PkgName":"openssl","InstalledVersion":"1.1","FixedVersion":"1.2","Severity":"CRITICAL"},
      {"VulnerabilityID":"CVE-2023-2","PkgName":"zlib","InstalledVersion":"1.0","Severity":"HIGH"}]}]}"#;

    #[test]
    fn findings_are_parsed_into_structured_rows() {
        let r = parse_trivy_report(REPORT).expect("report");
        assert_eq!(r.total, 2);
        assert_eq!(r.vulns[0].vulnerability_id, "CVE-2023-1");
        assert_eq!(r.vulns[0].pkg_name, "openssl");
        assert_eq!(r.vulns[0].fixed_version, "1.2");
        // A vuln with no fix must say so rather than emit an empty string.
        assert_eq!(r.vulns[1].fixed_version, "none");
        assert_eq!(
            r.by_severity,
            vec![("CRITICAL".to_owned(), 1), ("HIGH".to_owned(), 1)]
        );
    }

    #[test]
    fn a_scan_that_finds_cves_fails_the_call() {
        let r = parse_trivy_report(REPORT).unwrap();
        let resp = vuln_scan_response("vuln_scan(debian:11)", "CRITICAL,HIGH", 1, Some(&r), "");
        assert!(!resp.ok);
        assert_eq!(resp.data["status"], "findings");
        assert_eq!(resp.data["total"], 2);
        assert_eq!(resp.data["by_severity"]["CRITICAL"], 1);
    }

    #[test]
    fn a_clean_report_passes_the_call() {
        let r = parse_trivy_report(r#"{"SchemaVersion":2,"Results":[]}"#).unwrap();
        let resp = vuln_scan_response("vuln_scan(app)", "CRITICAL,HIGH", 0, Some(&r), "");
        assert!(resp.ok);
        assert_eq!(resp.data["status"], "clean");
        assert_eq!(resp.data["total"], 0);
    }

    #[test]
    fn a_fatal_scanner_error_is_unknown_not_clean() {
        // ! Verified live: image_scan without the docker socket returns
        // "FATAL unable to find the specified image" — zero findings.
        assert!(parse_trivy_report("FATAL unable to find image").is_none());
        let resp = vuln_scan_response(
            "image_scan(local:dev)",
            "CRITICAL,HIGH",
            1,
            None,
            "FATAL unable to find the specified image",
        );
        assert!(!resp.ok);
        assert_eq!(resp.data["status"], "scanner_unavailable");
        assert_eq!(resp.data["total"], 0);
    }

    #[test]
    fn a_nonzero_exit_with_an_empty_report_still_fails() {
        // Trivy said something was wrong · never round that down to clean.
        let r = VulnReport::default();
        let resp = vuln_scan_response("vuln_scan(x)", "HIGH", 1, Some(&r), "");
        assert!(!resp.ok);
    }

    #[test]
    fn dropped_rows_are_reported_rather_than_silently_cut() {
        // The old handler truncated free text at 8000 bytes with no signal.
        let mut r = VulnReport::default();
        for i in 0..(MAX_VULN_ROWS + 7) {
            r.vulns.push(Vuln {
                vulnerability_id: format!("CVE-{i}"),
                pkg_name: "p".into(),
                installed_version: "1".into(),
                fixed_version: "2".into(),
                severity: "HIGH".into(),
            });
        }
        r.total = r.vulns.len();
        let resp = vuln_scan_response("vuln_scan(x)", "HIGH", 1, Some(&r), "");
        assert_eq!(resp.data["truncated"], true);
        assert_eq!(resp.data["dropped"], 7);
        assert_eq!(resp.data["total"], MAX_VULN_ROWS + 7);
        assert_eq!(
            resp.data["vulnerabilities"].as_array().unwrap().len(),
            MAX_VULN_ROWS
        );
    }

    #[test]
    fn tail_keeps_the_end_of_a_long_stderr() {
        assert_eq!(tail("short", 100), "short");
        let t = tail(&"x".repeat(50), 10);
        assert!(t.starts_with("..."));
        assert_eq!(t.len(), 13);
    }
}

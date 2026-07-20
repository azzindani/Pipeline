//! `pipeline_security` handler · secret_scan · vuln_scan · dep_audit.
//!
//! Day-6 wires all 3 scan actions via Docker-run on standard images.
//! threat_model + compliance_check return not_implemented (need framework
//! catalog and structured gap reports · MVP+ work).
//!
//! ! Every action here is a gate an agent may key a push on. Command
//! construction and verdicts live in `crate::scanners` so the failing flags
//! (`--fail` · `--exit-code 1`) are asserted by tests, ✗ trusted to review.

#![allow(clippy::doc_markdown)]

use crate::scanners::{
    self, ScanStatus, SecretScope, TRIVY_IMAGE, TRUFFLEHOG_IMAGE, TrivyTarget, tail,
};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

const MOUNT_POINT: &str = "/work";

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "secret_scan" => secret_scan(&req.args).await,
        "vuln_scan" => vuln_scan(&req.args).await,
        "dep_audit" => dep_audit(&req.args).await,
        "threat_model" => threat_model(&req.args).await,
        "compliance_check" => compliance_check(&req.args).await,
        other => err(format!("unknown action 'pipeline_security.{other}'")),
    }
}

async fn secret_scan(args: &Value) -> ToolResponse {
    let scope = match SecretScope::parse(
        args.get("scope")
            .and_then(Value::as_str)
            .unwrap_or("filesystem"),
    ) {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let host = cwd.display().to_string();
    let cmd = scanners::trufflehog_cmd(scope, MOUNT_POINT);
    let cmd_refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    let out = match pipeline_docker::run_image(
        TRUFFLEHOG_IMAGE,
        &cmd_refs,
        &[],
        &[(host.as_str(), MOUNT_POINT)],
    )
    .await
    {
        Ok(o) => o,
        // Docker itself did not start · UNKNOWN, ✗ clean.
        Err(e) => return scanners::secret_scan_response(scope, -1, &[], &e.to_string()),
    };
    let findings = scanners::parse_secret_findings(&out.stdout);
    scanners::secret_scan_response(scope, out.exit_code, &findings, &tail(&out.stderr, 600))
}

async fn vuln_scan(args: &Value) -> ToolResponse {
    let target = args.get("target").and_then(Value::as_str);
    let severity = match scanners::normalise_severity(
        args.get("severity")
            .and_then(Value::as_str)
            .unwrap_or("CRITICAL,HIGH"),
    ) {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let host = cwd.display().to_string();
    // Image mode needs the socket to reach locally-built images · fs mode needs
    // the worktree bound in.
    let (trivy_target, volume) = match target {
        Some(image) => (
            TrivyTarget::Image(image),
            ("/var/run/docker.sock", "/var/run/docker.sock"),
        ),
        None => (
            TrivyTarget::Filesystem(MOUNT_POINT),
            (host.as_str(), MOUNT_POINT),
        ),
    };
    let label = format!("vuln_scan({})", target.unwrap_or("filesystem"));
    run_trivy(trivy_target, &severity, &[volume], &label).await
}

/// Shared by `security.vuln_scan` and `docker.image_scan` · one command
/// builder, one verdict path, so neither can drift into passing on findings.
pub(crate) async fn run_trivy(
    target: TrivyTarget<'_>,
    severity: &str,
    volumes: &[(&str, &str)],
    label: &str,
) -> ToolResponse {
    let cmd = scanners::trivy_cmd(target, severity);
    let cmd_refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    let out = match pipeline_docker::run_image(TRIVY_IMAGE, &cmd_refs, &[], volumes).await {
        Ok(o) => o,
        Err(e) => return scanners::vuln_scan_response(label, severity, -1, None, &e.to_string()),
    };
    let report = scanners::parse_trivy_report(&out.stdout);
    scanners::vuln_scan_response(
        label,
        severity,
        out.exit_code,
        report.as_ref(),
        &tail(&out.stderr, 600),
    )
}

async fn dep_audit(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (program, cmd_args) = match dep_audit_command(stack) {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let output = match Command::new(program)
        .args(&cmd_args)
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        // ! Binary absent → UNKNOWN. Reporting this as a failed audit is as
        // wrong as reporting it clean: neither says whether a CVE exists.
        Err(e) => {
            return dep_audit_response(
                stack,
                program,
                ScanStatus::ScannerUnavailable,
                -1,
                "",
                &format!("spawn: {e}"),
            );
        }
    };
    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = classify_dep_audit(code, &stderr);
    dep_audit_response(
        stack,
        program,
        status,
        code,
        &tail(&String::from_utf8_lossy(&output.stdout), 6_000),
        &tail(&stderr, 2_000),
    )
}

fn dep_audit_command(stack: &str) -> Result<(&'static str, Vec<&'static str>), String> {
    match stack {
        "rust" => Ok(("cargo", vec!["audit"])),
        "node" | "ts" | "typescript" => Ok(("npm", vec!["audit", "--audit-level=high"])),
        "bun" => Ok(("bun", vec!["pm", "audit"])),
        "python" | "python-uv" => Ok(("pip-audit", vec![])),
        other => Err(format!("unsupported stack '{other}'")),
    }
}

/// A present launcher with an absent subcommand (`cargo audit` without
/// cargo-audit installed) is still an absent scanner · exit code alone cannot
/// tell that from a CVE, so the stderr shape decides.
fn classify_dep_audit(code: i32, stderr: &str) -> ScanStatus {
    let s = stderr.to_lowercase();
    let missing = s.contains("no such command")
        || s.contains("command not found")
        || s.contains("not recognized")
        || s.contains("unknown command");
    if missing {
        ScanStatus::ScannerUnavailable
    } else if code == 0 {
        ScanStatus::Clean
    } else {
        ScanStatus::Findings
    }
}

fn dep_audit_response(
    stack: &str,
    program: &str,
    status: ScanStatus,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> ToolResponse {
    // "pass" reads clearer than "clean" for an audit · the other two names are
    // shared with the scan actions so a gate branches on one vocabulary.
    let status_str = match status {
        ScanStatus::Clean => "pass",
        ScanStatus::Findings => "findings",
        ScanStatus::ScannerUnavailable => "scanner_missing",
    };
    let error = match status {
        ScanStatus::Clean => None,
        ScanStatus::Findings => Some(format!("dep_audit({stack}) reported advisories")),
        ScanStatus::ScannerUnavailable => Some(format!(
            "dep_audit({stack}) UNKNOWN · '{program}' unavailable · ✗ pass, ✗ fail · install it to get a verdict"
        )),
    };
    ToolResponse {
        ok: status.is_ok(),
        data: json!({
            "command": format!("dep_audit({stack})"),
            "stack": stack,
            "scanner": program,
            "status": status_str,
            "determined": status != ScanStatus::ScannerUnavailable,
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error,
    }
}

#[allow(clippy::unused_async)]
async fn threat_model(args: &Value) -> ToolResponse {
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("application");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("security");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join("threat-model.md");
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    let body = format!(
        "# Threat model · scope = {scope}\n\nGenerated by pipeline_security.threat_model.\n\n## Spoofing\n\n## Tampering\n\n## Repudiation\n\n## Information disclosure\n\n## Denial of service\n\n## Elevation of privilege\n\n## Trust boundaries\n\n## Mitigations\n\n## Open questions\n"
    );
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(
        json!({"scope": scope, "path": path.display().to_string(), "framework": "STRIDE"}),
    )
}

#[allow(clippy::unused_async)]
async fn compliance_check(args: &Value) -> ToolResponse {
    let framework = args
        .get("framework")
        .and_then(Value::as_str)
        .unwrap_or("standards");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Cheap heuristic checks · agent extends.
    let mut checks: Vec<Value> = Vec::new();
    let dockerfile = cwd.join("Dockerfile");
    if dockerfile.exists() {
        let body = std::fs::read_to_string(&dockerfile).unwrap_or_default();
        checks.push(json!({
            "name": "dockerfile_non_root",
            "passed": body.contains("USER ") && !body.contains("USER root"),
        }));
        checks.push(json!({
            "name": "dockerfile_pinned_base",
            "passed": !body.contains(":latest"),
        }));
    }
    let env_file = cwd.join(".env");
    checks.push(
        json!({"name": "no_committed_env", "passed": !env_file.exists() || ignored(".env", &cwd)}),
    );
    let gitignore = cwd.join(".gitignore");
    checks.push(json!({"name": "has_gitignore", "passed": gitignore.exists()}));
    checks.push(json!({"name": "has_pipeline_yaml", "passed": cwd.join("pipeline.yaml").exists()}));

    let passed = checks
        .iter()
        .filter(|c| c.get("passed").and_then(Value::as_bool) == Some(true))
        .count();
    let total = checks.len();
    #[allow(clippy::cast_precision_loss)]
    let score = if total == 0 {
        0.0
    } else {
        (passed as f64 / total as f64) * 100.0
    };
    ToolResponse::ok(json!({
        "framework": framework,
        "checks": checks,
        "score_percent": score,
        "passed": passed,
        "total": total,
    }))
}

fn ignored(needle: &str, cwd: &std::path::Path) -> bool {
    let g = cwd.join(".gitignore");
    std::fs::read_to_string(&g).is_ok_and(|s| s.lines().any(|l| l.trim() == needle))
}

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

#[cfg(test)]
mod dep_audit_tests {
    use super::{ScanStatus, classify_dep_audit, dep_audit_command, dep_audit_response};

    #[test]
    fn each_supported_stack_maps_to_its_audit_binary() {
        assert_eq!(dep_audit_command("rust").unwrap().0, "cargo");
        assert_eq!(dep_audit_command("bun").unwrap().0, "bun");
        assert_eq!(dep_audit_command("python-uv").unwrap().0, "pip-audit");
        assert!(dep_audit_command("cobol").is_err());
    }

    #[test]
    fn a_missing_scanner_is_unknown_never_pass_and_never_fail() {
        // ! Regression: a missing binary and a real CVE both produced ok:false
        // with no way to tell them apart, so a gate could not distinguish
        // "you have a vulnerability" from "nothing was checked".
        let resp = dep_audit_response(
            "rust",
            "cargo",
            ScanStatus::ScannerUnavailable,
            -1,
            "",
            "spawn: No such file",
        );
        assert!(!resp.ok, "unavailable must not read as pass");
        assert_eq!(resp.data["status"], "scanner_missing");
        assert_eq!(resp.data["determined"], false);
        assert!(resp.error.unwrap().contains("UNKNOWN"));
    }

    #[test]
    fn a_real_advisory_is_findings_and_is_marked_determined() {
        let resp = dep_audit_response("rust", "cargo", ScanStatus::Findings, 1, "RUSTSEC", "");
        assert!(!resp.ok);
        assert_eq!(resp.data["status"], "findings");
        assert_eq!(resp.data["determined"], true);
    }

    #[test]
    fn a_clean_audit_passes_and_is_marked_determined() {
        let resp = dep_audit_response("rust", "cargo", ScanStatus::Clean, 0, "0 vulns", "");
        assert!(resp.ok);
        assert_eq!(resp.data["status"], "pass");
        assert_eq!(resp.data["determined"], true);
        assert!(resp.error.is_none());
    }

    #[test]
    fn the_three_dep_audit_states_have_three_distinct_status_strings() {
        let s = |st| {
            dep_audit_response("rust", "cargo", st, 0, "", "").data["status"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let all = [
            s(ScanStatus::Clean),
            s(ScanStatus::Findings),
            s(ScanStatus::ScannerUnavailable),
        ];
        let mut uniq = all.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 3, "states collapsed: {all:?}");
    }

    #[test]
    fn cargo_present_but_cargo_audit_absent_is_still_a_missing_scanner() {
        // `cargo audit` without cargo-audit exits 101 — indistinguishable from
        // an advisory by exit code alone.
        assert_eq!(
            classify_dep_audit(101, "error: no such command: `audit`"),
            ScanStatus::ScannerUnavailable
        );
    }

    #[test]
    fn a_nonzero_audit_with_ordinary_stderr_is_a_finding() {
        assert_eq!(
            classify_dep_audit(1, "2 vulnerabilities found"),
            ScanStatus::Findings
        );
        assert_eq!(classify_dep_audit(0, ""), ScanStatus::Clean);
    }
}

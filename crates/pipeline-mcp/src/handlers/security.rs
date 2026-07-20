//! `pipeline_security` handler · secret_scan · vuln_scan · dep_audit.
//!
//! Day-6 wires all 3 scan actions via Docker-run on standard images.
//! threat_model writes a STRIDE skeleton (scaffold · reads nothing).
//!
//! ! Every action here is a gate an agent may key a push on. Command
//! construction and verdicts live in `crate::scanners` so the failing flags
//! (`--fail` · `--exit-code 1`) are asserted by tests, ✗ trusted to review.
//!
//! `compliance_check` assesses ONE framework — `standards` — against the real
//! Standards corpus, and refuses every regulatory framework by name. ! The old
//! version read `framework`, echoed it, and never branched on it: five
//! file-existence checks produced `score_percent: 100, framework: "hipaa"` for
//! a repo with a .gitignore and no Dockerfile, which an agent then relayed to
//! its user as HIPAA compliance. A refusal is the only honest answer for a
//! framework whose controls do not live in a worktree.

#![allow(clippy::doc_markdown)]

use crate::scanners::{
    self, ScanStatus, SecretScope, TRIVY_IMAGE, TRUFFLEHOG_IMAGE, TrivyTarget, tail,
};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use pipeline_standards as std_lib;
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

// ---------- compliance ----------

/// The only framework Pipeline can assess from a worktree.
const ASSESSABLE: &str = "standards";

/// Frameworks that are refused, each with the reason it cannot be decided by
/// reading a repository.
///
/// ! These are refusals, ✗ gaps waiting for more checks. No amount of file
/// inspection turns into a HIPAA verdict: the controls are organisational,
/// contractual, and operational. Scoring one anyway is not an approximation —
/// it is a false attestation an agent will relay to a human as fact.
const REFUSED: &[(&str, &str)] = &[
    (
        "hipaa",
        "HIPAA turns on administrative · physical · technical safeguards — BAAs with every \
         processor, workforce training, audit-log retention, facility access control, breach \
         notification procedure. None of those exist in a source tree.",
    ),
    (
        "pci_dss",
        "PCI-DSS is scoped to the cardholder data environment: network segmentation, key \
         management and rotation, quarterly ASV scans, physical media handling, and an \
         assessor's report. A repo inspection observes none of it.",
    ),
    (
        "gdpr",
        "GDPR turns on lawful basis, data-subject rights, processor agreements, transfer \
         mechanism, and retention policy — legal and organisational facts, ✗ code facts.",
    ),
    (
        "iso27001",
        "ISO 27001 certifies an ISMS: risk treatment plan, statement of applicability, \
         management review, internal audit. It is an organisational audit, ✗ a repo scan.",
    ),
    (
        "soc2",
        "SOC 2 is an auditor's opinion on operating effectiveness across a period. Pipeline \
         observes a point-in-time worktree and cannot evidence a period at all.",
    ),
    (
        "owasp",
        "OWASP Top 10 / ASVS require exercising the running application — authentication \
         flows, access control, injection. Static repo inspection cannot decide them. Nearest \
         real coverage: security.secret_scan · vuln_scan · dep_audit · docker.image_scan.",
    ),
];

async fn compliance_check(args: &Value) -> ToolResponse {
    let framework = args
        .get("framework")
        .and_then(Value::as_str)
        .unwrap_or(ASSESSABLE)
        .to_ascii_lowercase();
    if let Some((_, reason)) = REFUSED.iter().find(|(n, _)| *n == framework) {
        return refuse_framework(&framework, reason);
    }
    if framework != ASSESSABLE {
        let refused: Vec<&str> = REFUSED.iter().map(|(n, _)| *n).collect();
        return err(format!(
            "unknown framework '{framework}' · assessable: {ASSESSABLE} · \
             explicitly refused (not decidable from a worktree): {}",
            refused.join(" · ")
        ));
    }
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    compliance_standards(&cwd).await
}

/// ! `ok: false` · a refusal is not a pass. An agent that keys a gate on `ok`
/// must not be told an unassessable framework is satisfied.
fn refuse_framework(framework: &str, reason: &str) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({
            "framework": framework,
            "assessable": false,
            "scored": false,
            "assessable_frameworks": [ASSESSABLE],
            "reason": reason,
            "note": "Pipeline reports what it observed · it ✗ attests to compliance it cannot compute.",
        }),
        next_suggested: vec![
            "pipeline_security.compliance_check(framework=standards)".into(),
            "pipeline_security.threat_model".into(),
        ],
        memory_refs: vec![],
        error: Some(format!(
            "{framework} cannot be assessed by static repo inspection · {reason}"
        )),
    }
}

/// `framework=standards` · the real one.
///
/// Obligations come from the Standards corpus the project actually routes to,
/// resolved and pinned by `pipeline-standards`. ! Checklist items are prose;
/// scoring them is agent work, ✗ regex work — so this returns the obligations
/// plus the mechanical signals it CAN decide, and refuses to emit a percentage
/// it did not compute. Same contract as `standards.check`.
async fn compliance_standards(cwd: &std::path::Path) -> ToolResponse {
    let cfg = match pipeline_config::PipelineConfig::load(cwd.join("pipeline.yaml")) {
        Ok(c) => c,
        Err(e) => {
            return err(format!(
                "no readable pipeline.yaml at {} ({e}) · the routed standards set is what \
                 defines the obligations, so without it there is nothing to check against",
                cwd.display()
            ));
        }
    };
    let (index, resolved, routed) =
        match std_lib::load(&cfg.standards, &cfg.stack.runtime, false).await {
            Ok(v) => v,
            Err(e) => return err(format!("standards corpus unavailable: {e}")),
        };
    let lists = std_lib::inject::checklists(&index, &routed);
    let obligations: usize = lists.iter().map(|c| c.items.len()).sum();
    let blocking = standards_blocking(&resolved, &routed);
    let checks = mechanical_checks(cwd);
    let failed: Vec<&Value> = checks
        .iter()
        .filter(|c| c.get("passed").and_then(Value::as_bool) == Some(false))
        .collect();

    let ok = blocking.is_empty() && failed.is_empty();
    ToolResponse {
        ok,
        data: json!({
            "framework": ASSESSABLE,
            "assessable": true,
            // ! No score_percent. Prose obligations are unscored by construction;
            // a number here would be the same fabrication in a new shape.
            "scored": false,
            "sha": resolved.sha,
            "bound_standards": routed.ids.len(),
            "obligations": obligations,
            "checklists": lists,
            "blocking": blocking,
            "mechanical_checks": checks,
            "mechanical_failed": failed.len(),
            "adjudication": "checklist items are prose obligations · the agent scores them against the codebase · mechanical_checks are the only items Pipeline decided itself",
        }),
        next_suggested: if ok {
            vec!["pipeline_standards.checklist".into()]
        } else {
            vec![
                "pipeline_standards.pin".into(),
                "pipeline_standards.checklist".into(),
            ]
        },
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "standards gate not clean · {} blocking · {} mechanical check(s) failed · \
                 {obligations} prose obligation(s) still require agent adjudication",
                blocking.len(),
                failed.len()
            ))
        },
    }
}

/// Corpus-level problems that invalidate any verdict built on it.
fn standards_blocking(resolved: &std_lib::Resolved, routed: &std_lib::RoutedSet) -> Vec<String> {
    let mut blocking: Vec<String> = Vec::new();
    if resolved.is_drifted() {
        blocking.push(format!(
            "standards drift · pinned {} but corpus is at {} · the obligations may have moved",
            resolved.pin.as_deref().unwrap_or("?"),
            resolved.short_sha()
        ));
    }
    if resolved.is_unpinned() {
        blocking.push("no standards.pin in pipeline.yaml · gates are unversioned".to_owned());
    }
    for d in &routed.decisions {
        blocking.push(format!(
            "unresolved route choice {:?} from {} · set standards.languages",
            d.options, d.from
        ));
    }
    for u in &routed.unknown_routes {
        blocking.push(format!("unknown route key · {u}"));
    }
    blocking
}

/// The handful of facts Pipeline genuinely decides by reading the worktree.
/// Each carries its own evidence so the caller can check the reasoning.
fn mechanical_checks(cwd: &std::path::Path) -> Vec<Value> {
    let mut checks: Vec<Value> = Vec::new();
    let dockerfile = cwd.join("Dockerfile");
    if let Ok(body) = std::fs::read_to_string(&dockerfile) {
        let runtime_user = dockerfile_runtime_user(&body);
        checks.push(json!({
            "name": "dockerfile_non_root",
            "passed": is_non_root(runtime_user.as_deref()),
            "evidence": runtime_user.map_or_else(
                || "shipping stage sets no USER · docker defaults to root".to_owned(),
                |u| format!("shipping stage runs as '{u}'")),
        }));
        let unpinned = unpinned_bases(&body);
        checks.push(json!({
            "name": "dockerfile_pinned_base",
            "passed": unpinned.is_empty(),
            "evidence": if unpinned.is_empty() { "every FROM is tag- or digest-pinned".to_owned() }
                        else { format!("unpinned base(s): {}", unpinned.join(" · ")) },
        }));
    }
    let env_file = cwd.join(".env");
    let env_ok = !env_file.exists() || ignored(".env", cwd);
    checks.push(json!({
        "name": "env_not_committable",
        "passed": env_ok,
        "evidence": if env_ok { "no .env, or .env is gitignored" } else { ".env exists and is not listed in .gitignore" },
    }));
    checks.push(json!({
        "name": "has_gitignore",
        "passed": cwd.join(".gitignore").exists(),
        "evidence": ".gitignore presence only",
    }));
    checks
}

/// Effective `USER` of the stage that actually ships — the last `FROM` block.
///
/// ! `body.contains("USER ")` was substantively wrong twice over: it matched
/// `# USER app` in a comment, and a multi-stage Dockerfile that builds as root
/// and runs as `app` was scored as FAILING because `USER root` appeared
/// anywhere. Only the final stage ships, and within it only the last `USER`
/// applies · a stage that inherits from a named earlier stage inherits its user.
fn dockerfile_runtime_user(body: &str) -> Option<String> {
    // (stage alias, effective user) in file order.
    let mut stages: Vec<(Option<String>, Option<String>)> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        let Some(directive) = toks.first() else {
            continue;
        };
        if directive.eq_ignore_ascii_case("FROM") {
            let base = toks.get(1).unwrap_or(&"").to_ascii_lowercase();
            let alias = toks
                .iter()
                .position(|x| x.eq_ignore_ascii_case("AS"))
                .and_then(|i| toks.get(i + 1))
                .map(|a| a.to_ascii_lowercase());
            // `FROM builder` carries builder's user forward · a new external
            // base resets to docker's default (root).
            let inherited = stages
                .iter()
                .find(|(a, _)| a.as_deref() == Some(base.as_str()))
                .and_then(|(_, u)| u.clone());
            stages.push((alias, inherited));
        } else if directive.eq_ignore_ascii_case("USER") {
            if let Some(last) = stages.last_mut() {
                last.1 = toks.get(1).map(|u| (*u).to_owned());
            }
        }
    }
    stages.last().and_then(|(_, u)| u.clone())
}

/// No `USER` at all means root · docker's default, ✗ "unknown".
fn is_non_root(user: Option<&str>) -> bool {
    let Some(u) = user else { return false };
    let lower = u.trim().to_ascii_lowercase();
    // `USER app:app` · only the user half decides.
    let name = lower.split(':').next().unwrap_or("");
    !name.is_empty() && name != "root" && name != "0"
}

/// External bases that are neither tag-pinned nor digest-pinned.
///
/// `body.contains(":latest")` also fired on `LABEL version=":latest"` and
/// missed the worse case entirely: `FROM debian` with no tag IS `:latest`.
/// A `FROM <alias>` referring to an earlier stage is internal, ✗ a base.
fn unpinned_bases(body: &str) -> Vec<String> {
    let mut aliases: Vec<String> = Vec::new();
    let mut unpinned: Vec<String> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        if !toks.first().is_some_and(|d| d.eq_ignore_ascii_case("FROM")) {
            continue;
        }
        let Some(base) = toks.get(1) else { continue };
        if let Some(i) = toks.iter().position(|x| x.eq_ignore_ascii_case("AS")) {
            if let Some(a) = toks.get(i + 1) {
                aliases.push(a.to_ascii_lowercase());
            }
        }
        let lower = base.to_ascii_lowercase();
        if aliases.contains(&lower) || lower.contains('@') {
            continue; // internal stage reference · or digest-pinned
        }
        // Strip the registry host so `registry:5000/img` is not read as a tag.
        let last = lower.rsplit('/').next().unwrap_or(&lower);
        let pinned = matches!(last.rsplit_once(':'), Some((_, tag)) if tag != "latest")
            && !lower.contains('$');
        if !pinned {
            unpinned.push((*base).to_owned());
        }
    }
    unpinned
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

#[cfg(test)]
mod compliance_tests {
    use super::{
        ASSESSABLE, REFUSED, dockerfile_runtime_user, is_non_root, refuse_framework, unpinned_bases,
    };

    #[test]
    fn a_regulatory_framework_is_refused_not_scored() {
        // ! The worst defect in the audit. `framework` was read, echoed, and
        // never branched on: a repo with a .gitignore, a pipeline.yaml and no
        // Dockerfile returned `score_percent: 100, framework: "hipaa"`, and the
        // agent relayed that to its user as HIPAA compliance.
        for (name, _) in REFUSED {
            let resp = refuse_framework(name, "reason");
            assert!(!resp.ok, "{name} refusal must never read as a pass");
            assert_eq!(resp.data["assessable"], false);
            assert_eq!(resp.data["scored"], false);
            assert!(
                resp.data.get("score_percent").is_none(),
                "{name} must carry no score at all · a number is the same lie in a new shape"
            );
            let e = resp.error.unwrap_or_default();
            assert!(
                e.contains(*name),
                "the refusal must name the framework: {e}"
            );
            assert!(e.contains("cannot be assessed"), "{e}");
        }
    }

    #[test]
    fn every_refused_framework_explains_what_pipeline_cannot_observe() {
        // A bare "unsupported" sends the agent looking for a flag to enable.
        // The reason has to say the controls are not in the worktree at all.
        for (name, reason) in REFUSED {
            assert!(reason.len() > 80, "{name} reason is too thin: {reason}");
        }
        let names: Vec<&str> = REFUSED.iter().map(|(n, _)| *n).collect();
        for expected in ["hipaa", "pci_dss", "gdpr", "iso27001", "owasp"] {
            assert!(names.contains(&expected), "{expected} must be refused");
        }
        assert!(
            !names.contains(&ASSESSABLE),
            "the one assessable framework must not also be refused"
        );
    }

    const MULTISTAGE: &str = "\
# build stage runs as root on purpose
FROM rust:1.85-slim AS builder
USER root
RUN cargo build --release

FROM debian:12-slim
RUN useradd -m app
USER app
CMD [\"/app\"]
";

    #[test]
    fn a_multistage_dockerfile_with_a_nonroot_runtime_passes() {
        // ! Regression: `body.contains(\"USER \") && !body.contains(\"USER root\")`
        // failed this file because the BUILD stage says `USER root` — but only
        // the shipping stage ships. The check was substantively backwards for
        // the single most common correct Dockerfile shape.
        assert_eq!(dockerfile_runtime_user(MULTISTAGE).as_deref(), Some("app"));
        assert!(is_non_root(dockerfile_runtime_user(MULTISTAGE).as_deref()));
    }

    #[test]
    fn a_commented_out_user_does_not_pass_the_non_root_check() {
        // `contains("USER ")` matched prose · a comment is not a directive.
        let df = "FROM debian:12\n# USER app  <- TODO, we still run as root\nCMD [\"/app\"]\n";
        assert_eq!(dockerfile_runtime_user(df), None);
        assert!(!is_non_root(dockerfile_runtime_user(df).as_deref()));
    }

    #[test]
    fn an_absent_user_directive_is_root_not_unknown() {
        // Docker's documented default · reporting "unknown" here would be a
        // hedge, not a determination.
        assert!(!is_non_root(None));
        assert!(!is_non_root(Some("root")));
        assert!(!is_non_root(Some("0")));
        assert!(is_non_root(Some("app:app")));
        assert!(is_non_root(Some("1000")));
    }

    #[test]
    fn a_stage_inheriting_from_an_earlier_stage_inherits_its_user() {
        let df = "FROM debian:12 AS base\nUSER app\n\nFROM base\nCMD [\"/x\"]\n";
        assert_eq!(dockerfile_runtime_user(df).as_deref(), Some("app"));
    }

    #[test]
    fn a_later_user_directive_in_the_same_stage_wins() {
        let df = "FROM debian:12\nUSER app\nUSER root\n";
        assert!(!is_non_root(dockerfile_runtime_user(df).as_deref()));
    }

    #[test]
    fn an_untagged_base_is_unpinned_because_it_resolves_to_latest() {
        // `!body.contains(":latest")` scored `FROM debian` as pinned · it is
        // exactly `debian:latest`, the case the check was written to catch.
        assert_eq!(unpinned_bases("FROM debian\n"), vec!["debian".to_owned()]);
        assert_eq!(
            unpinned_bases("FROM debian:latest\n"),
            vec!["debian:latest".to_owned()]
        );
        assert!(unpinned_bases(MULTISTAGE).is_empty());
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag_and_a_digest_is_pinned() {
        assert_eq!(
            unpinned_bases("FROM registry.local:5000/app\n"),
            vec!["registry.local:5000/app".to_owned()]
        );
        assert!(unpinned_bases("FROM registry.local:5000/app:1.2\n").is_empty());
        assert!(unpinned_bases("FROM debian@sha256:abc\n").is_empty());
    }

    #[test]
    fn an_internal_stage_reference_is_not_reported_as_an_unpinned_base() {
        let df = "FROM rust:1.85 AS builder\nFROM builder\nCMD [\"/x\"]\n";
        assert!(unpinned_bases(df).is_empty(), "{:?}", unpinned_bases(df));
    }
}

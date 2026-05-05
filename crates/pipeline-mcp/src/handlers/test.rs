//! `pipeline_test` handler · generate · run · ac_to_test.
//!
//! Day-5 ships:
//! - `run(filter?)` shells out to `cargo test`. Filter is passed through.
//! - `generate(target, kind)` writes a minimal Rust test scaffolding file.
//! - `ac_to_test(feature_id)` reads a feature blob from memory_kv and emits
//!   one `#[test]` per acceptance criterion. Stub bodies use `todo!()`.
//!
//! coverage · mutation_run · property_generate · validation_create ·
//! flake_detect return not_implemented (need cargo-llvm-cov, mutmut/Stryker,
//! proptest macros · MVP week 5).

#![allow(clippy::doc_markdown)]

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "run" => run_tests(&req.args).await,
        "generate" => generate(&req.args).await,
        "ac_to_test" => ac_to_test(&req.args, state).await,
        "coverage" => coverage(&req.args).await,
        "mutation_run" => mutation_run(&req.args).await,
        "property_generate" => property_generate(&req.args).await,
        "validation_create" => validation_create(&req.args).await,
        "flake_detect" => flake_detect(&req.args, state).await,
        other => err(format!("unknown action 'pipeline_test.{other}'")),
    }
}

async fn coverage(args: &Value) -> ToolResponse {
    let threshold = args.get("threshold").and_then(Value::as_f64).unwrap_or(0.0);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // cargo-llvm-cov · summary-only · workspace-scoped.
    let output = match Command::new("cargo")
        .args(["llvm-cov", "--workspace", "--summary-only", "--json"])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return err(format!(
                "cargo llvm-cov spawn: {e} · install with: cargo install cargo-llvm-cov"
            ));
        }
    };
    if !output.status.success() {
        return err(format!(
            "cargo llvm-cov failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let raw = String::from_utf8_lossy(&output.stdout).into_owned();
    // llvm-cov emits a JSON object with `data[0].totals.lines.percent` etc.
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let lines_pct = parsed
        .pointer("/data/0/totals/lines/percent")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let funcs_pct = parsed
        .pointer("/data/0/totals/functions/percent")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let regions_pct = parsed
        .pointer("/data/0/totals/regions/percent")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let ok = lines_pct >= threshold;
    ToolResponse {
        ok,
        data: json!({
            "threshold": threshold,
            "lines_percent": lines_pct,
            "functions_percent": funcs_pct,
            "regions_percent": regions_pct,
        }),
        next_suggested: if ok {
            vec!["pipeline_run.preflight".into()]
        } else {
            vec![
                "pipeline_test.generate".into(),
                "pipeline_test.ac_to_test".into(),
            ]
        },
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "coverage gate failed: lines={lines_pct:.1}% < threshold={threshold:.1}%"
            ))
        },
    }
}

async fn run_tests(args: &Value) -> ToolResponse {
    let filter = args.get("filter").and_then(Value::as_str);
    let suite = args
        .get("suite")
        .and_then(Value::as_str)
        .unwrap_or("workspace");

    let mut cmd_args: Vec<&str> = vec!["test"];
    if suite == "workspace" {
        cmd_args.push("--workspace");
    }
    cmd_args.push("--no-fail-fast");
    if let Some(f) = filter {
        cmd_args.push(f);
    }

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let output = match Command::new("cargo")
        .args(&cmd_args)
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("cargo test spawn: {e}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "exit_code": output.status.code().unwrap_or(-1),
            "filter": filter,
            "suite": suite,
            "stdout": truncate(&stdout, 16_000),
            "stderr": truncate(&stderr, 16_000),
        }),
        next_suggested: if ok {
            vec!["pipeline_run.stage(profile=full)".into()]
        } else {
            vec!["pipeline_memory.suggest_fix".into()]
        },
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "cargo test exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

#[allow(clippy::unused_async)] // signature parity with sibling handlers
async fn generate(args: &Value) -> ToolResponse {
    let target = match args.get("target").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'target' (file path under tests/ or a module name)".into()),
    };
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("unit");

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = resolve_test_path(&cwd, &target, kind);

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("create_dir_all: {e}"));
        }
    }
    if path.exists() {
        return err(format!(
            "refusing to overwrite existing file at {}",
            path.display()
        ));
    }

    let content = match kind {
        "integration" => integration_test_template(&target),
        _ => unit_test_template(&target),
    };
    if let Err(e) = std::fs::write(&path, &content) {
        return err(format!("write: {e}"));
    }
    ToolResponse {
        ok: true,
        data: json!({
            "path": path.display().to_string(),
            "kind": kind,
            "target": target,
        }),
        next_suggested: vec!["pipeline_test.run".into()],
        memory_refs: vec![],
        error: None,
    }
}

async fn ac_to_test(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let feature_id = match args
        .get("feature_id")
        .or_else(|| args.get("id"))
        .and_then(Value::as_str)
    {
        Some(s) => s.to_owned(),
        None => return err("missing 'feature_id'".into()),
    };
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    let blob = match mem.recall(&cfg.project, "feature", &feature_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return err(format!("feature '{feature_id}' not found")),
        Err(e) => return err(e.to_string()),
    };
    let feature: Value = match serde_json::from_str(&blob) {
        Ok(v) => v,
        Err(e) => return err(format!("corrupt feature blob: {e}")),
    };

    let name = feature
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("feature");
    let ac: Vec<String> = feature
        .get("ac")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if ac.is_empty() {
        return err(format!(
            "feature '{name}' has no acceptance criteria · call pipeline_plan.acceptance_define first"
        ));
    }

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let slug = slugify(name);
    let path = cwd.join("tests").join(format!("ac_{slug}.rs"));
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("create_dir_all: {e}"));
        }
    }
    let content = render_ac_tests(name, &ac);
    if let Err(e) = std::fs::write(&path, &content) {
        return err(format!("write: {e}"));
    }
    ToolResponse {
        ok: true,
        data: json!({
            "path": path.display().to_string(),
            "feature": name,
            "tests_emitted": ac.len(),
        }),
        next_suggested: vec![
            "pipeline_test.run".into(),
            "pipeline_run.stage(fast)".into(),
        ],
        memory_refs: vec![format!("feature:{feature_id}")],
        error: None,
    }
}

// ---------- helpers ----------

fn resolve_test_path(cwd: &std::path::Path, target: &str, kind: &str) -> PathBuf {
    // We already lowercased; clippy can't see through the binding.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    let is_rust_file = {
        let lower = target.to_ascii_lowercase();
        lower.ends_with(".rs")
    };
    if target.contains('/') || is_rust_file {
        return cwd.join(target);
    }
    let slug = slugify(target);
    match kind {
        "integration" => cwd.join("tests").join(format!("{slug}.rs")),
        _ => cwd.join("tests").join(format!("unit_{slug}.rs")),
    }
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned()
}

fn unit_test_template(target: &str) -> String {
    format!(
        "//! Tests scaffolded by pipeline_test.generate · target = {target}.\n\n#[test]\nfn smoke() {{\n    // TODO: replace with real assertion\n    assert!(true);\n}}\n"
    )
}

fn integration_test_template(target: &str) -> String {
    format!(
        "//! Integration tests scaffolded by pipeline_test.generate · target = {target}.\n//! Each test should boot the full stack via pipeline_docker.compose_up\n//! before running and tear down after.\n\n#[test]\nfn smoke_integration() {{\n    // TODO: real integration body\n    assert!(true);\n}}\n"
    )
}

fn render_ac_tests(feature_name: &str, criteria: &[String]) -> String {
    let mut out = format!(
        "//! Acceptance tests for feature `{feature_name}` · scaffolded by\n//! pipeline_test.ac_to_test. Replace `todo!()` with real assertions.\n\n"
    );
    for (i, criterion) in criteria.iter().enumerate() {
        let test_name = format!("ac_{:02}_{}", i + 1, slugify(criterion));
        let truncated: String = test_name.chars().take(80).collect();
        writeln!(out, "/// {criterion}").ok();
        writeln!(out, "#[test]").ok();
        writeln!(out, "fn {truncated}() {{").ok();
        writeln!(out, "    // criterion: {criterion}").ok();
        writeln!(out, "    todo!(\"implement assertion for: {criterion}\");").ok();
        writeln!(out, "}}\n").ok();
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        let mut t = s[..max].to_owned();
        write!(t, "\n... [truncated · {} more bytes]", s.len() - max).ok();
        t
    }
}

async fn mutation_run(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (program, cmd_args): (&str, Vec<&str>) = match stack {
        "rust" => ("cargo", vec!["mutants"]),
        "python" | "python-uv" => ("mutmut", vec!["run"]),
        "node" | "ts" | "typescript" => ("npx", vec!["stryker", "run"]),
        other => return err(format!("unsupported stack '{other}'")),
    };
    let output = match Command::new(program)
        .args(&cmd_args)
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("{program}: {e} · is it installed?")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "stack": stack,
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": truncate(&String::from_utf8_lossy(&output.stdout), 16_000),
            "stderr": truncate(&String::from_utf8_lossy(&output.stderr), 16_000),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "mutation_run({stack}) exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

#[allow(clippy::unused_async)]
async fn property_generate(args: &Value) -> ToolResponse {
    let target = match args.get("target").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'target' (function name)".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd
        .join("tests")
        .join(format!("prop_{}.rs", slugify(&target)));
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("mkdir: {e}"));
        }
    }
    let body = format!(
        "//! Property tests scaffolded by pipeline_test.property_generate · target = {target}.\n//! Add `proptest = \"1\"` to [dev-dependencies] in Cargo.toml.\n\nuse proptest::prelude::*;\n\nproptest! {{\n    #[test]\n    fn {fn_name}(input in any::<u64>()) {{\n        // TODO: invariant for {target}(input)\n        let _ = input;\n    }}\n}}\n",
        fn_name = slugify(&target),
    );
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"path": path.display().to_string(), "target": target}))
}

#[allow(clippy::unused_async)]
async fn validation_create(args: &Value) -> ToolResponse {
    let spec = args
        .get("spec")
        .and_then(Value::as_str)
        .unwrap_or("contract");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("validation");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join(format!("{}.sh", slugify(spec)));
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    let body = format!(
        "#!/usr/bin/env bash\n# Validation script scaffolded by pipeline_test.validation_create · spec={spec}\nset -euo pipefail\n\necho \"validating {spec}...\"\n# TODO: actual checks · curl + jq + diff against spec/{spec}.json\nexit 0\n"
    );
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    // chmod +x · best-effort.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&path, perms);
        }
    }
    ToolResponse::ok(json!({"spec": spec, "path": path.display().to_string()}))
}

async fn flake_detect(args: &Value, state: Arc<ServerState>) -> ToolResponse {
    let _ = args;
    // Heuristic: stage that has both pass and fail in run history is flaky.
    let cfg = match load_config_in_cwd() {
        Ok(c) => c,
        Err(e) => return err(format!("config: {e}")),
    };
    let mem = match ensure_memory(&state).await {
        Ok(m) => m,
        Err(e) => return err(format!("memory: {e}")),
    };
    let runs = mem.run_history(&cfg.project, 200).await.unwrap_or_default();
    let mut by_stage: std::collections::BTreeMap<String, (u32, u32)> =
        std::collections::BTreeMap::new();
    for r in &runs {
        let entry = by_stage.entry(r.stage.clone()).or_insert((0, 0));
        if r.status == "pass" {
            entry.0 += 1;
        } else if r.status == "fail" {
            entry.1 += 1;
        }
    }
    let flaky: Vec<Value> = by_stage
        .iter()
        .filter(|(_, (p, f))| *p > 0 && *f > 0)
        .map(|(stage, (p, f))| {
            let pct = (f64::from(*f) / f64::from(*p + *f)) * 100.0;
            json!({"stage": stage, "pass": p, "fail": f, "fail_rate": pct})
        })
        .collect();
    ToolResponse::ok(json!({
        "stages_seen": by_stage.len(),
        "flaky": flaky,
        "tip": if flaky.is_empty() { "no flaky stages detected" } else { "investigate · pin seeds / time-of-day / env vars" },
    }))
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
mod tests {
    use super::*;

    #[test]
    fn slugify_strips_punctuation() {
        assert_eq!(slugify("Hello, World!"), "hello__world");
        assert_eq!(slugify("rate-limit"), "rate_limit");
        assert_eq!(slugify("  trim  "), "trim");
    }

    #[test]
    fn render_ac_tests_emits_one_per_criterion() {
        let out = render_ac_tests("auth", &["valid login".into(), "rejects bad token".into()]);
        assert!(out.contains("fn ac_01_valid_login"));
        assert!(out.contains("fn ac_02_rejects_bad_token"));
        assert!(out.matches("#[test]").count() == 2);
    }

    #[test]
    fn resolve_test_path_uses_tests_dir_for_simple_target() {
        let cwd = std::path::Path::new("/tmp/proj");
        let p = resolve_test_path(cwd, "metering", "unit");
        assert!(p.ends_with("tests/unit_metering.rs"));
        let p2 = resolve_test_path(cwd, "metering", "integration");
        assert!(p2.ends_with("tests/metering.rs"));
    }
}

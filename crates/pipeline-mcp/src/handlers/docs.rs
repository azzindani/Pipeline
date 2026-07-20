//! `pipeline_docs` handler · generate · changelog · diagram · spec · publish.
//!
//! ! Three actions here previously fabricated, and the fabrications compound
//! downstream:
//! - `spec_generate` echoed `source` without opening it and emitted a `/health`
//!   endpoint nobody wrote. Specs feed Stage-3 contract testing, so a fabricated
//!   spec becomes a fabricated GATE.
//! - `diagram` wrote a hardcoded picture of Pipeline's own architecture for
//!   every project on earth, byte-identical, with `ok:true`.
//! - `publish` accepted `target`, published nothing, and swallowed a missing
//!   mkdocs into success.
//!
//! Each is now derived from the project or refused by name. Scope is narrow on
//! purpose: `spec_generate` parses Rust + axum route registrations, `diagram`
//! derives a Rust workspace crate graph, `publish` builds locally and ✗ pushes.
//! Everything outside that scope is an explicit refusal, ✗ a plausible skeleton.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "generate" => generate(&req.args).await,
        "changelog" => changelog(&req.args).await,
        "update_from_code" => update_from_code(&req.args).await,
        "diagram" => diagram(&req.args).await,
        "publish" => publish(&req.args).await,
        "spec_generate" => spec_generate(&req.args).await,
        other => err(format!("unknown action 'pipeline_docs.{other}'")),
    }
}

async fn generate(args: &Value) -> ToolResponse {
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("readme");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (path, content) = match kind {
        "readme" => (cwd.join("README.md"), README_TEMPLATE.to_owned()),
        "runbook" => (cwd.join("docs/RUNBOOK.md"), RUNBOOK_TEMPLATE.to_owned()),
        "onboarding" => (
            cwd.join("docs/ONBOARDING.md"),
            ONBOARDING_TEMPLATE.to_owned(),
        ),
        "api" => (cwd.join("docs/API.md"), API_TEMPLATE.to_owned()),
        "arch" => (cwd.join("docs/ARCHITECTURE.md"), ARCH_TEMPLATE.to_owned()),
        other => {
            return err(format!(
                "unknown kind '{other}' · readme|runbook|onboarding|api|arch"
            ));
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Err(e) = tokio::fs::write(&path, content).await {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"kind": kind, "path": path.display().to_string()}))
}

async fn changelog(args: &Value) -> ToolResponse {
    let from = args.get("from").and_then(Value::as_str);
    let to = args.get("to").and_then(Value::as_str).unwrap_or("HEAD");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let range = match from {
        Some(f) => format!("{f}..{to}"),
        None => to.to_owned(),
    };
    let output = match Command::new("git")
        .args(["log", "--oneline", "--no-decorate", &range])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git log: {e}")),
    };
    let log = String::from_utf8_lossy(&output.stdout).into_owned();
    let lines: Vec<&str> = log.lines().collect();
    ToolResponse {
        ok: output.status.success(),
        data: json!({
            "range": range,
            "commit_count": lines.len(),
            "log": log,
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if output.status.success() {
            None
        } else {
            Some("git log failed".into())
        },
    }
}

const README_TEMPLATE: &str = "# Project\n\n## What\n\nOne-paragraph description.\n\n## Run\n\n```\npipeline run fast\n```\n\n## Develop\n\n```\npipeline dev\n```\n";
const RUNBOOK_TEMPLATE: &str =
    "# Runbook\n\n## Common operations\n\n## Common failures\n\n## Escalation\n";
const ONBOARDING_TEMPLATE: &str = "# Onboarding\n\n## Day 1\n\n## Day 7\n\n## Day 30\n";
const API_TEMPLATE: &str = "# API\n\n## Endpoints\n\n## Errors\n\n## Auth\n";
const ARCH_TEMPLATE: &str =
    "# Architecture\n\n## Components\n\n## Data flow\n\n## Trust boundaries\n";

async fn update_from_code(_args: &Value) -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Best-effort: regenerate cargo doc · the agent then merges into READMEs.
    let output = match Command::new("cargo")
        .args(["doc", "--workspace", "--no-deps"])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("cargo doc: {e}")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "command": "cargo doc --workspace --no-deps",
            "exit_code": output.status.code().unwrap_or(-1),
            "out_dir": cwd.join("target/doc").display().to_string(),
        }),
        next_suggested: vec!["pipeline_docs.publish".into()],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some("cargo doc failed".into())
        },
    }
}

// ---------- diagram ----------

/// Kinds that cannot be derived from source, and why.
///
/// ! Each of these was previously emitted as a hardcoded diagram of PIPELINE's
/// own architecture — byte-identical for every project on earth, with `ok:true`
/// and a plausible path. A refusal costs the agent one call; a fabricated
/// architecture diagram gets pasted into a design review.
const UNDERIVABLE_KINDS: &[(&str, &str)] = &[
    (
        "sequence",
        "a sequence diagram describes runtime message order · that is a trace, not a manifest \
         fact · nothing in the source tree records it",
    ),
    (
        "er",
        "an ER diagram describes domain entities and cardinality · derive it from a live schema \
         via pipeline_data, ✗ from crate manifests",
    ),
    (
        "c4",
        "C4 context/container levels encode people, external systems, and deployment intent · \
         none of that is recoverable from code",
    ),
];

/// A workspace member and the sibling members it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CrateNode {
    name: String,
    deps: Vec<String>,
}

#[allow(clippy::unused_async)] // signature parity with sibling handlers
async fn diagram(args: &Value) -> ToolResponse {
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("arch");
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some((_, reason)) = UNDERIVABLE_KINDS.iter().find(|(k, _)| *k == kind) {
        return err(format!(
            "kind '{kind}' cannot be derived from this project · {reason} · \
             derivable kinds: arch | crates (Rust workspace crate dependency graph)"
        ));
    }
    if !matches!(kind, "arch" | "crates") {
        return err(format!(
            "unknown kind '{kind}' · derivable: arch | crates · \
             refused as underivable: sequence | er | c4"
        ));
    }
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let nodes = match crate_graph(&cwd) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    let path = match args.get("out").and_then(Value::as_str) {
        Some(o) => cwd.join(o),
        None => cwd.join("docs/diagrams/crates.mmd"),
    };
    if path.exists() && !overwrite {
        return err(format!(
            "refusing to overwrite {} · pass overwrite=true to re-derive",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("mkdir: {e}"));
        }
    }
    let edges: usize = nodes.iter().map(|n| n.deps.len()).sum();
    if let Err(e) = std::fs::write(&path, render_crate_graph(&nodes)) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({
        "kind": kind,
        "path": path.display().to_string(),
        "format": "mermaid",
        "derived_from": "Cargo.toml manifests of every workspace member",
        "crates": nodes.iter().map(|n| &n.name).collect::<Vec<_>>(),
        "edges": edges,
    }))
}

/// Build the crate dependency graph from the workspace's own manifests.
///
/// Line-oriented parsing · this crate carries no TOML dependency, and the two
/// tables that matter (`[workspace] members`, `[*dependencies]`) are trivially
/// recognisable. A member whose manifest is unreadable is dropped from the
/// graph rather than guessed at.
fn crate_graph(root: &Path) -> Result<Vec<CrateNode>, String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).map_err(|e| {
        format!(
            "no readable Cargo.toml at {} ({e}) · the crate graph is derived from a Rust \
             workspace · for other stacks the diagram is not implemented, ✗ invented",
            root.display()
        )
    })?;
    let members = expand_members(root, &parse_workspace_members(&manifest));
    if members.is_empty() {
        return Err(format!(
            "no [workspace] members in {} · nothing to graph · a single-crate project has no \
             internal architecture to derive",
            root.join("Cargo.toml").display()
        ));
    }
    let mut raw: Vec<(String, Vec<String>)> = Vec::new();
    for rel in &members {
        let Ok(m) = std::fs::read_to_string(root.join(rel).join("Cargo.toml")) else {
            continue;
        };
        if let Some(name) = parse_package_name(&m) {
            raw.push((name, parse_dependency_names(&m)));
        }
    }
    let names: Vec<String> = raw.iter().map(|(n, _)| n.clone()).collect();
    // Only intra-workspace edges · a graph including serde and tokio describes
    // the registry, not this project's architecture.
    Ok(raw
        .into_iter()
        .map(|(name, deps)| CrateNode {
            deps: deps
                .into_iter()
                .filter(|d| *d != name && names.contains(d))
                .collect(),
            name,
        })
        .collect())
}

/// `crates/*` globs are expanded against the filesystem · cargo accepts them,
/// so silently dropping them would silently drop most of the graph.
fn expand_members(root: &Path, patterns: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in patterns {
        let Some(prefix) = p.strip_suffix("/*") else {
            out.push(p.clone());
            continue;
        };
        let Ok(rd) = std::fs::read_dir(root.join(prefix)) else {
            continue;
        };
        for entry in rd.flatten() {
            if entry.path().join("Cargo.toml").is_file() {
                out.push(format!("{prefix}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Quoted strings on a line · `members = ["a", "b"]` → `["a", "b"]`.
fn quoted_values(line: &str) -> Vec<String> {
    line.split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_workspace_members(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_workspace = false;
    let mut in_members = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_workspace = t == "[workspace]";
            in_members = false;
            continue;
        }
        if !in_workspace {
            continue;
        }
        if in_members || (t.starts_with("members") && t.contains('[')) {
            in_members = true;
            out.extend(quoted_values(t));
            if t.contains(']') {
                in_members = false;
            }
        }
    }
    out
}

fn parse_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if in_package {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == "name" {
                    return quoted_values(v).into_iter().next();
                }
            }
        }
    }
    None
}

/// Dependency keys across every `*dependencies` table, including
/// `[dependencies.foo]` sub-tables and `[target.'cfg(...)'.dependencies]`.
fn parse_dependency_names(manifest: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(header) = t.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            let h = header.trim();
            // `[dependencies.foo]` names one dependency and opens no key list.
            if let Some((section, name)) = h.rsplit_once('.') {
                if section.ends_with("dependencies") {
                    out.push(name.trim().to_owned());
                    in_deps = false;
                    continue;
                }
            }
            in_deps = h.ends_with("dependencies");
            continue;
        }
        if !in_deps {
            continue;
        }
        // `serde = { ... }` and `serde.workspace = true` both key on `serde`.
        let key = t.split(['=', '.']).next().unwrap_or("").trim();
        if !key.is_empty() {
            out.push(key.to_owned());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn mermaid_id(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn render_crate_graph(nodes: &[CrateNode]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    writeln!(
        out,
        "%% Crate dependency graph · derived by pipeline_docs.diagram from Cargo.toml manifests."
    )
    .ok();
    writeln!(
        out,
        "%% {} crate(s) · edges are intra-workspace path/workspace dependencies only.",
        nodes.len()
    )
    .ok();
    writeln!(out, "graph TD").ok();
    for n in nodes {
        writeln!(out, "    {}[\"{}\"]", mermaid_id(&n.name), n.name).ok();
    }
    for n in nodes {
        for d in &n.deps {
            writeln!(out, "    {} --> {}", mermaid_id(&n.name), mermaid_id(d)).ok();
        }
    }
    out
}

// ---------- publish ----------

/// Build the docs site locally · ! ✗ pushes anywhere.
///
/// Publishing is outward-facing, so there is no code path here that writes to a
/// remote. `target` selects the builder, `local` is the only one implemented,
/// and every other value is refused by name rather than silently treated as
/// local. ! A missing mkdocs is an ERROR: the old version swallowed the spawn
/// failure into `ok:true` with a "note", so an agent gating a release on
/// `publish` proceeded having built nothing.
async fn publish(args: &Value) -> ToolResponse {
    let target = args
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("local");
    let out = args.get("out").and_then(Value::as_str).unwrap_or("site");
    if target != "local" {
        return err(format!(
            "target '{target}' is not implemented · publish builds the site locally and reports \
             its path · it ✗ pushes to any remote, branch, or bucket. Supported: local"
        ));
    }
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let config = ["mkdocs.yml", "mkdocs.yaml"]
        .into_iter()
        .map(|c| cwd.join(c))
        .find(|p| p.is_file());
    let Some(config) = config else {
        return err(format!(
            "no mkdocs.yml in {} · publish builds an mkdocs site and nothing else is \
             implemented · ✗ reporting success for a site that was never built",
            cwd.display()
        ));
    };
    let site_dir = cwd.join(out);
    let output = Command::new("mkdocs")
        .args(["build", "-d", &site_dir.display().to_string()])
        .current_dir(&cwd)
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => ToolResponse::ok(json!({
            "target": target,
            "config": config.display().to_string(),
            "site_dir": site_dir.display().to_string(),
            "files": count_files(&site_dir),
            "published": false,
            "note": "built locally · nothing was pushed · deploy the directory yourself",
        })),
        Ok(o) => err(format!(
            "mkdocs build failed (exit {}): {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        // ! spawn failure → error. Nothing was built; saying otherwise is the
        // exact defect this action was flagged for.
        Err(e) => err(format!(
            "mkdocs did not run: {e} · install with `pip install mkdocs` · \
             nothing was built and nothing was published"
        )),
    }
}

/// Evidence the build produced something · a "successful" build into an empty
/// directory is a result the caller needs to see.
fn count_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            if e.path().is_dir() {
                count_files(&e.path())
            } else {
                1
            }
        })
        .sum()
}

// ---------- spec generation ----------

/// Methods recognised inside an axum `.route(path, ...)` registration.
const HTTP_METHODS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

/// One derived endpoint · every field comes from the source, none is invented.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    path: String,
    method: String,
    handler: String,
    file: String,
}

/// What a scan of the source tree actually found — including what it could not
/// read. ! The unreadable and unrecognised lists are part of the result, not a
/// log line: a spec that silently omits half the API reads as a complete one.
#[derive(Debug, Default)]
struct SpecScan {
    routes: Vec<Route>,
    files_scanned: usize,
    unreadable: Vec<String>,
    unrecognised: Vec<String>,
}

#[allow(clippy::unused_async)] // signature parity with sibling handlers
async fn spec_generate(args: &Value) -> ToolResponse {
    let Some(source) = args.get("source").and_then(Value::as_str) else {
        return err(
            "missing 'source' (Rust file or directory to parse) · spec_generate derives the \
             spec from real route registrations · there is no default to invent one from"
                .into(),
        );
    };
    let format = args
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("openapi");
    if format != "openapi" {
        return err(format!(
            "format '{format}' cannot be derived from a route table · a route registration \
             carries paths and methods, ✗ message payloads or field types, so jsonschema | \
             protobuf | asyncapi would be a skeleton with your project's name on it. \
             Supported: openapi"
        ));
    }
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let root = cwd.join(source);
    let scan = match scan_sources(&root) {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    if scan.routes.is_empty() {
        return err(no_routes_message(&root, &scan));
    }
    let path = match args.get("out").and_then(Value::as_str) {
        Some(o) => cwd.join(o),
        None => cwd.join("specs/openapi.yaml"),
    };
    if path.exists() && !overwrite {
        return err(format!(
            "refusing to overwrite {} · pass overwrite=true to re-derive",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("mkdir: {e}"));
        }
    }
    let title = spec_title(&root);
    if let Err(e) = std::fs::write(&path, render_openapi(&title, &scan.routes)) {
        return err(format!("write: {e}"));
    }
    spec_response(format, source, &path, &scan)
}

fn spec_title(root: &Path) -> String {
    root.file_stem()
        .map_or_else(|| "api".to_owned(), |s| s.to_string_lossy().into_owned())
}

/// ! Refuse rather than emit an empty spec. An `openapi:` document with no
/// paths still validates, still writes, still feeds Stage-3 contract testing —
/// and gates on nothing.
fn no_routes_message(root: &Path, scan: &SpecScan) -> String {
    format!(
        "no routes derived from {} · {} file(s) scanned, {} unreadable, {} unrecognised \
         registration(s) · spec_generate parses Rust + axum `.route(\"/p\", get(handler))` \
         only · ✗ emitting an empty or invented spec, which would feed contract testing a \
         gate nobody wrote",
        root.display(),
        scan.files_scanned,
        scan.unreadable.len(),
        scan.unrecognised.len()
    )
}

fn spec_response(format: &str, source: &str, path: &Path, scan: &SpecScan) -> ToolResponse {
    let endpoints: Vec<Value> = scan
        .routes
        .iter()
        .map(|r| json!({"method": r.method, "path": r.path, "handler": r.handler, "file": r.file}))
        .collect();
    // Partial derivation is still a result · it is reported alongside, ✗ instead.
    let partial = !scan.unreadable.is_empty() || !scan.unrecognised.is_empty();
    ToolResponse {
        ok: true,
        data: json!({
            "format": format,
            "source": source,
            "path": path.display().to_string(),
            "files_scanned": scan.files_scanned,
            "routes": scan.routes.len(),
            "endpoints": endpoints,
            "complete": !partial,
            "unreadable_files": scan.unreadable,
            "unrecognised_route_calls": scan.unrecognised,
            "derived_from": "axum .route() registrations",
            "not_derived": "request bodies · response schemas · status codes · auth · a route table does not carry them, so they are absent rather than guessed",
        }),
        next_suggested: vec!["pipeline_e2e.run".into()],
        memory_refs: vec![],
        error: None,
    }
}

/// Read every Rust file under `root` and extract its route registrations.
fn scan_sources(root: &Path) -> Result<SpecScan, String> {
    if !root.exists() {
        return Err(format!("source '{}' does not exist", root.display()));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        if root.extension().and_then(|e| e.to_str()) != Some("rs") {
            return Err(format!(
                "'{}' is not a .rs file · spec_generate parses Rust sources · for another \
                 language the derivation is not implemented, ✗ approximated",
                root.display()
            ));
        }
        files.push(root.to_path_buf());
    } else {
        collect_rs_files(root, &mut files, 0);
    }
    let mut scan = SpecScan::default();
    files.sort();
    for f in &files {
        let rel = f.strip_prefix(root).unwrap_or(f).display().to_string();
        let Ok(text) = std::fs::read_to_string(f) else {
            // ! Reported, ✗ skipped. A file we could not read may hold the half
            // of the API the spec is now missing.
            scan.unreadable.push(rel);
            continue;
        };
        scan.files_scanned += 1;
        let (routes, unrecognised) = extract_routes(&text);
        for (path, method, handler) in routes {
            scan.routes.push(Route {
                path,
                method,
                handler,
                file: rel.clone(),
            });
        }
        scan.unrecognised
            .extend(unrecognised.into_iter().map(|u| format!("{rel}: {u}")));
    }
    Ok(scan)
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    if depth > 12 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            collect_rs_files(&p, out, depth + 1);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// Extract `(path, method, handler)` triples from axum `.route(...)` calls.
///
/// Returns the routes it understood and, separately, the registrations it did
/// not. ! A `.route()` call whose path is a constant or a computed expression
/// is a real endpoint this parser cannot name — dropping it silently is how a
/// three-endpoint spec passes for the whole API.
fn extract_routes(src: &str) -> (Vec<(String, String, String)>, Vec<String>) {
    let mut routes = Vec::new();
    let mut unrecognised = Vec::new();
    let mut idx = 0usize;
    while let Some(found) = src[idx..].find(".route(") {
        let start = idx + found + ".route(".len();
        idx = start;
        let Some(inner) = balanced_args(src, start) else {
            unrecognised.push(snippet(&src[start..]));
            continue;
        };
        idx = start + inner.len();
        let Some((path, tail)) = leading_string_literal(inner) else {
            unrecognised.push(snippet(inner));
            continue;
        };
        let methods = methods_in(tail);
        if methods.is_empty() {
            unrecognised.push(snippet(inner));
            continue;
        }
        for (method, handler) in methods {
            routes.push((path.clone(), method, handler));
        }
    }
    (routes, unrecognised)
}

fn snippet(s: &str) -> String {
    let one_line: String = s
        .chars()
        .take(80)
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    format!(".route({}…", one_line.trim())
}

/// Slice between an opening paren (already consumed at `start`) and its match ·
/// string literals are skipped so a `)` inside a path does not close the call.
fn balanced_args(src: &str, start: usize) -> Option<&str> {
    let mut depth = 1usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in src[start..].char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[start..start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn leading_string_literal(s: &str) -> Option<(String, &str)> {
    let t = s.trim_start();
    let rest = t.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some((rest[..end].to_owned(), &rest[end + 1..]))
}

/// `get(h).post(h2)` → `[("get", "h"), ("post", "h2")]`, in source order.
fn methods_in(s: &str) -> Vec<(String, String)> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    for (i, _) in s.char_indices() {
        // A preceding identifier char means this is `budget(`, ✗ `get(`.
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            continue;
        }
        for m in HTTP_METHODS {
            if s[i..].starts_with(m) && s[i + m.len()..].starts_with('(') {
                let handler = balanced_args(s, i + m.len() + 1)
                    .map_or_else(|| "<unparsed>".to_owned(), handler_name);
                out.push(((m).to_owned(), handler));
            }
        }
    }
    out
}

/// Name the handler when it is a plain path, otherwise say it is inline ·
/// ✗ inventing an `operationId` for a closure.
fn handler_name(inner: &str) -> String {
    let t = inner.trim();
    let ident: String = t
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    if ident.is_empty() || ident.len() != t.len() {
        return "<inline>".to_owned();
    }
    ident.rsplit("::").next().unwrap_or(&ident).to_owned()
}

/// axum path params → OpenAPI template form · `:id` and `*rest` become `{id}`.
fn openapi_path(axum_path: &str) -> (String, Vec<String>) {
    let mut params = Vec::new();
    let converted: Vec<String> = axum_path
        .split('/')
        .map(|seg| {
            if let Some(name) = seg.strip_prefix(':').or_else(|| seg.strip_prefix('*')) {
                params.push(name.to_owned());
                return format!("{{{name}}}");
            }
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                params.push(name.trim_start_matches('*').to_owned());
            }
            seg.to_owned()
        })
        .collect();
    (converted.join("/"), params)
}

fn render_openapi(title: &str, routes: &[Route]) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    let mut by_path: BTreeMap<String, (Vec<String>, BTreeMap<String, String>)> = BTreeMap::new();
    for r in routes {
        let (path, params) = openapi_path(&r.path);
        let entry = by_path
            .entry(path)
            .or_insert_with(|| (params, BTreeMap::new()));
        entry.1.insert(r.method.clone(), r.handler.clone());
    }

    let mut out = String::new();
    writeln!(out, "openapi: 3.1.0").ok();
    writeln!(out, "info:").ok();
    writeln!(out, "  title: {title}").ok();
    writeln!(out, "  version: 0.0.0").ok();
    writeln!(out, "  description: >-").ok();
    writeln!(
        out,
        "    Derived by pipeline_docs.spec_generate from axum route registrations."
    )
    .ok();
    writeln!(
        out,
        "    Paths, methods and handler names are real. Request bodies, response"
    )
    .ok();
    writeln!(
        out,
        "    schemas and status codes are ABSENT, not empty: a route table does not"
    )
    .ok();
    writeln!(
        out,
        "    carry them. Fill them in before using this as a contract-test gate."
    )
    .ok();
    writeln!(out, "paths:").ok();
    for (path, (params, methods)) in &by_path {
        writeln!(out, "  {path}:").ok();
        write_path_params(&mut out, params);
        for (method, handler) in methods {
            writeln!(out, "    {method}:").ok();
            writeln!(out, "      operationId: {handler}").ok();
            writeln!(out, "      responses:").ok();
            writeln!(out, "        default:").ok();
            writeln!(
                out,
                "          description: not derivable from the route table · document this"
            )
            .ok();
        }
    }
    out
}

fn write_path_params(out: &mut String, params: &[String]) {
    use std::fmt::Write as _;
    if params.is_empty() {
        return;
    }
    writeln!(out, "    parameters:").ok();
    for p in params {
        writeln!(out, "      - name: {p}").ok();
        writeln!(out, "        in: path").ok();
        writeln!(out, "        required: true").ok();
        writeln!(out, "        schema:").ok();
        writeln!(out, "          type: string").ok();
    }
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
mod spec_tests {
    use super::{
        Route, SpecScan, extract_routes, handler_name, no_routes_message, openapi_path,
        render_openapi, scan_sources, spec_response,
    };

    const AXUM_SRC: &str = r#"
use axum::{Router, routing::{get, post}};

pub fn app() -> Router {
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route("/users/:id", get(handlers::get_user))
        .route("/health", get(|| async { "ok" }))
}
"#;

    #[test]
    fn a_spec_is_derived_from_source_not_invented() {
        // ! Regression: `source` was read, echoed into the response, and never
        // opened. The handler emitted a `/health` GET returning '200': ok for
        // every project — and CLAUDE.md routes specs into Stage-3 contract
        // testing, so the invented endpoint became an invented gate.
        let (routes, _) = extract_routes(AXUM_SRC);
        let pairs: Vec<(&str, &str)> = routes
            .iter()
            .map(|(p, m, _)| (p.as_str(), m.as_str()))
            .collect();
        assert!(pairs.contains(&("/users", "get")), "{pairs:?}");
        assert!(pairs.contains(&("/users", "post")), "{pairs:?}");
        assert!(pairs.contains(&("/users/:id", "get")), "{pairs:?}");

        // …and a source with no routes yields no spec at all, ✗ a skeleton.
        let (none, _) = extract_routes("fn main() { println!(\"hi\"); }");
        assert!(none.is_empty());

        // The rendered document contains only what the source declared.
        let derived: Vec<Route> = routes
            .into_iter()
            .map(|(path, method, handler)| Route {
                path,
                method,
                handler,
                file: "app.rs".into(),
            })
            .collect();
        let yaml = render_openapi("app", &derived);
        assert!(yaml.contains("  /users:"), "{yaml}");
        assert!(yaml.contains("operationId: list_users"), "{yaml}");
        // The old fabricated pair · a real /health here comes from the source,
        // and it must NOT carry the invented 200 response.
        assert!(!yaml.contains("'200'"), "invented status code: {yaml}");
        assert!(
            !yaml.contains("description: ok"),
            "invented response: {yaml}"
        );
    }

    #[test]
    fn an_unparseable_source_is_reported_not_skipped_silently() {
        // ! A `.route()` whose path is a constant is a real endpoint this parser
        // cannot name. Dropping it quietly makes a partial spec read as the
        // whole API — the same failure mode as inventing one.
        let src = r#"
        Router::new()
            .route("/known", get(h))
            .route(ADMIN_PATH, get(admin))
            .route("/no_method", something_else())
        "#;
        let (routes, unrecognised) = extract_routes(src);
        assert_eq!(routes.len(), 1, "{routes:?}");
        assert_eq!(unrecognised.len(), 2, "{unrecognised:?}");
        assert!(unrecognised.iter().any(|u| u.contains("ADMIN_PATH")));

        // …and the response surfaces them, flagging the spec as incomplete.
        let scan = SpecScan {
            routes: vec![Route {
                path: "/known".into(),
                method: "get".into(),
                handler: "h".into(),
                file: "a.rs".into(),
            }],
            files_scanned: 1,
            unreadable: vec!["b.rs".into()],
            unrecognised,
        };
        let resp = spec_response("openapi", "src", std::path::Path::new("/tmp/o.yaml"), &scan);
        assert_eq!(resp.data["complete"], false);
        assert_eq!(resp.data["unreadable_files"][0], "b.rs");
        assert_eq!(
            resp.data["unrecognised_route_calls"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_source_with_no_routes_is_refused_rather_than_given_an_empty_spec() {
        // An `openapi:` document with zero paths still validates and still
        // writes · it would gate on nothing.
        let scan = SpecScan {
            files_scanned: 12,
            ..SpecScan::default()
        };
        let msg = no_routes_message(std::path::Path::new("/proj/src"), &scan);
        assert!(msg.contains("no routes derived"), "{msg}");
        assert!(msg.contains("12 file(s) scanned"), "{msg}");
        assert!(
            msg.contains("✗ emitting an empty or invented spec"),
            "{msg}"
        );
    }

    #[test]
    fn a_non_rust_source_is_refused_rather_than_approximated() {
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("app.py");
        std::fs::write(&py, "@app.get('/x')\ndef x(): ...\n").unwrap();
        let e = scan_sources(&py).unwrap_err();
        assert!(e.contains("not a .rs file"), "{e}");
        assert!(e.contains("✗ approximated"), "{e}");
    }

    #[test]
    fn a_missing_source_is_an_error_not_an_empty_scan() {
        let e = scan_sources(std::path::Path::new("/nope/absent")).unwrap_err();
        assert!(e.contains("does not exist"), "{e}");
    }

    #[test]
    fn a_directory_scan_reads_every_rust_file_and_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("api")).unwrap();
        std::fs::write(dir.path().join("api/routes.rs"), AXUM_SRC).unwrap();
        std::fs::write(dir.path().join("lib.rs"), "// nothing here\n").unwrap();
        let scan = scan_sources(dir.path()).unwrap();
        assert_eq!(scan.files_scanned, 2);
        assert_eq!(scan.routes.len(), 4);
        assert!(scan.routes.iter().all(|r| r.file.contains("routes.rs")));
    }

    #[test]
    fn an_identifier_ending_in_a_method_name_is_not_a_method() {
        // `budget(` must not register as `get(` · the substring trap.
        let (routes, _) = extract_routes(".route(\"/x\", budget(h))");
        assert!(routes.is_empty(), "{routes:?}");
    }

    #[test]
    fn path_parameters_are_converted_and_declared() {
        let (p, params) = openapi_path("/users/:id/posts/:post_id");
        assert_eq!(p, "/users/{id}/posts/{post_id}");
        assert_eq!(params, vec!["id".to_owned(), "post_id".to_owned()]);
        let yaml = render_openapi(
            "api",
            &[Route {
                path: "/users/:id".into(),
                method: "get".into(),
                handler: "get_user".into(),
                file: "a.rs".into(),
            }],
        );
        assert!(yaml.contains("/users/{id}:"), "{yaml}");
        assert!(yaml.contains("- name: id"), "{yaml}");
    }

    #[test]
    fn an_inline_closure_handler_is_labelled_not_given_an_invented_name() {
        assert_eq!(handler_name("list_users"), "list_users");
        assert_eq!(handler_name("handlers::get_user"), "get_user");
        assert_eq!(handler_name("|| async { \"ok\" }"), "<inline>");
    }
}

#[cfg(test)]
mod diagram_tests {
    use super::{
        CrateNode, UNDERIVABLE_KINDS, crate_graph, parse_dependency_names, parse_package_name,
        parse_workspace_members, render_crate_graph,
    };

    const ROOT: &str = "[workspace]\nresolver = \"2\"\nmembers = [\n    \"crates/core\",\n    \"crates/api\",\n]\n\n[workspace.package]\nversion = \"0.1.0\"\n";

    fn workspace() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), ROOT).unwrap();
        for (name, manifest) in [
            (
                "core",
                "[package]\nname = \"core\"\n\n[dependencies]\nserde = \"1\"\n",
            ),
            (
                "api",
                "[package]\nname = \"api\"\n\n[dependencies]\ncore = { path = \"../core\" }\ntokio = { workspace = true }\n",
            ),
        ] {
            let dir = d.path().join("crates").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        }
        d
    }

    #[test]
    fn the_graph_is_derived_from_this_workspace_not_from_pipelines_own() {
        // ! Regression: every project received the identical hardcoded mermaid
        // `A[Agent] -->|MCP| B[Pipeline]`, with ok:true and a plausible path.
        let d = workspace();
        let nodes = crate_graph(d.path()).unwrap();
        assert_eq!(nodes.len(), 2);
        let api = nodes.iter().find(|n| n.name == "api").unwrap();
        assert_eq!(api.deps, vec!["core".to_owned()]);
        let mermaid = render_crate_graph(&nodes);
        assert!(mermaid.contains("api --> core"), "{mermaid}");
        assert!(!mermaid.contains("Pipeline"), "{mermaid}");
        assert!(!mermaid.contains("Agent"), "{mermaid}");
        // External crates describe the registry, not this project's structure.
        assert!(!mermaid.contains("serde"), "{mermaid}");
        assert!(!mermaid.contains("tokio"), "{mermaid}");
    }

    #[test]
    fn a_non_workspace_project_is_refused_rather_than_given_a_stock_diagram() {
        let d = tempfile::tempdir().unwrap();
        let e = crate_graph(d.path()).unwrap_err();
        assert!(e.contains("no readable Cargo.toml"), "{e}");
        assert!(e.contains("✗ invented"), "{e}");

        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"solo\"\n").unwrap();
        let e = crate_graph(d.path()).unwrap_err();
        assert!(e.contains("no [workspace] members"), "{e}");
    }

    #[test]
    fn kinds_that_cannot_be_derived_each_say_why() {
        let names: Vec<&str> = UNDERIVABLE_KINDS.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, vec!["sequence", "er", "c4"]);
        for (k, reason) in UNDERIVABLE_KINDS {
            assert!(reason.len() > 60, "{k} reason is too thin: {reason}");
        }
    }

    #[test]
    fn glob_members_are_expanded_rather_than_dropped() {
        // `members = ["crates/*"]` is valid cargo · dropping it would drop the
        // entire graph while still reporting ok:true.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        for n in ["alpha", "beta"] {
            let dir = d.path().join("crates").join(n);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("Cargo.toml"),
                format!("[package]\nname = \"{n}\"\n"),
            )
            .unwrap();
        }
        let nodes = crate_graph(d.path()).unwrap();
        let mut names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn manifest_parsing_handles_the_shapes_cargo_actually_accepts() {
        assert_eq!(
            parse_workspace_members("[workspace]\nmembers = [\"a\", \"b\"]\n"),
            vec!["a".to_owned(), "b".to_owned()]
        );
        // A members list must not leak keys from the next table.
        assert_eq!(
            parse_workspace_members(ROOT),
            vec!["crates/core".to_owned(), "crates/api".to_owned()]
        );
        assert_eq!(
            parse_package_name("[package]\nname = \"x\"\nversion = \"1\"\n").as_deref(),
            Some("x")
        );
        let deps = parse_dependency_names(
            "[dependencies]\nserde = \"1\"\ntokio.workspace = true\n\n\
             [dev-dependencies]\ntempfile = \"3\"\n\n\
             [target.'cfg(unix)'.dependencies]\nnix = \"0\"\n\n\
             [dependencies.bollard]\nversion = \"0.16\"\n",
        );
        for expected in ["serde", "tokio", "tempfile", "nix", "bollard"] {
            assert!(
                deps.contains(&expected.to_owned()),
                "{expected} missing: {deps:?}"
            );
        }
        assert!(!deps.contains(&"dependencies".to_owned()), "{deps:?}");
    }

    #[test]
    fn crate_names_with_dashes_produce_valid_mermaid_ids() {
        let nodes = vec![
            CrateNode {
                name: "pipeline-core".into(),
                deps: vec![],
            },
            CrateNode {
                name: "pipeline-mcp".into(),
                deps: vec!["pipeline-core".into()],
            },
        ];
        let m = render_crate_graph(&nodes);
        assert!(m.contains("pipeline_mcp --> pipeline_core"), "{m}");
        // …while the label keeps the real name.
        assert!(m.contains("[\"pipeline-core\"]"), "{m}");
    }
}

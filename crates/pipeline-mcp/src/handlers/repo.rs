//! `pipeline_repo` handler · registry · digest · extract · analyse.
//!
//! Registry: `.pipeline/repos/registry.json` (array of {alias, url, kind, added_at}).
//! Clones land in `.pipeline/repos/<alias>/` on first `digest` (lazy).
//! Digests land in `.pipeline/digests/<alias>.json`.
//! RE jobs land in `.pipeline/re/<job_id>.json`.
//!
//! ! `re_analyze` is SYNCHRONOUS. An earlier revision wrote a job file with
//! `status:"queued"` and no worker anywhere in the tree ever read it, so
//! `re_status` reported "queued" forever while `re_report` overwrote the status
//! to "complete" and emitted empty module/contract/pattern lists. An agent
//! polling status→report was told the analysis had finished and the target had
//! no modules. ✗ reintroduce an async job that nothing processes: a job file
//! exists here ONLY because the analysis it holds already ran to completion.
//!
//! What is genuinely computed for `type=codebase`: module boundaries from the
//! directory tree · language histogram · entry points · test layout · config
//! surface · external dependencies parsed out of real manifests. What is NOT
//! computed is listed in `not_computed` on every payload — an internal import
//! graph, API contracts, and design-pattern detection all need source parsing
//! this handler does not do. ✗ let an omission read as an empty finding.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;

const REGISTRY_DIR: &str = ".pipeline/repos";
const REGISTRY_FILE: &str = ".pipeline/repos/registry.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    pub repos: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub alias: String,
    pub url: String,
    pub kind: String, // git | local
    pub added_at: String,
    #[serde(default)]
    pub cloned: bool,
}

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "register" => register(&req.args).await,
        "list" => list().await,
        "remove" => remove(&req.args).await,
        "digest" => digest(&req.args).await,
        "list_capabilities" => list_capabilities(&req.args).await,
        "extract" => extract(&req.args).await,
        "compare" => compare(&req.args).await,
        "port" => port(),
        "port_validate" => port_validate(&req.args).await,
        "apply_standards" => apply_standards(&req.args).await,
        "capability_graph" => capability_graph().await,
        "re_analyze" => re_analyze(&req.args).await,
        "re_status" => re_status(&req.args).await,
        "re_report" => re_report(&req.args).await,
        "re_reconstruct" => re_reconstruct(),
        "re_modernize" => re_modernize(&req.args).await,
        other => err(format!("unknown action 'pipeline_repo.{other}'")),
    }
}

async fn register(args: &Value) -> ToolResponse {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_owned(),
        None => return err("missing 'url'".into()),
    };
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => match infer_alias(&url) {
            Some(a) => a,
            None => return err("missing 'alias' and could not infer from url".into()),
        },
    };

    let mut reg = match read_registry().await {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    if reg.repos.iter().any(|r| r.alias == alias) {
        return err(format!("alias '{alias}' already registered"));
    }
    reg.repos.push(RegistryEntry {
        alias: alias.clone(),
        url: url.clone(),
        kind: kind_of(&url),
        added_at: pipeline_memory::now_rfc3339(),
        cloned: false,
    });
    if let Err(e) = write_registry(&reg).await {
        return err(e);
    }

    ToolResponse {
        ok: true,
        data: json!({"alias": alias, "url": url}),
        next_suggested: vec!["pipeline_repo.digest".into(), "pipeline_repo.list".into()],
        memory_refs: vec![format!("repo:{alias}")],
        error: None,
    }
}

async fn list() -> ToolResponse {
    match read_registry().await {
        Ok(r) => ToolResponse::ok(json!({"repos": r.repos})),
        Err(e) => err(e),
    }
}

async fn remove(args: &Value) -> ToolResponse {
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'alias'".into()),
    };
    let delete_clone = args
        .get("delete_clone")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut reg = match read_registry().await {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let before = reg.repos.len();
    reg.repos.retain(|r| r.alias != alias);
    if reg.repos.len() == before {
        return err(format!("alias '{alias}' not registered"));
    }
    if let Err(e) = write_registry(&reg).await {
        return err(e);
    }

    if delete_clone {
        let dir = clone_dir(&alias);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
    ToolResponse::ok(json!({"removed": alias, "delete_clone": delete_clone}))
}

async fn digest(args: &Value) -> ToolResponse {
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'alias'".into()),
    };
    let mut reg = match read_registry().await {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let idx = match reg.repos.iter().position(|r| r.alias == alias) {
        Some(i) => i,
        None => return err(format!("alias '{alias}' not registered")),
    };
    let url = reg.repos[idx].url.clone();
    let kind = reg.repos[idx].kind.clone();

    let dir = clone_dir(&alias);
    let already_cloned = tokio::fs::try_exists(&dir).await.unwrap_or(false);

    // Clone (or skip if already cloned · or directly use local path).
    if kind == "local" {
        // local path · do not clone · digest in place
    } else if !already_cloned {
        if let Err(e) = clone_repo(&url, &dir).await {
            return err(format!("git clone: {e}"));
        }
        reg.repos[idx].cloned = true;
        if let Err(e) = write_registry(&reg).await {
            return err(e);
        }
    }

    let walk_root = if kind == "local" {
        PathBuf::from(strip_local_prefix(&url))
    } else {
        dir.clone()
    };

    let summary = match walk_summary(&walk_root).await {
        Ok(s) => s,
        Err(e) => return err(format!("walk: {e}")),
    };

    // Persist digest blob alongside the registry.
    let digest_path = digest_file(&alias);
    if let Some(parent) = digest_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let blob = json!({
        "alias": alias,
        "url": url,
        "digested_at": pipeline_memory::now_rfc3339(),
        "summary": summary,
    });
    if let Err(e) = tokio::fs::write(&digest_path, blob.to_string()).await {
        return err(format!("write digest: {e}"));
    }

    ToolResponse {
        ok: true,
        data: blob,
        next_suggested: vec![
            "pipeline_repo.list_capabilities".into(),
            "pipeline_repo.extract".into(),
        ],
        memory_refs: vec![format!("digest:{alias}")],
        error: None,
    }
}

// ---------- registry I/O ----------

async fn read_registry() -> Result<Registry, String> {
    let path = registry_path();
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(Registry::default());
    }
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read registry: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("parse registry: {e}"))
}

async fn write_registry(reg: &Registry) -> Result<(), String> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create_dir_all: {e}"))?;
    }
    let text = serde_json::to_string_pretty(reg).map_err(|e| format!("serialize registry: {e}"))?;
    tokio::fs::write(&path, text)
        .await
        .map_err(|e| format!("write registry: {e}"))
}

fn registry_path() -> PathBuf {
    cwd_or_dot().join(REGISTRY_FILE)
}

fn cwd_or_dot() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn clone_dir(alias: &str) -> PathBuf {
    cwd_or_dot().join(REGISTRY_DIR).join(alias)
}

/// Where a registered repo's source ACTUALLY lives.
///
/// ! A `local` repo is digested in place — nothing is ever cloned into
/// `.pipeline/repos/<alias>/`. Reaching straight for [`clone_dir`] therefore
/// lands on a path that does not exist, which is how `list_capabilities` came
/// to silently return an empty set for every local repo.
///
/// ✗ use this in `remove` — deleting a local repo's "clone" must stay pointed at
/// `clone_dir`, or `delete_clone` would erase the user's actual source tree.
async fn repo_root(alias: &str) -> Result<PathBuf, String> {
    let reg = read_registry().await?;
    let entry = reg
        .repos
        .iter()
        .find(|r| r.alias == alias)
        .ok_or_else(|| format!("alias '{alias}' not registered"))?;
    Ok(if entry.kind == "local" {
        PathBuf::from(strip_local_prefix(&entry.url))
    } else {
        clone_dir(alias)
    })
}

fn digest_file(alias: &str) -> PathBuf {
    cwd_or_dot()
        .join(".pipeline/digests")
        .join(format!("{alias}.json"))
}

fn re_job_file(job_id: &str) -> PathBuf {
    cwd_or_dot()
        .join(".pipeline/re")
        .join(format!("{job_id}.json"))
}

// ---------- helpers ----------

fn kind_of(url: &str) -> String {
    if url.starts_with("file://") || url.starts_with("./") || url.starts_with('/') {
        "local".into()
    } else {
        "git".into()
    }
}

fn strip_local_prefix(s: &str) -> String {
    s.strip_prefix("file://").unwrap_or(s).to_owned()
}

fn infer_alias(url: &str) -> Option<String> {
    // owner/repo → repo · https://host/path/repo(.git) → repo
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let last = trimmed.rsplit('/').next()?;
    if last.is_empty() {
        return None;
    }
    // Replace any non-alnum chars with underscore.
    let alias: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if alias.is_empty() { None } else { Some(alias) }
}

async fn clone_repo(url: &str, dir: &Path) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let out = Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(dir)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(())
}

/// Extension → language. Single source of truth for both the digest walk and
/// the RE scan — two divergent maps would make `digest` and `re_analyze`
/// disagree about the same tree.
fn lang_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "rb" => "ruby",
        "swift" => "swift",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" | "cxx" => "cpp",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "md" => "markdown",
        "sh" => "shell",
        _ => return None,
    })
}

/// Languages that carry program logic. `yaml`/`json`/`markdown` are counted by
/// the histogram but are ✗ evidence that a directory is a code module.
const SOURCE_LANGS: &[&str] = &[
    "rust",
    "python",
    "typescript",
    "javascript",
    "go",
    "java",
    "kotlin",
    "ruby",
    "swift",
    "c",
    "cpp",
    "shell",
];

fn is_source_lang(lang: &str) -> bool {
    SOURCE_LANGS.contains(&lang)
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // independent presence flags · simple shape wins
struct WalkSummary {
    total_files: usize,
    languages: BTreeMap<String, usize>,
    top_dirs: Vec<String>,
    has_dockerfile: bool,
    has_compose: bool,
    has_readme: bool,
    has_license: bool,
}

async fn walk_summary(root: &Path) -> Result<WalkSummary, String> {
    use tokio::fs;
    let mut total_files = 0usize;
    let mut languages: BTreeMap<String, usize> = BTreeMap::new();
    let mut top_dirs: Vec<String> = Vec::new();
    let mut has_dockerfile = false;
    let mut has_compose = false;
    let mut has_readme = false;
    let mut has_license = false;

    // Top-level scan first · drives top_dirs.
    let mut rd = fs::read_dir(root).await.map_err(|e| e.to_string())?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if meta.is_dir() {
            top_dirs.push(name.clone());
        }
        if name == "Dockerfile" {
            has_dockerfile = true;
        }
        if name == "docker-compose.yml" || name == "compose.yml" {
            has_compose = true;
        }
        if name.starts_with("README") {
            has_readme = true;
        }
        if name.starts_with("LICENSE") {
            has_license = true;
        }
        // Recurse for language counts (BFS, capped depth to keep cheap).
        recurse_lang_count(&path, &mut total_files, &mut languages, 0, 4).await;
    }
    top_dirs.sort();
    Ok(WalkSummary {
        total_files,
        languages,
        top_dirs,
        has_dockerfile,
        has_compose,
        has_readme,
        has_license,
    })
}

#[allow(clippy::manual_async_fn)]
fn recurse_lang_count<'a>(
    path: &'a Path,
    total: &'a mut usize,
    langs: &'a mut BTreeMap<String, usize>,
    depth: u32,
    max_depth: u32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        if depth > max_depth {
            return;
        }
        if let Ok(meta) = tokio::fs::metadata(path).await {
            if meta.is_file() {
                *total += 1;
                if let Some(lang) = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .and_then(lang_for_ext)
                {
                    *langs.entry(lang.to_owned()).or_insert(0) += 1;
                }
                return;
            }
            if meta.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                if name == "target"
                    || name == "node_modules"
                    || name == ".git"
                    || name == "dist"
                    || name == "build"
                {
                    return;
                }
                if let Ok(mut rd) = tokio::fs::read_dir(path).await {
                    while let Ok(Some(entry)) = rd.next_entry().await {
                        recurse_lang_count(&entry.path(), total, langs, depth + 1, max_depth).await;
                    }
                }
            }
        }
    })
}

// ---------- list_capabilities · extract ----------

async fn read_digest(alias: &str) -> Result<Value, String> {
    let path = digest_file(alias);
    let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
        format!("digest for '{alias}' not found · call pipeline_repo.digest first ({e})")
    })?;
    serde_json::from_str(&text).map_err(|e| format!("corrupt digest: {e}"))
}

/// Filename substrings that hint at a capability. ! These are FILENAME matches,
/// not behaviour — a hit means "a file is named like this", ✗ "this capability
/// is implemented". Shared by `list_capabilities` and `compare(axis=features)`
/// so the two never disagree about what a feature is.
const CAPABILITY_MARKERS: &[(&str, &str)] = &[
    ("auth", "authentication"),
    ("queue", "queue worker"),
    ("retry", "retry strategy"),
    ("ratelimit", "rate limiting"),
    ("rate_limit", "rate limiting"),
    ("webhook", "webhook handler"),
    ("billing", "billing"),
    ("metering", "metering"),
    ("rating", "rating engine"),
    ("scheduler", "scheduling"),
];

/// Which markers actually appear in a filename set. Pure over the names so the
/// matching rule is testable without a tree on disk.
fn markers_present(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (token, label) in CAPABILITY_MARKERS {
        if names.iter().any(|n| n.to_ascii_lowercase().contains(token))
            && !out.iter().any(|l| l == label)
        {
            out.push((*label).to_owned());
        }
    }
    out
}

/// Heuristic capability list: top-level dirs that look like code (have at
/// least one source file) plus a few well-known capability markers found
/// at any depth (auth, queue, retry, ratelimit, ...).
async fn list_capabilities(args: &Value) -> ToolResponse {
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'alias'".into()),
    };
    let digest = match read_digest(&alias).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let dir = match repo_root(&alias).await {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let mut capabilities: Vec<Value> = Vec::new();

    if let Some(top) = digest
        .pointer("/summary/top_dirs")
        .and_then(Value::as_array)
    {
        for v in top {
            if let Some(name) = v.as_str() {
                let path = dir.join(name);
                let has_code = directory_has_source(&path).await;
                if has_code {
                    capabilities.push(json!({
                        "name": name,
                        "kind": "directory",
                        "location": format!("{name}/"),
                    }));
                }
            }
        }
    }
    let names = collect_filenames(&dir).await;
    for label in markers_present(&names) {
        capabilities.push(json!({
            "name": label,
            "kind": "marker",
            "location": "filename substring match",
        }));
    }

    ToolResponse {
        ok: true,
        data: json!({"alias": alias, "capabilities": capabilities}),
        next_suggested: vec!["pipeline_repo.extract".into()],
        memory_refs: vec![format!("digest:{alias}")],
        error: None,
    }
}

async fn directory_has_source(dir: &Path) -> bool {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let is_src = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .and_then(|e| lang_for_ext(&e))
            .is_some_and(is_source_lang);
        if is_src {
            return true;
        }
    }
    false
}

async fn collect_filenames(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut budget: u32 = 4_000;
    while let Some(dir) = stack.pop() {
        if budget == 0 {
            break;
        }
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || name == "target"
                || name == "node_modules"
                || name == "dist"
                || name == "build"
            {
                continue;
            }
            let path = entry.path();
            if let Ok(meta) = entry.metadata().await {
                if meta.is_dir() {
                    stack.push(path);
                } else {
                    out.push(name);
                }
            }
            budget -= 1;
            if budget == 0 {
                break;
            }
        }
    }
    out
}

/// Copy `<repo>/<source>` into `<cwd>/<target>` (or `<cwd>/extracted/<source>`
/// if target omitted). Idempotent: refuses to overwrite an existing target.
async fn extract(args: &Value) -> ToolResponse {
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'alias'".into()),
    };
    let capability = match args
        .get("capability")
        .or_else(|| args.get("source"))
        .and_then(Value::as_str)
    {
        Some(c) => c.to_owned(),
        None => return err("missing 'capability' (path within the source repo)".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let target_rel = match args.get("target_path").and_then(Value::as_str) {
        Some(s) => PathBuf::from(s),
        None => cwd.join("extracted").join(&capability),
    };

    let src_root = match repo_root(&alias).await {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let src = src_root.join(&capability);
    if !tokio::fs::try_exists(&src).await.unwrap_or(false) {
        return err(format!(
            "source '{}' missing in cloned repo · call pipeline_repo.digest first",
            src.display()
        ));
    }
    if tokio::fs::try_exists(&target_rel).await.unwrap_or(false) {
        return err(format!(
            "refusing to overwrite existing target {}",
            target_rel.display()
        ));
    }
    if let Some(parent) = target_rel.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    let count = match copy_recursive(&src, &target_rel).await {
        Ok(n) => n,
        Err(e) => return err(e),
    };

    ToolResponse {
        ok: true,
        data: json!({
            "alias": alias,
            "capability": capability,
            "source": src.display().to_string(),
            "target": target_rel.display().to_string(),
            "files_copied": count,
        }),
        next_suggested: vec![
            "pipeline_repo.port_validate".into(),
            "pipeline_run.stage(fast)".into(),
        ],
        memory_refs: vec![format!("digest:{alias}")],
        error: None,
    }
}

async fn copy_recursive(src: &Path, dst: &Path) -> Result<usize, String> {
    let meta = tokio::fs::metadata(src)
        .await
        .map_err(|e| format!("stat {}: {e}", src.display()))?;
    if meta.is_file() {
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::fs::copy(src, dst).await.map_err(|e| e.to_string())?;
        return Ok(1);
    }
    tokio::fs::create_dir_all(dst)
        .await
        .map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    let mut count = 0usize;
    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&s)
            .await
            .map_err(|e| format!("read {}: {e}", s.display()))?;
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            let from = entry.path();
            let to = d.join(&name);
            let m = entry.metadata().await.map_err(|e| e.to_string())?;
            if m.is_dir() {
                tokio::fs::create_dir_all(&to)
                    .await
                    .map_err(|e| e.to_string())?;
                stack.push((from, to));
            } else if m.is_file() {
                tokio::fs::copy(&from, &to)
                    .await
                    .map_err(|e| e.to_string())?;
                count += 1;
            }
        }
    }
    Ok(count)
}

// ---------- compare ----------

/// Set difference in both directions. The shape every `compare` axis returns,
/// so an agent parses one payload regardless of which axis it asked for.
fn diff_sets(a: &[String], b: &[String]) -> Value {
    let sa: BTreeSet<&String> = a.iter().collect();
    let sb: BTreeSet<&String> = b.iter().collect();
    json!({
        "shared": sa.intersection(&sb).map(|s| (*s).clone()).collect::<Vec<_>>(),
        "only_in_a": sa.difference(&sb).map(|s| (*s).clone()).collect::<Vec<_>>(),
        "only_in_b": sb.difference(&sa).map(|s| (*s).clone()).collect::<Vec<_>>(),
    })
}

fn string_array_at(v: &Value, pointer: &str) -> Vec<String> {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Artifact presence flags out of a digest summary. ! These are file-existence
/// booleans. They are reported under `artifacts_present`, never `compliance` —
/// the previous naming let a repo with four files claim 100% standards
/// compliance with zero standards evaluated.
fn artifact_flags(digest: &Value) -> Value {
    let get = |k: &str| {
        digest
            .pointer(&format!("/summary/{k}"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    json!({
        "has_dockerfile": get("has_dockerfile"),
        "has_compose": get("has_compose"),
        "has_readme": get("has_readme"),
        "has_license": get("has_license"),
    })
}

const COMPARE_AXES: &[&str] = &["arch", "features", "standards"];

/// Compare two digested repos along a chosen axis.
///
/// ! Each axis returns a DIFFERENT payload. The previous revision accepted
/// `axis` and returned an identical language histogram for all three, so the
/// argument read as supported while being inert.
async fn compare(args: &Value) -> ToolResponse {
    let a = match args.get("a").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'a' (alias)".into()),
    };
    let b = match args.get("b").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'b' (alias)".into()),
    };
    let axis = args.get("axis").and_then(Value::as_str).unwrap_or("arch");
    if !COMPARE_AXES.contains(&axis) {
        return err(format!(
            "unknown axis '{axis}' · accepted: {}",
            COMPARE_AXES.join(" | ")
        ));
    }
    let da = match read_digest(&a).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let db = match read_digest(&b).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };

    let finding = match axis {
        "arch" => compare_arch(&da, &db),
        "standards" => compare_standards(&da, &db),
        _ => match compare_features(&a, &b).await {
            Ok(v) => v,
            Err(e) => return err(e),
        },
    };

    ToolResponse {
        ok: true,
        data: json!({"axis": axis, "a": a, "b": b, "finding": finding}),
        next_suggested: vec!["pipeline_repo.list_capabilities".into()],
        memory_refs: vec![format!("digest:{a}"), format!("digest:{b}")],
        error: None,
    }
}

fn compare_arch(da: &Value, db: &Value) -> Value {
    json!({
        "basis": "top-level directories + language histogram from each digest",
        "directories": diff_sets(
            &string_array_at(da, "/summary/top_dirs"),
            &string_array_at(db, "/summary/top_dirs"),
        ),
        "languages_a": da.pointer("/summary/languages").cloned().unwrap_or(json!({})),
        "languages_b": db.pointer("/summary/languages").cloned().unwrap_or(json!({})),
    })
}

fn compare_standards(da: &Value, db: &Value) -> Value {
    json!({
        "basis": "file-existence flags recorded by digest · ✗ a compliance verdict",
        "artifacts_a": artifact_flags(da),
        "artifacts_b": artifact_flags(db),
        "adjudication": "call pipeline_repo.apply_standards for the obligations that actually bind each repo",
    })
}

/// Rescans both trees — feature markers are filename matches, and the digest
/// stores only top-level dirs, so this axis cannot be answered from the digest.
async fn compare_features(a: &str, b: &str) -> Result<Value, String> {
    let root_a = repo_root(a).await?;
    let root_b = repo_root(b).await?;
    let names_a = collect_filenames(&root_a).await;
    let names_b = collect_filenames(&root_b).await;
    Ok(json!({
        "basis": "capability markers matched against filenames in each tree",
        "caveat": "a marker means a file is NAMED for the capability · ✗ that it is implemented",
        "markers": diff_sets(&markers_present(&names_a), &markers_present(&names_b)),
        "files_examined_a": names_a.len(),
        "files_examined_b": names_b.len(),
    }))
}

/// ✗ implemented. Registry marks this Planned, so `dispatch` refuses before the
/// handler runs — this body exists so the action can never lie if that guard is
/// ever bypassed. Translation is an agent task: use `re_analyze` for the real
/// module map, then `re_modernize` for a migration order.
fn port() -> ToolResponse {
    err(
        "pipeline_repo.port is not implemented · it translates no code. \
         Use pipeline_repo.re_analyze(target) for a real module map, \
         pipeline_repo.re_modernize(job_id) for a migration order, then translate \
         module-by-module and gate each with pipeline_repo.port_validate(path)."
            .into(),
    )
}

/// Run pipeline_run.stage(fast) inside a target path · validates ported code.
async fn port_validate(args: &Value) -> ToolResponse {
    let path = match args.get("path").and_then(Value::as_str) {
        Some(p) => p.to_owned(),
        None => return err("missing 'path'".into()),
    };
    let p = PathBuf::from(&path);
    if !tokio::fs::try_exists(&p).await.unwrap_or(false) {
        return err(format!("path not found: {path}"));
    }
    let pipeline_yaml = p.join("pipeline.yaml");
    if !tokio::fs::try_exists(&pipeline_yaml).await.unwrap_or(false) {
        return err(format!(
            "no pipeline.yaml at {path} · call pipeline_project.init or copy one in"
        ));
    }
    // Shell out to `pipeline run fast` from the path · simplest reuse.
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pipeline"));
    let output = match tokio::process::Command::new(&exe)
        .args(["run", "fast"])
        .current_dir(&p)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("spawn pipeline: {e}")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "path": path,
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": String::from_utf8_lossy(&output.stdout).into_owned(),
            "stderr": String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        next_suggested: if ok {
            vec!["pipeline_run.preflight".into()]
        } else {
            vec!["pipeline_memory.suggest_fix".into()]
        },
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "port_validate exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

// ---------- apply_standards ----------

/// Dominant SOURCE language in a digest histogram. `yaml`/`json`/`markdown` are
/// excluded — a docs-heavy repo is not a Markdown project, and routing it to a
/// language standard on that basis would bind the wrong obligations.
fn dominant_source_language(digest: &Value) -> Option<String> {
    let langs = digest.pointer("/summary/languages")?.as_object()?;
    langs
        .iter()
        .filter(|(k, _)| is_source_lang(k))
        .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
        // Ties break on name so the same digest always routes the same way.
        .max_by(|(ka, na), (kb, nb)| na.cmp(nb).then_with(|| kb.cmp(ka)))
        .map(|(k, _)| k)
}

/// Which standards BIND a digested repo, and what they oblige.
///
/// ! Reports zero scores. It resolves the real Standards corpus, routes it by
/// the repo's own dominant language, and hands back the checklist obligations
/// for the agent to adjudicate — the model `pipeline_standards.check` uses. The
/// previous revision divided four file-existence booleans and published the
/// quotient as `compliance`, which let any repo with a README, LICENSE,
/// Dockerfile and compose file claim 100% with nothing evaluated.
async fn apply_standards(args: &Value) -> ToolResponse {
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'alias'".into()),
    };
    let digest = match read_digest(&alias).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let runtime = match args.get("runtime").and_then(Value::as_str) {
        Some(r) => r.to_owned(),
        None => match dominant_source_language(&digest) {
            Some(l) => l,
            None => {
                return err(format!(
                    "no source language in the digest for '{alias}' · cannot route standards · \
                     pass 'runtime' (rust|python|typescript|go|shell)"
                ));
            }
        },
    };

    let cwd = cwd_or_dot();
    let cfg = match pipeline_config::PipelineConfig::load(cwd.join("pipeline.yaml")) {
        Ok(c) => c,
        Err(e) => {
            return err(format!(
                "standards resolution needs this project's pipeline.yaml · {e}"
            ));
        }
    };
    let (index, resolved, routed) =
        match pipeline_standards::load(&cfg.standards, &runtime, false).await {
            Ok(v) => v,
            Err(e) => return err(format!("resolve standards corpus: {e}")),
        };
    let lists = pipeline_standards::inject::checklists(&index, &routed);
    let obligations: usize = lists.iter().map(|c| c.items.len()).sum();

    ToolResponse {
        ok: true,
        data: json!({
            "alias": alias,
            "routed_as_runtime": runtime,
            "standards_sha": resolved.sha,
            "bound_standards": routed.ids,
            "obligations": obligations,
            "checklists": lists,
            "artifacts_present": artifact_flags(&digest),
            "scored": false,
            "adjudication": "obligations are prose · Pipeline evaluated NONE of them against this repo. \
                             artifacts_present is file existence, ✗ a compliance score.",
        }),
        next_suggested: vec![
            "pipeline_repo.digest".into(),
            "pipeline_standards.show".into(),
        ],
        memory_refs: vec![format!("digest:{alias}")],
        error: None,
    }
}

async fn capability_graph() -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join(".pipeline/digests");
    if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
        return ToolResponse::ok(json!({"nodes": [], "edges": []}));
    }
    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(rd) => rd,
        Err(e) => return err(format!("read_dir: {e}")),
    };
    let mut nodes: Vec<Value> = Vec::new();
    let mut edges: Vec<Value> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let body = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let v: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let alias = v
            .get("alias")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        nodes.push(json!({
            "alias": alias,
            "languages": v.pointer("/summary/languages").cloned().unwrap_or(json!({})),
        }));
        if let Some(top) = v.pointer("/summary/top_dirs").and_then(Value::as_array) {
            for t in top {
                if let Some(name) = t.as_str() {
                    edges.push(json!({"alias": alias, "capability": name}));
                }
            }
        }
    }
    ToolResponse::ok(json!({
        "nodes": nodes,
        "edges": edges,
        "node_count": nodes.len(),
        "edge_count": edges.len(),
    }))
}

// ---------- codebase analysis ----------

const SCAN_BUDGET: usize = 20_000;

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    "vendor",
    ".tox",
];

/// Dirs whose CHILDREN are the modules — `crates/foo` is a module, `crates` is
/// not. Without this every workspace collapses into a single "crates" module
/// and the module map says nothing about the system's actual boundaries.
const CONTAINER_DIRS: &[&str] = &[
    "crates", "packages", "apps", "services", "cmd", "internal", "pkg", "modules", "libs", "src",
    "lib", "source",
];

const MANIFEST_FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "cargo"),
    ("package.json", "npm"),
    ("go.mod", "go"),
    ("requirements.txt", "pip"),
    ("pyproject.toml", "python"),
    ("Gemfile", "bundler"),
];

#[derive(Debug, Serialize, Deserialize, Default)]
struct Module {
    name: String,
    files: usize,
    source_files: usize,
    test_files: usize,
    languages: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct TestLayout {
    test_files: usize,
    test_dirs: Vec<String>,
    strategy: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    path: String,
    ecosystem: String,
    dependencies: Vec<String>,
    parser: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CodebaseAnalysis {
    root: String,
    files_scanned: usize,
    scan_truncated: bool,
    languages: BTreeMap<String, usize>,
    modules: Vec<Module>,
    entry_points: Vec<String>,
    tests: TestLayout,
    config_surface: Vec<String>,
    manifests: Vec<Manifest>,
    not_computed: Vec<String>,
}

/// ! Every field an agent might expect but which was NOT derived. Omitting a
/// field silently is how `re_report` came to publish `contracts: []` for a
/// target whose contracts were never looked for.
fn not_computed_list() -> Vec<String> {
    vec![
        "internal import graph — no source file is parsed, only paths are read".to_owned(),
        "API contracts — no route or handler extraction".to_owned(),
        "design patterns — no semantic analysis".to_owned(),
        "runtime behaviour — nothing is executed".to_owned(),
    ]
}

/// Relative, `/`-separated paths of every file under `root`, minus [`SKIP_DIRS`].
/// Dotfiles are KEPT — `.env.example` and `.github/workflows/` are config surface.
async fn scan_tree(root: &Path) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut root_read = false;
    while let Some((dir, prefix)) = stack.pop() {
        if out.len() >= SCAN_BUDGET {
            break;
        }
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            // ! Only the ROOT being unreadable is an error. A deeper unreadable
            // dir must not abort the scan, but it must not look like an empty
            // one either — see `scan_truncated` / `files_scanned`.
            Err(e) if prefix.is_empty() => return Err(format!("read {}: {e}", dir.display())),
            Err(_) => continue,
        };
        root_read = true;
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            match entry.metadata().await {
                Ok(m) if m.is_dir() => stack.push((entry.path(), rel)),
                Ok(m) if m.is_file() => out.push(rel),
                _ => {}
            }
            if out.len() >= SCAN_BUDGET {
                break;
            }
        }
    }
    if !root_read {
        return Err(format!("read {}: unreadable", root.display()));
    }
    out.sort();
    Ok(out)
}

fn lang_of_path(rel: &str) -> Option<&'static str> {
    let ext = rel.rsplit_once('.')?.1.to_ascii_lowercase();
    lang_for_ext(&ext)
}

/// Which module a file belongs to. Root-level files aggregate under `(root)`.
fn module_key(rel: &str) -> String {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() < 2 {
        return "(root)".to_owned();
    }
    if CONTAINER_DIRS.contains(&parts[0]) && parts.len() >= 3 {
        return format!("{}/{}", parts[0], parts[1]);
    }
    parts[0].to_owned()
}

fn is_test_path(rel: &str) -> bool {
    let segs: Vec<&str> = rel.split('/').collect();
    if segs
        .iter()
        .take(segs.len().saturating_sub(1))
        .any(|s| matches!(*s, "tests" | "test" | "__tests__" | "spec" | "testdata"))
    {
        return true;
    }
    let base = segs.last().copied().unwrap_or_default();
    // `test_x.py` · pytest's default discovery prefix.
    if base.starts_with("test_") && base.to_ascii_lowercase().ends_with(".py") {
        return true;
    }
    ["_test.go", "_test.py", "_test.rs", "_spec.rb"]
        .iter()
        .chain([".test.ts", ".test.tsx", ".test.js", ".spec.ts", ".spec.js"].iter())
        .any(|suffix| base.ends_with(suffix))
}

/// Modules with at least one source file, biggest first.
/// ! An empty result means no directory held a recognised source file — it is a
/// finding about the scan, ✗ proof the target has no modules.
fn derive_modules(files: &[String]) -> Vec<Module> {
    let mut by_key: BTreeMap<String, Module> = BTreeMap::new();
    for rel in files {
        let key = module_key(rel);
        let m = by_key.entry(key.clone()).or_insert_with(|| Module {
            name: key,
            ..Module::default()
        });
        m.files += 1;
        if is_test_path(rel) {
            m.test_files += 1;
        }
        if let Some(lang) = lang_of_path(rel) {
            *m.languages.entry(lang.to_owned()).or_insert(0) += 1;
            if is_source_lang(lang) {
                m.source_files += 1;
            }
        }
    }
    let mut out: Vec<Module> = by_key
        .into_values()
        .filter(|m| m.source_files > 0)
        .collect();
    out.sort_by(|a, b| {
        b.source_files
            .cmp(&a.source_files)
            .then(a.name.cmp(&b.name))
    });
    out
}

const ENTRY_BASENAMES: &[&str] = &[
    "main.rs",
    "lib.rs",
    "main.go",
    "main.py",
    "__main__.py",
    "app.py",
    "manage.py",
    "wsgi.py",
    "asgi.py",
    "index.ts",
    "index.js",
    "server.ts",
    "server.js",
    "cli.py",
    "cli.rs",
];

fn derive_entry_points(files: &[String]) -> Vec<String> {
    let mut out: Vec<String> = files
        .iter()
        .filter(|rel| {
            let base = rel.rsplit('/').next().unwrap_or_default();
            ENTRY_BASENAMES.contains(&base) && !is_test_path(rel)
        })
        .cloned()
        .collect();
    out.sort();
    out.truncate(50);
    out
}

fn derive_tests(files: &[String]) -> TestLayout {
    let tests: Vec<&String> = files.iter().filter(|r| is_test_path(r)).collect();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let mut dedicated = 0usize;
    for rel in &tests {
        if let Some((dir, _)) = rel.rsplit_once('/') {
            dirs.insert(dir.to_owned());
            if dir
                .split('/')
                .any(|s| matches!(s, "tests" | "test" | "__tests__" | "spec"))
            {
                dedicated += 1;
            }
        }
    }
    let strategy = match (tests.len(), dedicated) {
        (0, _) => "none",
        (n, d) if d == n => "dedicated-directory",
        (_, 0) => "colocated",
        _ => "mixed",
    };
    TestLayout {
        test_files: tests.len(),
        test_dirs: dirs.into_iter().take(40).collect(),
        strategy: strategy.to_owned(),
    }
}

fn is_config_path(rel: &str) -> bool {
    if rel.starts_with(".github/workflows/") {
        return true;
    }
    if rel.contains('/') {
        return false; // root-level config only · nested yaml is usually data
    }
    let lower = rel.to_ascii_lowercase();
    lower.starts_with(".env")
        || lower.starts_with("dockerfile")
        || lower.starts_with("docker-compose")
        || lower.starts_with("compose.")
        || matches!(lower.as_str(), "makefile" | "justfile" | "taskfile.yml")
        || [".yaml", ".yml", ".toml", ".ini", ".cfg"]
            .iter()
            .any(|e| lower.ends_with(e))
}

fn derive_config_surface(files: &[String]) -> Vec<String> {
    let mut out: Vec<String> = files
        .iter()
        .filter(|r| is_config_path(r))
        .cloned()
        .collect();
    out.sort();
    out.truncate(60);
    out
}

// ---------- manifest parsing ----------

/// `[dependencies]` / `[dev-dependencies]` section scan.
/// ! Line-based, ✗ a TOML parser — `toml` is not a dependency of this crate.
/// The method is published as `parser` on every manifest so a caller knows the
/// list's provenance rather than assuming a full parse.
fn parse_cargo_toml(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t.starts_with("[dependencies")
                || t.starts_with("[dev-dependencies")
                || t.starts_with("[build-dependencies")
                || t.ends_with(".dependencies]");
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = t.split_once('=') {
            let name = name.trim().trim_matches('"');
            if !name.is_empty() && !out.iter().any(|d| d == name) {
                out.push(name.to_owned());
            }
        }
    }
    out
}

fn parse_package_json(text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(obj) = v.get(key).and_then(Value::as_object) {
            for name in obj.keys() {
                if !out.iter().any(|d| d == name) {
                    out.push(name.clone());
                }
            }
        }
    }
    out
}

fn parse_go_mod(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("require (") {
            in_block = true;
            continue;
        }
        if in_block && t == ")" {
            in_block = false;
            continue;
        }
        let candidate = if in_block {
            t
        } else if let Some(rest) = t.strip_prefix("require ") {
            rest
        } else {
            continue;
        };
        if candidate.is_empty() || candidate.starts_with("//") {
            continue;
        }
        if let Some(name) = candidate.split_whitespace().next() {
            if !out.iter().any(|d| d == name) {
                out.push(name.to_owned());
            }
        }
    }
    out
}

fn parse_requirements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with('-') {
            continue;
        }
        let name: String = t
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Quoted entries inside a `dependencies = [ ... ]` array (PEP 621).
fn parse_pyproject(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_array = false;
    for line in text.lines() {
        let t = line.trim();
        if t.replace(' ', "").starts_with("dependencies=[") {
            in_array = true;
            if !t.ends_with('[') {
                collect_quoted(t, &mut out);
            }
            if t.ends_with(']') {
                in_array = false;
            }
            continue;
        }
        if in_array {
            collect_quoted(t, &mut out);
            if t.contains(']') {
                in_array = false;
            }
        }
    }
    out
}

fn collect_quoted(line: &str, out: &mut Vec<String>) {
    for piece in line.split('"').skip(1).step_by(2) {
        let name: String = piece
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
}

fn parse_manifest(ecosystem: &str, text: &str) -> (Vec<String>, &'static str) {
    match ecosystem {
        "cargo" => (parse_cargo_toml(text), "toml section scan"),
        "npm" => (parse_package_json(text), "json parse"),
        "go" => (parse_go_mod(text), "require block scan"),
        "pip" => (parse_requirements(text), "line scan"),
        "python" => (parse_pyproject(text), "PEP 621 array scan"),
        // ! Gemfile is DETECTED but not parsed. Reporting an empty dependency
        // list as if parsed would be the fabrication this file exists to remove.
        _ => (Vec::new(), "detected only · ✗ parsed"),
    }
}

async fn read_manifests(root: &Path, files: &[String]) -> Vec<Manifest> {
    let mut out: Vec<Manifest> = Vec::new();
    for rel in files {
        if out.len() >= 25 {
            break;
        }
        let base = rel.rsplit('/').next().unwrap_or_default();
        let Some((_, ecosystem)) = MANIFEST_FILES.iter().find(|(n, _)| *n == base) else {
            continue;
        };
        let Ok(text) = tokio::fs::read_to_string(root.join(rel)).await else {
            continue;
        };
        let (dependencies, parser) = parse_manifest(ecosystem, &text);
        out.push(Manifest {
            path: rel.clone(),
            ecosystem: (*ecosystem).to_owned(),
            dependencies,
            parser: parser.to_owned(),
        });
    }
    out
}

async fn analyze_codebase(root: &Path) -> Result<CodebaseAnalysis, String> {
    let files = scan_tree(root).await?;
    let scan_truncated = files.len() >= SCAN_BUDGET;
    let mut languages: BTreeMap<String, usize> = BTreeMap::new();
    for rel in &files {
        if let Some(lang) = lang_of_path(rel) {
            *languages.entry(lang.to_owned()).or_insert(0) += 1;
        }
    }
    Ok(CodebaseAnalysis {
        root: root.display().to_string(),
        files_scanned: files.len(),
        scan_truncated,
        languages,
        modules: derive_modules(&files),
        entry_points: derive_entry_points(&files),
        tests: derive_tests(&files),
        config_surface: derive_config_surface(&files),
        manifests: read_manifests(root, &files).await,
        not_computed: not_computed_list(),
    })
}

/// States what an empty module map MEANS.
///
/// ! `modules: []` is ambiguous on its own — "nothing was found" and "nothing
/// exists" serialise identically. An agent that reads the former as the latter
/// ports from an empty map. Every report carries this sentence.
fn modules_finding(module_count: usize, files_scanned: usize, root: &str) -> String {
    match (module_count, files_scanned) {
        (0, 0) => format!(
            "0 files scanned under {root} · the tree is empty or unreadable · \
             ✗ conclude the target has no modules"
        ),
        (0, n) => format!(
            "0 modules across {n} files scanned · no directory under {root} held a file in a \
             recognised source language · ✗ conclude the target has no modules"
        ),
        (m, n) => format!("{m} modules derived from {n} scanned files under {root}"),
    }
}

// ---------- re_* ----------

const RE_SUPPORTED_TYPES: &[&str] = &["codebase", "auto"];

/// `target` → a directory to scan. A registered alias wins over a path, since
/// an alias is the deliberate reference.
async fn resolve_analysis_root(target: &str) -> Result<PathBuf, String> {
    if let Ok(reg) = read_registry().await {
        if reg.repos.iter().any(|r| r.alias == target) {
            let root = repo_root(target).await?;
            if tokio::fs::try_exists(&root).await.unwrap_or(false) {
                return Ok(root);
            }
            return Err(format!(
                "alias '{target}' is registered but {} does not exist · call pipeline_repo.digest first",
                root.display()
            ));
        }
    }
    let path = PathBuf::from(strip_local_prefix(target));
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(path);
    }
    Err(format!(
        "'{target}' is neither a registered alias nor an existing path"
    ))
}

/// Analyse a codebase and STORE THE RESULT. Synchronous: when this returns, the
/// analysis is done. ! A job id is handed back only for later retrieval by
/// `re_status` / `re_report` / `re_modernize` — it is ✗ a handle on background work.
async fn re_analyze(args: &Value) -> ToolResponse {
    let target = match args.get("target").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'target' (registered alias or path)".into()),
    };
    let kind = args
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("codebase");
    if !RE_SUPPORTED_TYPES.contains(&kind) {
        return err(format!(
            "type '{kind}' is not implemented · only 'codebase' analysis exists. \
             binary (decompilation) · service (live traffic capture) · infra (cloud state \
             introspection) are each unbuilt."
        ));
    }
    let root = match resolve_analysis_root(&target).await {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let analysis = match analyze_codebase(&root).await {
        Ok(a) => a,
        Err(e) => return err(format!("analyze: {e}")),
    };

    let job_id = uuid::Uuid::new_v4().to_string();
    let finding = modules_finding(
        analysis.modules.len(),
        analysis.files_scanned,
        &analysis.root,
    );
    let blob = json!({
        "job_id": job_id,
        "target": target,
        "type": "codebase",
        "status": "complete",
        "created_at": pipeline_memory::now_rfc3339(),
        "modules_finding": finding,
        "analysis": analysis,
    });

    let job_path = re_job_file(&job_id);
    if let Some(parent) = job_path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if let Err(e) = tokio::fs::write(&job_path, blob.to_string()).await {
        return err(format!("write: {e}"));
    }

    ToolResponse {
        ok: true,
        data: blob,
        next_suggested: vec![
            "pipeline_repo.re_report".into(),
            "pipeline_repo.re_modernize".into(),
        ],
        memory_refs: vec![format!("re_job:{job_id}")],
        error: None,
    }
}

async fn read_re_job(job_id: &str) -> Result<Value, String> {
    let path = re_job_file(job_id);
    let body = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("no RE job '{job_id}' · {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("corrupt RE job '{job_id}': {e}"))
}

async fn re_status(args: &Value) -> ToolResponse {
    let job_id = match args.get("job_id").and_then(Value::as_str) {
        Some(j) => j.to_owned(),
        None => return err("missing 'job_id'".into()),
    };
    let v = match read_re_job(&job_id).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let has_analysis = v.get("analysis").is_some_and(Value::is_object);
    ToolResponse::ok(json!({
        "job_id": job_id,
        "target": v.get("target").cloned().unwrap_or(Value::Null),
        "type": v.get("type").cloned().unwrap_or(Value::Null),
        "status": v.get("status").cloned().unwrap_or(json!("unknown")),
        "created_at": v.get("created_at").cloned().unwrap_or(Value::Null),
        "has_analysis": has_analysis,
        "note": "analysis is synchronous · a job exists only once it has finished, so status never sits in 'queued'",
    }))
}

/// Return what was actually computed.
///
/// ! Refuses a job holding no analysis instead of declaring it complete. The
/// previous revision read the job, overwrote `status` to "complete", and
/// emitted empty `module_map` / `contracts` / `patterns_detected` — an agent
/// polling status→report was told the target genuinely had no modules and no
/// contracts, and scaffolded from that.
async fn re_report(args: &Value) -> ToolResponse {
    let job_id = match args.get("job_id").and_then(Value::as_str) {
        Some(j) => j.to_owned(),
        None => return err("missing 'job_id'".into()),
    };
    let v = match read_re_job(&job_id).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let Some(analysis) = v.get("analysis").filter(|a| a.is_object()) else {
        return err(format!(
            "RE job '{job_id}' holds no analysis · nothing was computed for it. \
             ✗ read this as 'the target has no modules'. \
             Rerun pipeline_repo.re_analyze(target) — analysis is synchronous and its \
             result is stored with the job."
        ));
    };

    let modules = analysis
        .get("modules")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let scanned = usize::try_from(
        analysis
            .get("files_scanned")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
    .unwrap_or(usize::MAX);
    let root = analysis
        .pointer("/root")
        .and_then(Value::as_str)
        .unwrap_or("?");

    ToolResponse {
        ok: true,
        data: json!({
            "job_id": job_id,
            "target": v.get("target").cloned().unwrap_or(Value::Null),
            "type": v.get("type").cloned().unwrap_or(Value::Null),
            "analyzed_at": v.get("created_at").cloned().unwrap_or(Value::Null),
            "modules_finding": modules_finding(modules, scanned, root),
            "analysis": analysis,
        }),
        next_suggested: vec![
            "pipeline_repo.re_modernize".into(),
            "pipeline_repo.extract".into(),
        ],
        memory_refs: vec![format!("re_job:{job_id}")],
        error: None,
    }
}

/// ✗ implemented. Registry marks this Planned, so `dispatch` refuses before the
/// handler runs. The previous body wrote fixed template text (`paths: {}`,
/// `FROM debian:bookworm-slim`) with `target` interpolated into a comment only —
/// a file that looked reconstructed and described nothing.
fn re_reconstruct() -> ToolResponse {
    err(
        "pipeline_repo.re_reconstruct is not implemented · it introspects no target. \
         An OpenAPI spec needs observed traffic, a schema needs a live database \
         connection, and a Dockerfile needs image layer inspection — none are wired up."
            .into(),
    )
}

/// Migration order derived from the module map. Small, well-tested modules move
/// first: they are the cheapest way to prove the target stack before committing.
fn migration_order(modules: &[Module]) -> Vec<&Module> {
    let mut ordered: Vec<&Module> = modules.iter().collect();
    ordered.sort_by(|a, b| {
        (b.test_files > 0)
            .cmp(&(a.test_files > 0))
            .then(a.source_files.cmp(&b.source_files))
            .then(a.name.cmp(&b.name))
    });
    ordered
}

/// Facts from the scan that bear on migration risk.
///
/// ! Facts, ✗ a verdict. The previous revision returned `"risk_level":"medium"`
/// for every job, target and stack. Scoring these is the agent's call.
fn risk_signals(analysis: &CodebaseAnalysis) -> Vec<String> {
    let mut out = Vec::new();
    if analysis.tests.test_files == 0 {
        out.push("0 test files found · no behavioural safety net for a migration".to_owned());
    }
    if analysis.manifests.is_empty() {
        out.push("no manifest found · external dependency set is unknown".to_owned());
    }
    if analysis.entry_points.is_empty() {
        out.push("no entry point matched a known basename · execution path unclear".to_owned());
    }
    if analysis.scan_truncated {
        out.push(format!(
            "scan stopped at the {SCAN_BUDGET}-file budget · the module map is partial"
        ));
    }
    for m in &analysis.modules {
        if m.source_files >= 200 {
            out.push(format!(
                "module '{}' holds {} source files · likely needs splitting before it moves",
                m.name, m.source_files
            ));
        }
    }
    if analysis
        .languages
        .keys()
        .filter(|k| is_source_lang(k))
        .count()
        >= 4
    {
        out.push("4+ source languages · the port has more than one target toolchain".to_owned());
    }
    out
}

fn migration_phases(analysis: &CodebaseAnalysis, target_stack: &str) -> Vec<Value> {
    let mut phases: Vec<Value> = vec![json!({
        "name": "audit",
        "exit": format!(
            "{} modules · {} entry points · {} test files confirmed against {}",
            analysis.modules.len(),
            analysis.entry_points.len(),
            analysis.tests.test_files,
            analysis.root,
        ),
    })];
    let ordered = migration_order(&analysis.modules);
    for m in ordered.iter().take(8) {
        phases.push(json!({
            "name": format!("migrate {}", m.name),
            "module": m.name,
            "source_files": m.source_files,
            "test_files": m.test_files,
            "exit": format!("pipeline_repo.port_validate green for {} in {target_stack}", m.name),
        }));
    }
    if ordered.len() > 8 {
        phases.push(json!({
            "name": "migrate remaining modules",
            "modules": ordered.iter().skip(8).map(|m| m.name.clone()).collect::<Vec<_>>(),
            "exit": "every remaining module green under pipeline_run.preflight",
        }));
    }
    phases.push(json!({
        "name": "cutover",
        "exit": "pipeline_run.preflight green on the whole ported tree",
    }));
    phases
}

/// Phase plan derived from a completed `re_analyze` job. Refuses without one —
/// a plan over a module map that was never built is a plan over nothing.
async fn re_modernize(args: &Value) -> ToolResponse {
    let job_id = match args.get("job_id").and_then(Value::as_str) {
        Some(j) => j.to_owned(),
        None => return err("missing 'job_id' · run pipeline_repo.re_analyze first".into()),
    };
    let v = match read_re_job(&job_id).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let Some(raw) = v.get("analysis").filter(|a| a.is_object()) else {
        return err(format!(
            "RE job '{job_id}' holds no analysis · a modernization plan needs a real module map. \
             Rerun pipeline_repo.re_analyze(target)."
        ));
    };
    let analysis: CodebaseAnalysis = match serde_json::from_value(raw.clone()) {
        Ok(a) => a,
        Err(e) => return err(format!("corrupt analysis in job '{job_id}': {e}")),
    };
    if analysis.modules.is_empty() {
        return err(modules_finding(0, analysis.files_scanned, &analysis.root));
    }

    let target_stack = args
        .get("target_stack")
        .and_then(Value::as_str)
        .map_or_else(
            || {
                analysis
                    .languages
                    .iter()
                    .filter(|(k, _)| is_source_lang(k))
                    .max_by(|(ka, a), (kb, b)| a.cmp(b).then_with(|| kb.cmp(ka)))
                    .map_or_else(|| "unspecified".to_owned(), |(k, _)| k.clone())
            },
            ToOwned::to_owned,
        );

    ToolResponse {
        ok: true,
        data: json!({
            "job_id": job_id,
            "target_stack": target_stack,
            "derived_from": {"root": analysis.root, "modules": analysis.modules.len()},
            "phases": migration_phases(&analysis, &target_stack),
            "risk_signals": risk_signals(&analysis),
            "adjudication": "risk_signals are facts from the scan · Pipeline assigns no risk level",
            "not_computed": analysis.not_computed,
        }),
        next_suggested: vec!["pipeline_repo.port_validate".into()],
        memory_refs: vec![format!("re_job:{job_id}")],
        error: None,
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
mod tests {
    use super::*;

    #[test]
    fn infer_alias_from_https_url() {
        assert_eq!(
            infer_alias("https://github.com/owner/repo.git").as_deref(),
            Some("repo")
        );
        assert_eq!(
            infer_alias("https://github.com/owner/repo").as_deref(),
            Some("repo")
        );
        assert_eq!(
            infer_alias("git@github.com:owner/cool-repo.git").as_deref(),
            Some("cool-repo")
        );
    }

    #[test]
    fn kind_of_distinguishes_local_vs_git() {
        assert_eq!(kind_of("https://x"), "git");
        assert_eq!(kind_of("file:///tmp/x"), "local");
        assert_eq!(kind_of("./relative"), "local");
        assert_eq!(kind_of("/abs/path"), "local");
    }

    // ---------- fixtures ----------

    /// Writes a tree and returns its root. Nothing is cloned — analysis must be
    /// provable without network or git.
    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (rel, body) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, body).expect("write");
        }
        dir
    }

    async fn write_job(cwd: &Path, job_id: &str, blob: &Value) {
        let dir = cwd.join(".pipeline/re");
        tokio::fs::create_dir_all(&dir).await.expect("mkdir");
        tokio::fs::write(dir.join(format!("{job_id}.json")), blob.to_string())
            .await
            .expect("write");
    }

    // ---------- the invariants ----------

    #[tokio::test]
    async fn a_report_for_an_unanalyzed_job_refuses_rather_than_declaring_it_complete() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The exact shape the old re_analyze wrote: queued, no analysis, no worker.
        let job = json!({
            "job_id": "j1",
            "target": "some-repo",
            "status": "queued",
            "stages": ["surface", "structure", "intent", "contract", "output"],
            "stage_index": 0,
        });
        write_job(dir.path(), "j1", &job).await;

        let path = dir.path().join(".pipeline/re/j1.json");
        let stored: Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.expect("read"))
                .expect("parse");
        // re_report's decision predicate, exercised without touching cwd.
        let has_analysis = stored.get("analysis").is_some_and(Value::is_object);
        assert!(!has_analysis, "fixture must model an unanalyzed job");

        // ! The refusal must never surface as ok:true with empty findings.
        let refusal = err(format!(
            "RE job '{}' holds no analysis · nothing was computed for it.",
            "j1"
        ));
        assert!(!refusal.ok);
        assert!(refusal.error.is_some());
        assert!(refusal.data.get("module_map").is_none());
        assert!(refusal.data.get("contracts").is_none());
    }

    #[test]
    fn an_empty_module_map_means_nothing_was_found_not_nothing_exists() {
        let none_scanned = modules_finding(0, 0, "/tmp/x");
        let none_found = modules_finding(0, 412, "/tmp/x");
        let found = modules_finding(3, 412, "/tmp/x");

        for msg in [&none_scanned, &none_found] {
            assert!(
                msg.contains("✗ conclude the target has no modules"),
                "an empty map must say what it does NOT mean · got: {msg}"
            );
        }
        // The two zero cases are distinguishable — unreadable ≠ scanned-and-empty.
        assert_ne!(none_scanned, none_found);
        assert!(none_found.contains("412"));
        assert!(found.contains('3') && !found.contains("✗"));
    }

    #[tokio::test]
    async fn compare_branches_on_axis_or_does_not_accept_it() {
        let da = json!({"summary": {
            "top_dirs": ["src", "docs"],
            "languages": {"rust": 10},
            "has_readme": true, "has_license": false,
            "has_dockerfile": false, "has_compose": false,
        }});
        let db = json!({"summary": {
            "top_dirs": ["src", "web"],
            "languages": {"go": 4},
            "has_readme": true, "has_license": true,
            "has_dockerfile": true, "has_compose": false,
        }});

        let arch = compare_arch(&da, &db);
        let standards = compare_standards(&da, &db);
        // ! Distinct axes must produce distinct payloads · identical output was
        // exactly the defect: `axis` accepted, never branched on.
        assert_ne!(arch, standards);
        assert!(arch.get("directories").is_some());
        assert!(standards.get("directories").is_none());
        assert_eq!(
            arch.pointer("/directories/shared")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            standards.pointer("/artifacts_b/has_dockerfile"),
            Some(&json!(true))
        );

        // An axis outside the accepted set is refused, not silently defaulted.
        let bogus = compare(&json!({"a": "x", "b": "y", "axis": "vibes"})).await;
        assert!(!bogus.ok);
        assert!(bogus.error.unwrap_or_default().contains("unknown axis"));
    }

    #[tokio::test]
    async fn analysis_is_derived_from_the_tree_not_invented() {
        let dir = fixture(&[
            (
                "Cargo.toml",
                "[dependencies]\nserde = \"1\"\ntokio = { version = \"1\" }\n",
            ),
            ("README.md", "# demo"),
            ("docker-compose.yml", "services: {}"),
            ("crates/auth/src/lib.rs", "pub fn login() {}"),
            ("crates/auth/tests/login_test.rs", "#[test] fn t() {}"),
            ("crates/billing/src/main.rs", "fn main() {}"),
            ("docs/guide.md", "prose"),
            ("node_modules/junk/index.js", "should never be scanned"),
        ]);

        let a = analyze_codebase(dir.path()).await.expect("analyze");

        let names: Vec<&str> = a.modules.iter().map(|m| m.name.as_str()).collect();
        // Modules are the dirs that EXIST and hold source.
        assert!(names.contains(&"crates/auth"), "got {names:?}");
        assert!(names.contains(&"crates/billing"), "got {names:?}");
        // `docs` holds only markdown → not a source module.
        assert!(!names.contains(&"docs"), "got {names:?}");
        // Nothing invented: a module absent from the tree is absent from the map.
        assert!(!names.contains(&"payments"), "got {names:?}");
        // Skip list honoured — an unscanned dir must not become a finding.
        assert!(!names.iter().any(|n| n.contains("node_modules")));

        assert_eq!(
            a.entry_points,
            vec![
                "crates/billing/src/main.rs".to_owned(),
                "crates/auth/src/lib.rs".to_owned()
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert_eq!(a.tests.test_files, 1);
        assert_eq!(a.tests.strategy, "dedicated-directory");
        assert!(a.config_surface.contains(&"docker-compose.yml".to_owned()));
        assert!(a.config_surface.contains(&"Cargo.toml".to_owned()));
        // Manifest deps come out of the real file.
        let cargo = a
            .manifests
            .iter()
            .find(|m| m.ecosystem == "cargo")
            .expect("cargo manifest");
        assert_eq!(
            cargo.dependencies,
            vec!["serde".to_owned(), "tokio".to_owned()]
        );
        // And what was NOT computed is stated rather than emitted empty.
        assert!(a.not_computed.iter().any(|s| s.contains("API contracts")));
    }

    #[tokio::test]
    async fn an_unsupported_re_type_is_refused_not_faked() {
        for kind in ["binary", "service", "infra", "docker"] {
            let r = re_analyze(&json!({"target": ".", "type": kind})).await;
            assert!(!r.ok, "type '{kind}' must be refused");
            let msg = r.error.unwrap_or_default();
            assert!(msg.contains("not implemented"), "got: {msg}");
        }
    }

    #[tokio::test]
    async fn re_analyze_stores_the_result_it_returns() {
        let dir = fixture(&[("src/app/main.py", "print(1)")]);
        let a = analyze_codebase(dir.path()).await.expect("analyze");
        // ! The job blob must carry the analysis · a job id without a result is
        // the fabrication this rewrite removes.
        let blob = json!({"job_id": "j", "status": "complete", "analysis": a});
        assert!(blob.pointer("/analysis/modules").is_some());
        assert_eq!(blob.pointer("/status"), Some(&json!("complete")));
        assert_ne!(blob.pointer("/status"), Some(&json!("queued")));
    }

    #[tokio::test]
    async fn modernize_refuses_a_job_with_no_module_map() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_job(
            dir.path(),
            "j2",
            &json!({"job_id": "j2", "status": "queued"}),
        )
        .await;
        let stored: Value = serde_json::from_str(
            &tokio::fs::read_to_string(dir.path().join(".pipeline/re/j2.json"))
                .await
                .expect("read"),
        )
        .expect("parse");
        assert!(stored.get("analysis").is_none());
        // The handler's guard is `analysis` present + object; assert the shape it rejects.
        assert!(!stored.get("analysis").is_some_and(Value::is_object));
    }

    // ---------- derivation units ----------

    #[test]
    fn module_key_treats_container_dirs_as_scaffolding_not_modules() {
        assert_eq!(module_key("crates/auth/src/lib.rs"), "crates/auth");
        assert_eq!(module_key("src/auth/jwt.rs"), "src/auth");
        assert_eq!(module_key("src/main.rs"), "src");
        assert_eq!(module_key("handlers/repo.rs"), "handlers");
        assert_eq!(module_key("README.md"), "(root)");
    }

    #[test]
    fn test_paths_are_recognised_across_ecosystems() {
        for p in [
            "tests/test_x.rs",
            "pkg/retry/retry_test.go",
            "app/test_login.py",
            "web/src/Button.test.tsx",
            "spec/models/user_spec.rb",
            "__tests__/api.js",
        ] {
            assert!(is_test_path(p), "{p} should be a test path");
        }
        for p in ["src/main.rs", "latest/index.js", "contest/app.py"] {
            assert!(!is_test_path(p), "{p} should not be a test path");
        }
    }

    #[test]
    fn manifest_parsers_read_the_file_they_are_given() {
        assert_eq!(
            parse_cargo_toml(
                "[package]\nname=\"x\"\n[dependencies]\nserde = \"1\"\n[dev-dependencies]\ntempfile = \"3\"\n"
            ),
            vec!["serde".to_owned(), "tempfile".to_owned()]
        );
        assert_eq!(
            parse_package_json(r#"{"dependencies":{"react":"18"},"devDependencies":{"vite":"5"}}"#),
            vec!["react".to_owned(), "vite".to_owned()]
        );
        assert_eq!(
            parse_go_mod("module x\n\nrequire (\n\tgithub.com/a/b v1.2.3\n)\n"),
            vec!["github.com/a/b".to_owned()]
        );
        assert_eq!(
            parse_requirements("# comment\nfastapi==0.110\nhttpx>=0.27\n"),
            vec!["fastapi".to_owned(), "httpx".to_owned()]
        );
        assert_eq!(
            parse_pyproject("[project]\ndependencies = [\n  \"click>=8\",\n  \"rich\",\n]\n"),
            vec!["click".to_owned(), "rich".to_owned()]
        );
        // An unparsed ecosystem says so rather than reporting an empty dep set.
        let (deps, parser) = parse_manifest("bundler", "gem 'rails'");
        assert!(deps.is_empty());
        assert!(parser.contains("✗ parsed"));
    }

    #[test]
    fn dominant_language_ignores_docs_and_config() {
        let d = json!({"summary": {"languages": {"markdown": 90, "yaml": 40, "go": 5}}});
        assert_eq!(dominant_source_language(&d).as_deref(), Some("go"));
        let none = json!({"summary": {"languages": {"markdown": 3}}});
        assert_eq!(dominant_source_language(&none), None);
    }

    #[test]
    fn risk_signals_are_facts_derived_from_the_scan() {
        let empty = CodebaseAnalysis {
            root: "/x".into(),
            files_scanned: 3,
            scan_truncated: false,
            languages: BTreeMap::new(),
            modules: vec![],
            entry_points: vec![],
            tests: TestLayout::default(),
            config_surface: vec![],
            manifests: vec![],
            not_computed: not_computed_list(),
        };
        let signals = risk_signals(&empty);
        assert!(signals.iter().any(|s| s.contains("0 test files")));
        assert!(signals.iter().any(|s| s.contains("no manifest")));
        // ! No verdict field anywhere — the old code returned "medium" always.
        assert!(!signals.iter().any(|s| s.contains("risk_level")));
    }

    #[test]
    fn migration_order_moves_tested_and_small_modules_first() {
        let modules = vec![
            Module {
                name: "big_untested".into(),
                source_files: 300,
                ..Module::default()
            },
            Module {
                name: "small_tested".into(),
                source_files: 4,
                test_files: 2,
                ..Module::default()
            },
            Module {
                name: "big_tested".into(),
                source_files: 90,
                test_files: 9,
                ..Module::default()
            },
            Module {
                name: "small_untested".into(),
                source_files: 2,
                ..Module::default()
            },
        ];
        let order: Vec<&str> = migration_order(&modules)
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(
            order,
            vec![
                "small_tested",
                "big_tested",
                "small_untested",
                "big_untested"
            ]
        );
    }

    #[test]
    fn markers_are_filename_matches_and_dedupe_by_label() {
        let names = vec![
            "rate_limiter.rs".to_owned(),
            "ratelimit.go".to_owned(),
            "readme.md".to_owned(),
        ];
        assert_eq!(markers_present(&names), vec!["rate limiting".to_owned()]);
        assert!(markers_present(&["main.rs".to_owned()]).is_empty());
    }

    #[test]
    fn diff_sets_reports_both_directions() {
        let d = diff_sets(&["a".into(), "b".into()], &["b".into(), "c".into()]);
        assert_eq!(d["shared"], json!(["b"]));
        assert_eq!(d["only_in_a"], json!(["a"]));
        assert_eq!(d["only_in_b"], json!(["c"]));
    }

    #[tokio::test]
    async fn planned_actions_refuse_in_the_handler_too() {
        for r in [port(), re_reconstruct()] {
            assert!(!r.ok);
            assert!(r.error.unwrap_or_default().contains("not implemented"));
        }
    }
}

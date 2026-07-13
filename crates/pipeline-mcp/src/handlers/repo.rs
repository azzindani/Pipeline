//! `pipeline_repo` handler · register · list · remove · digest.
//!
//! Registry: `.pipeline/repos/registry.json` (array of {alias, url, kind, added_at}).
//! Clones land in `.pipeline/repos/<alias>/` on first `digest` (lazy).
//!
//! Day-5 ships register/list/remove/digest. extract · port · port_validate ·
//! compare · capability_graph · re_* return not_implemented (need digest →
//! semantic embedding plus stack-aware codegen · MVP+ work).

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
        "port" => port(&req.args).await,
        "port_validate" => port_validate(&req.args).await,
        "apply_standards" => apply_standards(&req.args).await,
        "capability_graph" => capability_graph().await,
        "re_analyze" => re_analyze(&req.args).await,
        "re_status" => re_status(&req.args).await,
        "re_report" => re_report(&req.args).await,
        "re_reconstruct" => re_reconstruct(&req.args).await,
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
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(REGISTRY_FILE)
}

fn clone_dir(alias: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(REGISTRY_DIR)
        .join(alias)
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
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".pipeline/digests")
        .join(format!("{alias}.json"))
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
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    let lang = match ext {
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
                        _ => return,
                    };
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

// ---------- list_capabilities · extract · compare · port · port_validate ----------

async fn read_digest(alias: &str) -> Result<Value, String> {
    let path = digest_file(alias);
    let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
        format!("digest for '{alias}' not found · call pipeline_repo.digest first ({e})")
    })?;
    serde_json::from_str(&text).map_err(|e| format!("corrupt digest: {e}"))
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
    // Keyword markers in file names · cheap.
    let markers = [
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
    let names = collect_filenames(&dir).await;
    for (token, label) in markers {
        if names.iter().any(|n| n.to_ascii_lowercase().contains(token)) {
            capabilities.push(json!({
                "name": label,
                "kind": "marker",
                "location": format!("files matching *{token}*"),
            }));
        }
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
    let exts = ["rs", "py", "ts", "tsx", "js", "go", "java", "rb", "kt"];
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        if let Some(ext) = entry.path().extension().and_then(|s| s.to_str()) {
            if exts.contains(&ext.to_ascii_lowercase().as_str()) {
                return true;
            }
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

/// Compare two digested repos along a chosen axis: features | arch | standards.
/// Day-8b returns side-by-side language breakdowns and presence flags.
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
    let da = match read_digest(&a).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let db = match read_digest(&b).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };

    let summary_a = da.get("summary").cloned().unwrap_or(json!({}));
    let summary_b = db.get("summary").cloned().unwrap_or(json!({}));
    ToolResponse {
        ok: true,
        data: json!({
            "axis": axis,
            "a": {"alias": a, "summary": summary_a},
            "b": {"alias": b, "summary": summary_b},
            "note": "Day-8b · structural side-by-side · semantic compare lands MVP",
        }),
        next_suggested: vec!["pipeline_repo.list_capabilities".into()],
        memory_refs: vec![format!("digest:{a}"), format!("digest:{b}")],
        error: None,
    }
}

/// Emit a port plan from `alias` to `target_lang`. Day-8b returns a
/// per-file mapping with target extensions and language-specific notes;
/// actual translation is agent-driven.
async fn port(args: &Value) -> ToolResponse {
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'alias'".into()),
    };
    let target_lang = match args.get("target_lang").and_then(Value::as_str) {
        Some(s) => s.to_lowercase(),
        None => return err("missing 'target_lang' (rust|python|typescript|go)".into()),
    };
    let scope = args.get("scope").and_then(Value::as_str).unwrap_or("full");
    let digest = match read_digest(&alias).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };

    let target_ext = match target_lang.as_str() {
        "rust" => "rs",
        "python" => "py",
        "typescript" | "ts" => "ts",
        "go" => "go",
        other => return err(format!("unsupported target_lang '{other}'")),
    };

    let langs = digest
        .pointer("/summary/languages")
        .cloned()
        .unwrap_or(json!({}));
    let mut translation_paths: Vec<Value> = Vec::new();
    if let Some(obj) = langs.as_object() {
        for (lang, count) in obj {
            translation_paths.push(json!({
                "from": lang,
                "to": target_lang,
                "files_at_source": count,
                "confidence": port_confidence(lang.as_str(), &target_lang),
            }));
        }
    }

    ToolResponse {
        ok: true,
        data: json!({
            "alias": alias,
            "target_lang": target_lang,
            "target_ext": target_ext,
            "scope": scope,
            "translation_paths": translation_paths,
            "next_actions": [
                "agent walks each source file in scope",
                "translates module-by-module preserving public API",
                "calls pipeline_repo.port_validate(path) on each module",
                "fixes failures using pipeline_run.fix_suggestion",
            ],
        }),
        next_suggested: vec!["pipeline_repo.port_validate".into()],
        memory_refs: vec![format!("digest:{alias}")],
        error: None,
    }
}

#[allow(clippy::match_same_arms)] // arms intentionally split to document each translation pair
fn port_confidence(from: &str, to: &str) -> &'static str {
    match (from, to) {
        ("python", "rust" | "go" | "typescript" | "ts") | ("go", "rust") => "high",
        (_, "python") => "high",
        ("typescript" | "ts", "rust") | ("rust", "go") => "medium",
        _ => "medium",
    }
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

async fn apply_standards(args: &Value) -> ToolResponse {
    let alias = match args.get("alias").and_then(Value::as_str) {
        Some(a) => a.to_owned(),
        None => return err("missing 'alias'".into()),
    };
    let digest = match read_digest(&alias).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let s = digest.get("summary").cloned().unwrap_or(json!({}));
    let mut report = serde_json::Map::new();
    report.insert(
        "has_dockerfile".into(),
        s.get("has_dockerfile").cloned().unwrap_or(json!(false)),
    );
    report.insert(
        "has_compose".into(),
        s.get("has_compose").cloned().unwrap_or(json!(false)),
    );
    report.insert(
        "has_readme".into(),
        s.get("has_readme").cloned().unwrap_or(json!(false)),
    );
    report.insert(
        "has_license".into(),
        s.get("has_license").cloned().unwrap_or(json!(false)),
    );
    let total = report.len();
    let passing = report
        .values()
        .filter(|v| v.as_bool() == Some(true))
        .count();
    #[allow(clippy::cast_precision_loss)]
    let score = (passing as f64 / total as f64) * 100.0;
    ToolResponse::ok(json!({
        "alias": alias,
        "compliance": report,
        "score_percent": score,
        "tip": "presence-only · semantic standards check lands MVP via pipeline_standards.check",
    }))
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

async fn re_analyze(args: &Value) -> ToolResponse {
    let target = match args.get("target").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'target'".into()),
    };
    let kind = args.get("type").and_then(Value::as_str).unwrap_or("auto");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let job_id = uuid::Uuid::new_v4().to_string();
    let dir = cwd.join(".pipeline/re");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return err(format!("mkdir: {e}"));
    }
    let job_path = dir.join(format!("{job_id}.json"));
    let blob = json!({
        "job_id": job_id,
        "target": target,
        "type": kind,
        "status": "queued",
        "created_at": pipeline_memory::now_rfc3339(),
        "stages": ["surface", "structure", "intent", "contract", "output"],
        "stage_index": 0,
    });
    if let Err(e) = tokio::fs::write(&job_path, blob.to_string()).await {
        return err(format!("write: {e}"));
    }
    ToolResponse {
        ok: true,
        data: blob,
        next_suggested: vec![
            "pipeline_repo.re_status".into(),
            "pipeline_repo.re_report".into(),
        ],
        memory_refs: vec![format!("re_job:{job_id}")],
        error: None,
    }
}

async fn re_status(args: &Value) -> ToolResponse {
    let job_id = match args.get("job_id").and_then(Value::as_str) {
        Some(j) => j.to_owned(),
        None => return err("missing 'job_id'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join(".pipeline/re").join(format!("{job_id}.json"));
    let body = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => return err(format!("read: {e}")),
    };
    let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
    ToolResponse::ok(v)
}

async fn re_report(args: &Value) -> ToolResponse {
    let job_id = match args.get("job_id").and_then(Value::as_str) {
        Some(j) => j.to_owned(),
        None => return err("missing 'job_id'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join(".pipeline/re").join(format!("{job_id}.json"));
    let body = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => return err(format!("read: {e}")),
    };
    let mut v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
    // Day-9 stub: mark complete + emit a synthetic report shape that downstream
    // tools (port, scaffold) can consume. Real RE pipeline lands MVP+.
    v["status"] = json!("complete");
    v["module_map"] = json!([]);
    v["contracts"] = json!([]);
    v["patterns_detected"] = json!([]);
    v["reported_at"] = json!(pipeline_memory::now_rfc3339());
    ToolResponse::ok(v)
}

async fn re_reconstruct(args: &Value) -> ToolResponse {
    let kind = match args.get("kind").and_then(Value::as_str) {
        Some(k) => k.to_owned(),
        None => return err("missing 'kind' (api|schema|dockerfile)".into()),
    };
    let target = args.get("target").and_then(Value::as_str).unwrap_or("");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join(".pipeline/re/reconstructed");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return err(format!("mkdir: {e}"));
    }
    let (path, body): (PathBuf, String) = match kind.as_str() {
        "api" => (
            dir.join("openapi.yaml"),
            format!(
                "# Reconstructed from {target}\nopenapi: 3.1.0\ninfo:\n  title: Reconstructed API\n  version: 0.0.1\npaths: {{}}\n"
            ),
        ),
        "schema" => (
            dir.join("schema.sql"),
            format!(
                "-- Reconstructed schema from {target}\n-- TODO: derive from live DB introspection\n"
            ),
        ),
        "dockerfile" => (
            dir.join("Dockerfile"),
            format!(
                "# Reconstructed from {target}\n# TODO: derive from `docker history`\nFROM debian:bookworm-slim\n"
            ),
        ),
        other => return err(format!("unsupported kind '{other}'")),
    };
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Err(e) = tokio::fs::write(&path, body).await {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"kind": kind, "target": target, "path": path.display().to_string()}))
}

#[allow(clippy::unused_async)]
async fn re_modernize(args: &Value) -> ToolResponse {
    let job_id = args.get("job_id").and_then(Value::as_str).unwrap_or("");
    let target_stack = args
        .get("target_stack")
        .and_then(Value::as_str)
        .unwrap_or("rust");
    ToolResponse::ok(json!({
        "job_id": job_id,
        "target_stack": target_stack,
        "phases": [
            {"name": "audit", "exit": "RE report complete"},
            {"name": "extract", "exit": "core capabilities identified"},
            {"name": "translate", "exit": "modules emitted in target stack"},
            {"name": "validate", "exit": "pipeline_run.preflight green per module"},
            {"name": "cutover", "exit": "feature parity with strangler fig"},
        ],
        "risk_level": "medium",
        "tip": "Day-9 stub plan · execution lives in agent loop driving repo.port + run.preflight",
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
}

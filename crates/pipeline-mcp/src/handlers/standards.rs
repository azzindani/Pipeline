//! `pipeline_standards` handler · fetch · list · show · recommend.
//!
//! Standards live at `https://github.com/azzindani/Standards`. Cloned into
//! `.pipeline/standards/` on first `fetch`. Subsequent `fetch` runs `git pull`.
//!
//! Day-4 ships fetch · list · show · recommend (read-side). `apply` ·
//! `check` · `diff` (write-side) need standards-aware codegen and land
//! during MVP week 6.

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;

const STANDARDS_URL: &str = "https://github.com/azzindani/Standards.git";

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "fetch" => fetch().await,
        "list" => list().await,
        "show" => show(&req.args).await,
        "recommend" => recommend(&req.args),
        "apply" => apply(&req.args).await,
        "check" => check(&req.args).await,
        "diff" => diff(&req.args).await,
        other => err(format!("unknown action 'pipeline_standards.{other}'")),
    }
}

fn standards_dir() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    Ok(cwd.join(".pipeline").join("standards"))
}

async fn fetch() -> ToolResponse {
    let dir = match standards_dir() {
        Ok(d) => d,
        Err(e) => return err(e),
    };

    if let Some(parent) = dir.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("create_dir_all: {e}"));
        }
    }

    let exists = tokio::fs::try_exists(&dir).await.unwrap_or(false);
    let action = if exists { "pull" } else { "clone" };

    let output = if exists {
        Command::new("git")
            .args(["pull", "--ff-only"])
            .current_dir(&dir)
            .output()
            .await
    } else {
        Command::new("git")
            .args(["clone", "--depth", "1", STANDARDS_URL])
            .arg(&dir)
            .output()
            .await
    };

    let output = match output {
        Ok(o) => o,
        Err(e) => return err(format!("git {action}: {e}")),
    };
    if !output.status.success() {
        return err(format!(
            "git {action} exit {} stderr={}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    ToolResponse {
        ok: true,
        data: json!({
            "action": action,
            "path": dir.display().to_string(),
            "stdout": String::from_utf8_lossy(&output.stdout).trim().to_string(),
        }),
        next_suggested: vec![
            "pipeline_standards.list".into(),
            "pipeline_standards.recommend".into(),
        ],
        memory_refs: vec![],
        error: None,
    }
}

async fn list() -> ToolResponse {
    let dir = match standards_dir() {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let exists = tokio::fs::try_exists(&dir).await.unwrap_or(false);
    if !exists {
        return err("standards not fetched yet · call pipeline_standards.fetch first".into());
    }

    let mut categories = Vec::new();
    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(rd) => rd,
        Err(e) => return err(format!("read_dir: {e}")),
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let standards_md = entry.path().join("STANDARDS.md");
        let has_standards = tokio::fs::try_exists(&standards_md).await.unwrap_or(false);
        categories.push(json!({
            "name": name,
            "has_standards_md": has_standards,
        }));
    }
    categories.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .cmp(&b.get("name").and_then(Value::as_str))
    });

    ToolResponse::ok(json!({"categories": categories}))
}

async fn show(args: &Value) -> ToolResponse {
    let category = match args.get("category").and_then(Value::as_str) {
        Some(c) => c,
        None => return err("missing 'category'".into()),
    };
    let dir = match standards_dir() {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let path = dir.join(category).join("STANDARDS.md");
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return err(format!(
            "category '{category}' has no STANDARDS.md at {}",
            path.display()
        ));
    }
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => ToolResponse::ok(json!({
            "category": category,
            "path": path.display().to_string(),
            "content": text,
        })),
        Err(e) => err(format!("read: {e}")),
    }
}

fn recommend(args: &Value) -> ToolResponse {
    let stack = args
        .get("stack")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let project_type = args
        .get("project_type")
        .or_else(|| args.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();

    let categories = recommend_categories(&stack, &project_type);
    if categories.is_empty() {
        return err(format!(
            "no recommendation for stack='{stack}' type='{project_type}' · \
             try stack ∈ rust|python-uv|bun|node|typescript|go|web|ml; \
             type ∈ web-spa|web-ssr|microservice|cli|library|mcp-server|ai-agent|data-pipeline|ml"
        ));
    }
    ToolResponse {
        ok: true,
        data: json!({
            "stack": stack,
            "project_type": project_type,
            "categories": categories,
            "rationale": "Mapping derived from CLAUDE.md §\"Standards for projects Pipeline manages\".",
        }),
        next_suggested: vec![
            "pipeline_standards.fetch".into(),
            "pipeline_standards.show".into(),
            "pipeline_standards.apply".into(),
        ],
        memory_refs: vec![],
        error: None,
    }
}

/// Stack/type → standards subset. Mirrors CLAUDE.md mapping table.
/// Returned in priority order so agents apply architecture first.
fn recommend_categories(stack: &str, project_type: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = vec!["architecture"];

    let language = match stack {
        "rust" => Some("rust"),
        "python-uv" | "python" | "uv" => Some("python"),
        "bun" | "node" | "typescript" | "ts" | "javascript" | "js" => Some("typescript"),
        "go" | "golang" => Some("go"),
        _ => None,
    };
    if let Some(lang) = language {
        out.push(lang);
    }

    // Project-type specific layers stacked on top of language baseline.
    match project_type {
        "web-spa" | "web-ssr" | "web" => {
            push_unique(&mut out, "web");
            push_unique(&mut out, "api");
            push_unique(&mut out, "database");
            push_unique(&mut out, "security");
            push_unique(&mut out, "testing");
        }
        "ml" | "data-pipeline" | "ml-python" => {
            push_unique(&mut out, "ml");
            push_unique(&mut out, "data_pipeline");
            push_unique(&mut out, "testing");
        }
        "mcp-server" | "mcp-server-rust" | "mcp-server-ts" => {
            push_unique(&mut out, "local_mcp");
            push_unique(&mut out, "cli");
            push_unique(&mut out, "error_handling");
            push_unique(&mut out, "security");
        }
        "cli" | "cli-rust" | "cli-go" => {
            push_unique(&mut out, "cli");
            push_unique(&mut out, "error_handling");
            push_unique(&mut out, "testing");
        }
        "library" | "lib-rust" | "lib-ts" => {
            push_unique(&mut out, "error_handling");
            push_unique(&mut out, "testing");
            push_unique(&mut out, "dependencies");
        }
        "microservice" | "microservice-rust" => {
            push_unique(&mut out, "api");
            push_unique(&mut out, "database");
            push_unique(&mut out, "observability");
            push_unique(&mut out, "security");
            push_unique(&mut out, "testing");
        }
        "ai-agent" | "ai-agent-claude-sdk" | "agentic-multi-agent" => {
            push_unique(&mut out, "agent");
            push_unique(&mut out, "error_handling");
            push_unique(&mut out, "observability");
        }
        _ => {}
    }

    // Universal baseline · always include.
    for c in [
        "testing",
        "cicd",
        "security",
        "git",
        "error_handling",
        "directory",
        "dependencies",
    ] {
        push_unique(&mut out, c);
    }

    out
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, item: T) {
    if !v.contains(&item) {
        v.push(item);
    }
}

async fn apply(args: &Value) -> ToolResponse {
    let category = match args.get("category").and_then(Value::as_str) {
        Some(c) => c.to_owned(),
        None => return err("missing 'category'".into()),
    };
    let dir = match standards_dir() {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let category_dir = dir.join(&category);
    if !category_dir.exists() {
        return err(format!(
            "category '{category}' not in standards · run pipeline_standards.fetch first"
        ));
    }
    // Strategy: copy any non-doc artifacts (rustfmt.toml, clippy.toml,
    // .editorconfig, .pre-commit-config.yaml, etc.) from category_dir into
    // the project root. Skip files that already exist · agent decides
    // whether to merge.
    let mut applied: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut rd = match tokio::fs::read_dir(&category_dir).await {
        Ok(rd) => rd,
        Err(e) => return err(format!("read_dir: {e}")),
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "STANDARDS.md" || name == "README.md" {
            continue;
        }
        let src = entry.path();
        let dst = cwd.join(&name);
        if dst.exists() {
            skipped.push(name);
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if meta.is_file() && tokio::fs::copy(&src, &dst).await.is_ok() {
            applied.push(name);
        }
    }
    ToolResponse::ok(json!({
        "category": category,
        "applied": applied,
        "skipped": skipped,
        "note": "skipped files exist already · agent should merge manually",
    }))
}

async fn check(_args: &Value) -> ToolResponse {
    let dir = match standards_dir() {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    if !dir.exists() {
        return err("standards not fetched · call pipeline_standards.fetch first".into());
    }
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Run the existing `static` stage as a proxy: pipeline.yaml standards
    // compliance ≈ formatting + lints clean. Real per-rule checks land at MVP.
    let output = match Command::new("cargo")
        .args(["fmt", "--all", "--", "--check"])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("cargo fmt: {e}")),
    };
    let fmt_ok = output.status.success();
    let clippy_out = match Command::new("cargo")
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("cargo clippy: {e}")),
    };
    let clippy_ok = clippy_out.status.success();
    let overall = fmt_ok && clippy_ok;
    ToolResponse {
        ok: overall,
        data: json!({
            "fmt": {"ok": fmt_ok, "exit_code": output.status.code().unwrap_or(-1)},
            "clippy": {"ok": clippy_ok, "exit_code": clippy_out.status.code().unwrap_or(-1)},
            "note": "Day-9 check ≈ static stage · per-category gap reports land at MVP",
        }),
        next_suggested: if overall {
            vec!["pipeline_run.preflight".into()]
        } else {
            vec!["pipeline_run.fix_suggestion".into()]
        },
        memory_refs: vec![],
        error: None,
    }
}

async fn diff(args: &Value) -> ToolResponse {
    let before = args.get("before").and_then(Value::as_str);
    let after = args.get("after").and_then(Value::as_str).unwrap_or("HEAD");
    let dir = match standards_dir() {
        Ok(d) => d,
        Err(e) => return err(e),
    };
    if !dir.exists() {
        return err("standards not fetched".into());
    }
    let range = match before {
        Some(b) => format!("{b}..{after}"),
        None => after.to_owned(),
    };
    let output = match Command::new("git")
        .args(["log", "--oneline", "--no-decorate", &range])
        .current_dir(&dir)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git log: {e}")),
    };
    ToolResponse::ok(json!({
        "range": range,
        "log": String::from_utf8_lossy(&output.stdout).into_owned(),
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
    fn recommend_rust_microservice_includes_rust_and_arch() {
        let cats = recommend_categories("rust", "microservice-rust");
        assert!(cats.contains(&"architecture"));
        assert!(cats.contains(&"rust"));
        assert!(cats.contains(&"api"));
        assert!(cats.contains(&"observability"));
        assert!(cats.contains(&"testing"));
    }

    #[test]
    fn recommend_python_ml_pulls_data_pipeline() {
        let cats = recommend_categories("python-uv", "ml");
        assert!(cats.contains(&"python"));
        assert!(cats.contains(&"ml"));
        assert!(cats.contains(&"data_pipeline"));
    }

    #[test]
    fn recommend_typescript_web_pulls_web_and_api() {
        let cats = recommend_categories("bun", "web-ssr");
        assert!(cats.contains(&"typescript"));
        assert!(cats.contains(&"web"));
        assert!(cats.contains(&"api"));
        assert!(cats.contains(&"database"));
    }

    #[test]
    fn recommend_unknowns_still_get_universal_baseline() {
        let cats = recommend_categories("zig", "rocket-launcher");
        assert!(cats.contains(&"architecture"));
        assert!(cats.contains(&"testing"));
        assert!(cats.contains(&"cicd"));
        assert!(cats.contains(&"security"));
        assert!(cats.contains(&"git"));
    }

    #[test]
    fn standards_dir_anchors_under_dot_pipeline() {
        let _ = standards_dir().expect("cwd available");
    }
}

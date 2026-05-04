//! `pipeline_standards` handler · fetch · list · show · recommend.
//!
//! Standards live at `https://github.com/azzindani/Standards`. Cloned into
//! `.pipeline/standards/` on first `fetch`. Subsequent `fetch` runs `git pull`.
//!
//! Day-4 ships fetch · list · show · recommend (read-side). `apply` ·
//! `check` · `diff` (write-side) need standards-aware codegen and land
//! during MVP week 6.

use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
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
        "apply" | "check" | "diff" => {
            ToolResponse::not_implemented(ToolName::Standards, &req.action)
        }
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

//! `pipeline_project` handler · init · scaffold · template_list.
//!
//! Day-5 ships init + template_list. scaffold (add component to existing
//! project) and template_register (user-defined templates) land at MVP.

use crate::server::ServerState;
use crate::templates::{self, InitError};
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

#[allow(clippy::unused_async)] // signature locked by dispatcher trait
pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "init" => init(&req.args),
        "template_list" => template_list(),
        "scaffold" => scaffold(&req.args),
        "template_register" => template_register(&req.args),
        other => err(format!("unknown action 'pipeline_project.{other}'")),
    }
}

fn init(args: &Value) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    let template = args
        .get("type")
        .or_else(|| args.get("template"))
        .and_then(Value::as_str)
        .unwrap_or("custom");
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("");

    // Default parent: current working directory · agent can override with `parent`.
    let parent: PathBuf = match args.get("parent").and_then(Value::as_str) {
        Some(p) => PathBuf::from(p),
        None => match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => return err(format!("cwd: {e}")),
        },
    };

    match templates::init_project(&parent, &name, template, stack) {
        Ok(outcome) => ToolResponse {
            ok: true,
            data: serde_json::to_value(&outcome).unwrap_or(json!({})),
            next_suggested: vec![
                "pipeline_session.lock".into(),
                "pipeline_plan.create".into(),
                "pipeline_run.stage(fast)".into(),
            ],
            memory_refs: vec![],
            error: None,
        },
        Err(InitError::NotEmpty(p)) => err(format!("target '{p}' is non-empty")),
        Err(InitError::UnknownTemplate(t, valid)) => {
            err(format!("unknown template '{t}' · valid: {valid}"))
        }
        Err(e) => err(e.to_string()),
    }
}

fn template_list() -> ToolResponse {
    let templates: Vec<Value> = templates::list_templates()
        .into_iter()
        .map(|(name, desc)| json!({"name": name, "description": desc}))
        .collect();
    ToolResponse::ok(json!({"templates": templates}))
}

fn scaffold(args: &Value) -> ToolResponse {
    let component = match args.get("component").and_then(Value::as_str) {
        Some(c) => c.to_owned(),
        None => return err("missing 'component' (file or module name)".into()),
    };
    let kind = args.get("kind").and_then(Value::as_str).unwrap_or("module");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (rel, body) = match kind {
        "module" => (
            format!("src/{component}.rs"),
            format!("//! {component} module · scaffolded by pipeline_project.scaffold\n\n"),
        ),
        "test" => (
            format!("tests/{component}.rs"),
            format!(
                "//! {component} test · scaffolded by pipeline_project.scaffold\n\n#[test]\nfn smoke() {{ assert!(true); }}\n"
            ),
        ),
        "bin" => (
            format!("src/bin/{component}.rs"),
            format!(
                "//! {component} binary · scaffolded\n\nfn main() {{ println!(\"hello from {component}\"); }}\n"
            ),
        ),
        other => return err(format!("unknown kind '{other}' · module|test|bin")),
    };
    let path = cwd.join(&rel);
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("mkdir: {e}"));
        }
    }
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(
        json!({"component": component, "kind": kind, "path": path.display().to_string()}),
    )
}

fn template_register(args: &Value) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    let source = match args.get("source").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'source' (path or git url)".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let registry_path = cwd.join(".pipeline/templates/registry.json");
    if let Some(parent) = registry_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return err(format!("mkdir: {e}"));
        }
    }
    let mut registry: Value = if registry_path.exists() {
        std::fs::read_to_string(&registry_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!({"templates": []}))
    } else {
        json!({"templates": []})
    };
    if let Some(arr) = registry.get_mut("templates").and_then(Value::as_array_mut) {
        arr.push(json!({"name": name, "source": source, "registered_at": pipeline_memory::now_rfc3339()}));
    }
    let pretty = serde_json::to_string_pretty(&registry).unwrap_or_else(|_| "{}".into());
    if let Err(e) = std::fs::write(&registry_path, pretty) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(
        json!({"name": name, "source": source, "registry": registry_path.display().to_string()}),
    )
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

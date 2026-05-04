//! `pipeline_project` handler · init · scaffold · template_list.
//!
//! Day-5 ships init + template_list. scaffold (add component to existing
//! project) and template_register (user-defined templates) land at MVP.

use crate::server::ServerState;
use crate::templates::{self, InitError};
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

#[allow(clippy::unused_async)] // signature locked by dispatcher trait
pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "init" => init(&req.args),
        "template_list" => template_list(),
        "scaffold" | "template_register" => {
            ToolResponse::not_implemented(ToolName::Project, &req.action)
        }
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

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

//! Tool name → handler module dispatcher.

use crate::handlers;
use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use std::sync::Arc;

pub async fn call_tool(name: &str, req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    let Some(tool) = parse_tool_name(name) else {
        return ToolResponse {
            ok: false,
            data: serde_json::json!({}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some(format!("unknown tool '{name}'")),
        };
    };

    match tool {
        ToolName::Session => handlers::session::handle(req, state).await,
        ToolName::Run => handlers::run::handle(req, state).await,
        ToolName::Meta => handlers::meta::handle(req, state).await,
        ToolName::Memory => handlers::memory::handle(req, state).await,
        ToolName::Report => handlers::report::handle(req, state).await,
        // Day-2 implements 5 of 19 tools end-to-end · the other 14 surface
        // not_implemented but stay schema-listed for agent discoverability.
        other => ToolResponse::not_implemented(other, &req.action),
    }
}

fn parse_tool_name(s: &str) -> Option<ToolName> {
    match s {
        "pipeline_session" => Some(ToolName::Session),
        "pipeline_plan" => Some(ToolName::Plan),
        "pipeline_standards" => Some(ToolName::Standards),
        "pipeline_project" => Some(ToolName::Project),
        "pipeline_env" => Some(ToolName::Env),
        "pipeline_docker" => Some(ToolName::Docker),
        "pipeline_run" => Some(ToolName::Run),
        "pipeline_test" => Some(ToolName::Test),
        "pipeline_e2e" => Some(ToolName::E2e),
        "pipeline_simulate" => Some(ToolName::Simulate),
        "pipeline_deploy" => Some(ToolName::Deploy),
        "pipeline_repo" => Some(ToolName::Repo),
        "pipeline_docs" => Some(ToolName::Docs),
        "pipeline_data" => Some(ToolName::Data),
        "pipeline_observe" => Some(ToolName::Observe),
        "pipeline_security" => Some(ToolName::Security),
        "pipeline_memory" => Some(ToolName::Memory),
        "pipeline_report" => Some(ToolName::Report),
        "pipeline_meta" => Some(ToolName::Meta),
        _ => None,
    }
}

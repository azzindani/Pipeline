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

    // ! Validate before dispatch, for every transport. Clients are not obliged
    // to enforce a published schema, so this is the only place the contract
    // actually binds — an argument the handler would silently drop dies here
    // instead of surfacing later as a confidently wrong result.
    if let Some(desc) = crate::registry::descriptor_for(name) {
        if let Err(e) = desc.validate(&req.action, &req.args) {
            return ToolResponse {
                ok: false,
                data: serde_json::json!({}),
                next_suggested: vec![],
                memory_refs: vec![],
                error: Some(e),
            };
        }
        // ! A `Planned` action is refused HERE, before its handler runs.
        //
        // Centralised on purpose. Fixing 41 fabricating handlers individually
        // leaves nothing stopping the 42nd — and several are worse than useless
        // when reached: `e2e.record` spawns an interactive tool with no timeout
        // and blocks forever; `docs.publish` swallows a spawn failure into
        // success. Refusing at the boundary makes the fidelity marker
        // self-enforcing: flipping an action to `Real` is the only thing that
        // lets its handler run, so the marker cannot drift from behaviour.
        if let Some(spec) = desc.action(&req.action) {
            if spec.fidelity == crate::spec::Fidelity::Planned {
                return ToolResponse {
                    ok: false,
                    data: serde_json::json!({
                        "action": format!("{name}.{}", req.action),
                        "fidelity": "planned",
                    }),
                    next_suggested: vec![],
                    memory_refs: vec![],
                    error: Some(format!(
                        "{name}.{} is not implemented · {} · ✗ retry: this refusal is deliberate, not transient",
                        req.action, spec.summary
                    )),
                };
            }
        }
    }

    match tool {
        ToolName::Session => handlers::session::handle(req, state).await,
        ToolName::Plan => handlers::plan::handle(req, state).await,
        ToolName::Standards => handlers::standards::handle(req, state).await,
        ToolName::Project => handlers::project::handle(req, state).await,
        ToolName::Env => handlers::env::handle(req, state).await,
        ToolName::Docker => handlers::docker::handle(req, state).await,
        ToolName::Run => handlers::run::handle(req, state).await,
        ToolName::Test => handlers::test::handle(req, state).await,
        ToolName::E2e => handlers::e2e::handle(req, state).await,
        ToolName::Simulate => handlers::simulate::handle(req, state).await,
        ToolName::Deploy => handlers::deploy::handle(req, state).await,
        ToolName::Repo => handlers::repo::handle(req, state).await,
        ToolName::Docs => handlers::docs::handle(req, state).await,
        ToolName::Data => handlers::data::handle(req, state).await,
        ToolName::Observe => handlers::observe::handle(req, state).await,
        ToolName::Security => handlers::security::handle(req, state).await,
        ToolName::Memory => handlers::memory::handle(req, state).await,
        ToolName::Report => handlers::report::handle(req, state).await,
        ToolName::Meta => handlers::meta::handle(req, state).await,
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

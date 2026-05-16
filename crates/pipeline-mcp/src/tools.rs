//! Super tool envelope · 19 tools dispatch by `action` parameter.

use serde::{Deserialize, Serialize};

/// Identifier for each of the 19 super tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    Session,
    Plan,
    Standards,
    Project,
    Env,
    Docker,
    Run,
    Test,
    E2e,
    Simulate,
    Deploy,
    Repo,
    Docs,
    Data,
    Observe,
    Security,
    Memory,
    Report,
    Meta,
}

impl ToolName {
    pub const ALL: [Self; 19] = [
        Self::Session,
        Self::Plan,
        Self::Standards,
        Self::Project,
        Self::Env,
        Self::Docker,
        Self::Run,
        Self::Test,
        Self::E2e,
        Self::Simulate,
        Self::Deploy,
        Self::Repo,
        Self::Docs,
        Self::Data,
        Self::Observe,
        Self::Security,
        Self::Memory,
        Self::Report,
        Self::Meta,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "pipeline_session",
            Self::Plan => "pipeline_plan",
            Self::Standards => "pipeline_standards",
            Self::Project => "pipeline_project",
            Self::Env => "pipeline_env",
            Self::Docker => "pipeline_docker",
            Self::Run => "pipeline_run",
            Self::Test => "pipeline_test",
            Self::E2e => "pipeline_e2e",
            Self::Simulate => "pipeline_simulate",
            Self::Deploy => "pipeline_deploy",
            Self::Repo => "pipeline_repo",
            Self::Docs => "pipeline_docs",
            Self::Data => "pipeline_data",
            Self::Observe => "pipeline_observe",
            Self::Security => "pipeline_security",
            Self::Memory => "pipeline_memory",
            Self::Report => "pipeline_report",
            Self::Meta => "pipeline_meta",
        }
    }
}

/// Inbound request envelope · all 19 tools share this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolRequest {
    pub action: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Outbound response envelope · `next_suggested` closes the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResponse {
    pub ok: bool,
    pub data: serde_json::Value,
    #[serde(default)]
    pub next_suggested: Vec<String>,
    #[serde(default)]
    pub memory_refs: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl ToolResponse {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data,
            next_suggested: Vec::new(),
            memory_refs: Vec::new(),
            error: None,
        }
    }

    pub fn not_implemented(tool: ToolName, action: &str) -> Self {
        Self {
            ok: false,
            data: serde_json::json!({}),
            next_suggested: Vec::new(),
            memory_refs: Vec::new(),
            error: Some(format!(
                "{}.{action} not yet implemented · see PLAN.md milestone",
                tool.as_str()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_nineteen_tool_names() {
        assert_eq!(ToolName::ALL.len(), 19);
    }

    #[test]
    fn tool_names_are_unique() {
        let mut names: Vec<&str> = ToolName::ALL.iter().map(|t| t.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 19, "duplicate tool name");
    }

    #[test]
    fn response_serializes_with_next_suggested() {
        let r = ToolResponse::ok(serde_json::json!({"hello": "world"}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(s.contains("next_suggested"));
    }
}

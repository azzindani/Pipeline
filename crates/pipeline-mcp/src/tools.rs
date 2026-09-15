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

impl ToolRequest {
    /// Parse a `tools/call` `arguments` object · the one shape every transport shares.
    ///
    /// ! A top-level key other than `action` · `args` is an **error**, ✗ dropped. The
    /// published schema already says `additionalProperties: false`, but each transport
    /// read only the two keys it knew, so `{"action":"explain","topic":"memory"}` ran with
    /// `topic` silently discarded and reported success. Flattening per-action arguments is
    /// the commonest way a caller lands here, so the refusal says where they belong.
    ///
    /// # Errors
    /// `arguments` present but not an object · an unknown top-level key.
    pub fn from_arguments(tool: &str, arguments: &serde_json::Value) -> Result<Self, String> {
        let obj = match arguments {
            serde_json::Value::Object(m) => m,
            // Absent · dispatch then refuses the empty action and names the real ones.
            serde_json::Value::Null => {
                return Ok(Self {
                    action: String::new(),
                    args: serde_json::Value::Null,
                });
            }
            _ => {
                return Err(format!(
                    "{tool}: 'arguments' must be an object · {{\"action\": …, \"args\": {{…}}}}"
                ));
            }
        };
        if let Some(key) = obj
            .keys()
            .find(|k| !matches!(k.as_str(), "action" | "args"))
        {
            return Err(format!(
                "{tool}: unknown argument '{key}' · accepted at the top level: action · args · \
                 per-action arguments go inside 'args'"
            ));
        }
        Ok(Self {
            action: obj
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            args: obj.get("args").cloned().unwrap_or(serde_json::Value::Null),
        })
    }
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
    /// A request refused before any handler ran · the error says what to change.
    pub fn refused(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: serde_json::json!({}),
            next_suggested: Vec::new(),
            memory_refs: Vec::new(),
            error: Some(error.into()),
        }
    }

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
    fn arguments_parse_into_action_and_args() {
        let req = ToolRequest::from_arguments(
            "pipeline_meta",
            &serde_json::json!({"action": "explain", "args": {"topic": "memory"}}),
        )
        .unwrap();
        assert_eq!(req.action, "explain");
        assert_eq!(req.args, serde_json::json!({"topic": "memory"}));

        // Absent `arguments` parses to an empty action, which dispatch then refuses by name.
        let bare = ToolRequest::from_arguments("pipeline_meta", &serde_json::Value::Null).unwrap();
        assert_eq!(bare.action, "");
        assert!(bare.args.is_null());
    }

    #[test]
    fn a_flattened_argument_is_refused_by_name_not_dropped() {
        // The published schema says additionalProperties:false at the top level; this is
        // where that binds. Before, `topic` vanished and the call reported success.
        let err = ToolRequest::from_arguments(
            "pipeline_meta",
            &serde_json::json!({"action": "explain", "topic": "memory"}),
        )
        .unwrap_err();
        assert!(err.contains("unknown argument 'topic'"), "{err}");
        assert!(
            err.contains("inside 'args'"),
            "the refusal must say where the argument belongs: {err}"
        );
    }

    #[test]
    fn non_object_arguments_are_refused() {
        let err = ToolRequest::from_arguments("pipeline_meta", &serde_json::json!("explain"))
            .unwrap_err();
        assert!(err.contains("must be an object"), "{err}");
    }

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

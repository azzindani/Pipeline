//! Static tool registry · description + action list per super tool.
//!
//! Action list is inlined in each description so an agent picks the right
//! action without a second tool call.

use crate::tools::ToolName;

#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub name: ToolName,
    pub summary: &'static str,
    pub actions: &'static [&'static str],
}

impl ToolDescriptor {
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }
}

/// Build the canonical 19-tool descriptor list.
#[allow(clippy::too_many_lines, clippy::enum_glob_use)] // static registry · readability wins
pub fn registry() -> Vec<ToolDescriptor> {
    use ToolName::*;
    vec![
        ToolDescriptor {
            name: Session,
            summary: "Session lock · context · handover.",
            actions: &[
                "lock",
                "unlock",
                "steal",
                "start",
                "checkpoint",
                "end",
                "handover",
                "context",
                "file_context",
                "task_context",
                "agent_register",
            ],
        },
        ToolDescriptor {
            name: Plan,
            summary: "Idea intake · feasibility · PRD · features · milestones · ADRs · risks.",
            actions: &[
                "idea_capture",
                "link_ingest",
                "research_notes_list",
                "research_notes_show",
                "feasibility",
                "create",
                "prd_write",
                "prd_read",
                "prd_update",
                "features_add",
                "features_list",
                "features_update",
                "features_track",
                "acceptance_define",
                "milestone_create",
                "milestone_progress",
                "progress",
                "decision_log",
                "risk_add",
                "risk_list",
                "estimate",
            ],
        },
        ToolDescriptor {
            name: Standards,
            summary: "Standards fetch · select · apply · check.",
            actions: &[
                "list",
                "show",
                "recommend",
                "apply",
                "check",
                "fetch",
                "diff",
            ],
        },
        ToolDescriptor {
            name: Project,
            summary: "Project init · scaffold · templates.",
            actions: &["init", "scaffold", "template_list", "template_register"],
        },
        ToolDescriptor {
            name: Env,
            summary: "Environment · deps · runtime · tooling · secrets.",
            actions: &[
                "create",
                "deps_install",
                "deps_audit",
                "deps_update",
                "deps_lock",
                "runtime_provision",
                "tooling_install",
                "secrets_setup",
                "secrets_inject",
                "devcontainer_open",
            ],
        },
        ToolDescriptor {
            name: Docker,
            summary: "Docker · compose · image.",
            actions: &[
                "build",
                "run",
                "exec",
                "logs",
                "inspect",
                "rm",
                "compose_up",
                "compose_down",
                "compose_ps",
                "compose_logs",
                "image_scan",
                "image_promote",
                "image_push",
                "image_pull",
                "dockerfile_generate",
                "dockerfile_lint",
            ],
        },
        ToolDescriptor {
            name: Run,
            summary: "Stage execution · preflight · commit · push.",
            actions: &[
                "stage",
                "status",
                "logs",
                "fix_suggestion",
                "preflight",
                "commit",
                "push",
                "explain",
            ],
        },
        ToolDescriptor {
            name: Test,
            summary: "Test generate · run · coverage · mutation · property.",
            actions: &[
                "generate",
                "run",
                "coverage",
                "mutation_run",
                "property_generate",
                "validation_create",
                "ac_to_test",
                "flake_detect",
            ],
        },
        ToolDescriptor {
            name: E2e,
            summary: "Playwright · browser control · visual · a11y.",
            actions: &[
                "run",
                "record",
                "browser_launch",
                "browser_close",
                "trace",
                "screenshot",
                "visual_regression",
                "a11y_check",
                "against_env",
                "devtools_eval",
            ],
        },
        ToolDescriptor {
            name: Simulate,
            summary: "Persona · journey · use case · load · chaos.",
            actions: &[
                "persona_create",
                "journey_define",
                "journey_simulate",
                "use_case_define",
                "load",
                "chaos_inject",
            ],
        },
        ToolDescriptor {
            name: Deploy,
            summary: "CI/CD generate · deploy · rollback · canary · health.",
            actions: &[
                "cicd_generate",
                "target",
                "rollback",
                "smoke",
                "health",
                "release_create",
                "canary",
                "blue_green",
                "diff",
            ],
        },
        ToolDescriptor {
            name: Repo,
            summary: "Multi-repo · digest · port · compare · reverse engineer.",
            actions: &[
                "register",
                "list",
                "remove",
                "digest",
                "list_capabilities",
                "extract",
                "compare",
                "port",
                "port_validate",
                "apply_standards",
                "capability_graph",
                "re_analyze",
                "re_status",
                "re_report",
                "re_reconstruct",
                "re_modernize",
            ],
        },
        ToolDescriptor {
            name: Docs,
            summary: "Docs · changelog · diagram · spec generation.",
            actions: &[
                "generate",
                "update_from_code",
                "changelog",
                "diagram",
                "publish",
                "spec_generate",
            ],
        },
        ToolDescriptor {
            name: Data,
            summary: "DB · schema · migrate · seed · ETL · quality.",
            actions: &[
                "db_provision",
                "schema_generate",
                "schema_migrate",
                "seed",
                "etl_create",
                "quality_check",
                "db_diff",
                "anonymize",
            ],
        },
        ToolDescriptor {
            name: Observe,
            summary: "Metrics · logs · traces · perf · optimize.",
            actions: &[
                "metrics_setup",
                "logs_aggregate",
                "traces_setup",
                "alerts_define",
                "perf_baseline",
                "perf_compare",
                "optimize_suggest",
                "image_size_optimize",
                "query_optimize",
            ],
        },
        ToolDescriptor {
            name: Security,
            summary: "Secrets · vulns · audit · threat · compliance.",
            actions: &[
                "secret_scan",
                "vuln_scan",
                "dep_audit",
                "threat_model",
                "compliance_check",
            ],
        },
        ToolDescriptor {
            name: Memory,
            summary: "Remember · recall · history · patterns · export.",
            actions: &[
                "remember",
                "recall",
                "history",
                "known_issues",
                "suggest_fix",
                "pattern_report",
                "export",
                "import",
            ],
        },
        ToolDescriptor {
            name: Report,
            summary: "Dashboard · velocity · burndown · last.",
            actions: &[
                "dashboard",
                "velocity_metrics",
                "burndown",
                "last",
                "summary",
            ],
        },
        ToolDescriptor {
            name: Meta,
            summary: "Explain · config · self-check · version.",
            actions: &[
                "explain",
                "config_get",
                "config_set",
                "self_check",
                "version",
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_nineteen_tools() {
        assert_eq!(registry().len(), 19);
    }

    #[test]
    fn registry_action_count_matches_plan() {
        // PLAN.md §3.2 reports 172 total actions.
        let total: usize = registry().iter().map(ToolDescriptor::action_count).sum();
        assert_eq!(total, 172, "action count drift vs PLAN.md §3.2");
    }

    #[test]
    fn every_tool_has_at_least_one_action() {
        for t in registry() {
            assert!(t.action_count() > 0, "{} has zero actions", t.name.as_str());
        }
    }
}

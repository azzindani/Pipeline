//! Conformance · the registry's claims must match the handlers' behaviour.
//!
//! A live dogfooding audit found ~60 actions returning `ok: true` while doing
//! nothing, doing the wrong thing, or emitting fabricated data. Fixing them one
//! by one only refills — nothing stopped the next one being written. These tests
//! are what stops it.
//!
//! Four invariants, each mapped to a defect the audit actually found:
//!
//! | Invariant | Defect it prevents |
//! |---|---|
//! | declared arg → read by the handler | `deps_install` accepted `packages`, never read it, reported success |
//! | `Planned` → never `ok: true` | `re_report` overwrote status to "complete" and returned empty findings |
//! | handler action → registry entry | undocumented actions carry no fidelity, so nothing can check them |
//! | unknown arg → rejected at the boundary | a typo'd argument was indistinguishable from an ignored one |

use pipeline_mcp::{ToolRequest, call_tool, registry, spec::Fidelity};
use std::sync::Arc;

/// Handler source for a tool · `pipeline_run` → `handlers/run.rs`.
fn handler_source(tool: &str) -> String {
    let stem = tool.strip_prefix("pipeline_").expect("tool name prefix");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/handlers")
        .join(format!("{stem}.rs"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn every_declared_argument_is_actually_read_by_its_handler() {
    // ! The audit's most common defect: an argument accepted, echoed back in the
    // response, and never consulted. The caller cannot distinguish that from an
    // argument that worked — so the registry declaring it is a lie the test
    // catches. Grep-level, deliberately: it is the cheapest check that would
    // have caught `deps_install`, `compliance_check`, `repo.compare`, and
    // `metrics_setup` on the day each was written.
    let mut violations = Vec::new();
    for t in registry() {
        let src = handler_source(t.name.as_str());
        for action in t.actions {
            for arg in action.args.args() {
                let quoted = format!("\"{}\"", arg.name);
                if !src.contains(&quoted) {
                    violations.push(format!(
                        "{}.{} declares '{}' · handler never mentions it",
                        t.name.as_str(),
                        action.name,
                        arg.name
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "declared arguments no handler reads ({}):\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

#[test]
fn every_registry_action_is_dispatchable() {
    // An action listed but not matched returns "unknown action", which reads to
    // an agent as a version mismatch rather than a registry bug.
    let mut missing = Vec::new();
    for t in registry() {
        let src = handler_source(t.name.as_str());
        for action in t.actions {
            let arm = format!("\"{}\"", action.name);
            if !src.contains(&arm) {
                missing.push(format!("{}.{}", t.name.as_str(), action.name));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "registry lists actions no handler dispatches: {}",
        missing.join(" · ")
    );
}

#[test]
fn every_dispatched_action_is_in_the_registry() {
    // The reverse direction. An action reachable but undeclared carries no
    // fidelity marker and no arg schema — invisible to the agent and to every
    // other test in this file, which is exactly how the fabrications hid.
    let mut undeclared = Vec::new();
    for t in registry() {
        let src = handler_source(t.name.as_str());
        let declared: Vec<&str> = t.actions.iter().map(|a| a.name).collect();
        for line in src.lines() {
            let line = line.trim();
            // Match arms of the shape `"name" => …` or `"a" | "b" => …`.
            if !line.starts_with('"') || !line.contains("=>") {
                continue;
            }
            let head = line.split("=>").next().unwrap_or("");
            for tok in head.split('|') {
                let name = tok
                    .trim()
                    .trim_matches(|c: char| c == '"' || c.is_whitespace());
                if name.is_empty() || name.contains(' ') || !name.is_ascii() {
                    continue;
                }
                // Skip string matches that are not action dispatch (arg values).
                if !name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    continue;
                }
                if !declared.contains(&name) && is_action_arm(&src, name) {
                    undeclared.push(format!("{}.{}", t.name.as_str(), name));
                }
            }
        }
    }
    undeclared.sort_unstable();
    undeclared.dedup();
    assert!(
        undeclared.is_empty(),
        "handlers dispatch actions the registry does not declare: {}",
        undeclared.join(" · ")
    );
}

/// Heuristic: only arms inside the top-level `match req.action.as_str()` count.
/// Everything else is a value match (stack names, engines, formats).
fn is_action_arm(src: &str, name: &str) -> bool {
    let Some(start) = src.find("match req.action.as_str()") else {
        return false;
    };
    // The dispatch match ends at the first line that closes it at fn indent.
    let tail = &src[start..];
    let end = tail.find("\n}").unwrap_or(tail.len());
    tail[..end].contains(&format!("\"{name}\""))
}

#[tokio::test]
async fn a_planned_action_never_reports_success() {
    // ! The core promise. `Planned` tells the agent "this will refuse" — if one
    // returns ok:true it is fabricating, and the marker becomes worse than
    // useless because it is now trusted.
    //
    // Runs in a scratch dir so nothing touches the developer's project state.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(pipeline_mcp::ServerState::new());
    let mut liars = Vec::new();

    for t in registry() {
        for action in t.actions {
            if action.fidelity != Fidelity::Planned {
                continue;
            }
            let _guard = std::env::set_current_dir(tmp.path());
            let resp = call_tool(
                t.name.as_str(),
                ToolRequest {
                    action: action.name.to_owned(),
                    args: serde_json::json!({}),
                },
                state.clone(),
            )
            .await;
            if resp.ok {
                liars.push(format!("{}.{}", t.name.as_str(), action.name));
            } else {
                assert!(
                    resp.error.is_some(),
                    "{}.{} refused with no error message · an agent cannot act on that",
                    t.name.as_str(),
                    action.name
                );
            }
        }
    }
    assert!(
        liars.is_empty(),
        "Planned actions reporting success ({}): {}",
        liars.len(),
        liars.join(" · ")
    );
}

#[tokio::test]
async fn an_unknown_argument_is_rejected_not_ignored() {
    // The enabling condition for the whole defect class. A misspelled argument
    // must fail loudly · silently dropping it is how `packages` disappeared.
    let state = Arc::new(pipeline_mcp::ServerState::new());
    let resp = call_tool(
        "pipeline_env",
        ToolRequest {
            action: "deps_install".to_owned(),
            args: serde_json::json!({"stack": "rust", "pacakges": ["serde"]}),
        },
        state,
    )
    .await;
    assert!(!resp.ok, "a typo'd argument was accepted");
    let err = resp.error.unwrap_or_default();
    assert!(
        err.contains("pacakges"),
        "error must name the bad key: {err}"
    );
    assert!(
        err.contains("did you mean 'packages'"),
        "a near-miss should suggest the intended argument: {err}"
    );
}

#[tokio::test]
async fn a_missing_required_argument_is_named_with_its_help_text() {
    let state = Arc::new(pipeline_mcp::ServerState::new());
    let resp = call_tool(
        "pipeline_docker",
        ToolRequest {
            action: "build".to_owned(),
            args: serde_json::json!({}),
        },
        state,
    )
    .await;
    assert!(!resp.ok);
    let err = resp.error.unwrap_or_default();
    assert!(err.contains("tag"), "must name the missing arg: {err}");
    assert!(
        err.contains("name:tag"),
        "must carry the arg's help so the agent can fix it in one step: {err}"
    );
}

#[tokio::test]
async fn an_unknown_action_lists_the_known_ones() {
    let state = Arc::new(pipeline_mcp::ServerState::new());
    let resp = call_tool(
        "pipeline_run",
        ToolRequest {
            action: "stagee".to_owned(),
            args: serde_json::json!({}),
        },
        state,
    )
    .await;
    assert!(!resp.ok);
    let err = resp.error.unwrap_or_default();
    assert!(err.contains("stage"), "must offer the real actions: {err}");
}

#[test]
fn every_specified_action_publishes_a_closed_schema() {
    // `additionalProperties: false` is what makes an unknown key visible.
    for t in registry() {
        let schema = t.input_schema();
        let Some(clauses) = schema.get("allOf").and_then(|v| v.as_array()) else {
            continue;
        };
        for c in clauses {
            let args = &c["then"]["properties"]["args"];
            assert_eq!(
                args["additionalProperties"],
                serde_json::json!(false),
                "{} publishes an open schema · unknown args would pass silently",
                t.name.as_str()
            );
        }
    }
}

#[test]
fn the_published_schema_names_every_action() {
    for t in registry() {
        let schema = t.input_schema();
        let listed = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .len();
        assert_eq!(
            listed,
            t.actions.len(),
            "{} publishes {} actions but declares {}",
            t.name.as_str(),
            listed,
            t.actions.len()
        );
    }
}

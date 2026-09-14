//! Surface budget · the context tax an agent pays before it does any work.
//!
//! Every MCP client sends `tools/list` at connect and keeps the result resident
//! for the whole session. Pipeline is designed to sit alongside other servers in
//! an agent already carrying 200+ tools, so this payload is not free — it is a
//! fixed per-session cost subtracted from the working context.
//!
//! Measured at the time of writing: 19 tools · 175 actions · 82,636 bytes,
//! of which `inputSchema` is 62,311 (75%) and descriptions 20,325 (25%).
//!
//! ! The cost is driven by per-action schema clauses, ✗ by tool count. Merging
//! tools moves the same text into fewer descriptions and saves nothing, which is
//! why the ceiling below caps actions and tools separately. Crossing the action
//! cap → promote | delete Scaffold and Planned actions first, ✗ add a tool.

use pipeline_mcp::{ToolDescriptor, registry};

/// Bytes of the serialized `tools/list` result. Headroom over the measured
/// 82,636 for the ~15 actions in `docs/MATURITY.md`, ✗ open-ended growth.
const MAX_PAYLOAD_BYTES: usize = 100_000;

/// Client tool-selection accuracy degrades past ~40–50 tools in a single server.
const MAX_TOOLS: usize = 19;

/// Actions are the real cost driver. 175 today · 200 leaves room for MATURITY.
const MAX_ACTIONS: usize = 200;

fn tool_entry(t: &ToolDescriptor) -> serde_json::Value {
    serde_json::json!({
        "name": t.name.as_str(),
        "description": t.describe(),
        "inputSchema": t.input_schema(),
    })
}

fn payload_bytes() -> usize {
    registry()
        .iter()
        .map(|t| {
            serde_json::to_string(&tool_entry(t))
                .expect("serialize")
                .len()
        })
        .sum()
}

#[test]
fn tools_list_payload_stays_within_budget() {
    let bytes = payload_bytes();
    assert!(
        bytes <= MAX_PAYLOAD_BYTES,
        "tools/list payload is {bytes} bytes, over the {MAX_PAYLOAD_BYTES} budget.\n\
         This is context every agent pays at connect, before any work.\n\
         Remedy, in order: promote | delete Scaffold and Planned actions · \
         shorten action descriptions · emit permissive schemas for actions whose \
         args dispatch already validates. ✗ raise this constant to make the test pass."
    );
}

#[test]
fn tool_and_action_counts_stay_within_ceiling() {
    let tools = registry().len();
    let actions: usize = registry().iter().map(ToolDescriptor::action_count).sum();

    assert!(
        tools <= MAX_TOOLS,
        "{tools} tools, ceiling {MAX_TOOLS}. A new capability is an action on an \
         existing tool, ✗ a new tool — see docs/MATURITY.md §7."
    );
    assert!(
        actions <= MAX_ACTIONS,
        "{actions} actions, ceiling {MAX_ACTIONS}. Drop Scaffold | Planned actions \
         before adding more, ✗ add a tool."
    );
}

#[test]
fn schema_share_of_payload_is_reported() {
    // ! Not a gate — a tripwire. If descriptions ever dominate, the remedy flips
    // from "simplify schemas" to "shorten prose", and the budget test's advice
    // above would be pointing at the wrong half.
    let mut desc = 0usize;
    let mut total = 0usize;
    for t in registry() {
        let t = &t;
        let d = t.describe().len();
        desc += d;
        total += serde_json::to_string(&tool_entry(t))
            .expect("serialize")
            .len();
    }
    let schema = total - desc;
    assert!(
        schema > desc,
        "descriptions ({desc} B) now exceed schemas ({schema} B) — the budget \
         test's remedy ordering assumes the opposite. Update both together."
    );
}

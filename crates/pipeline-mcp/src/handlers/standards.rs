//! `pipeline_standards` handler · resolve · route · inject · check.
//!
//! Standards live in a SEPARATE repo. Pipeline consumes it as a versioned
//! dependency via `pipeline-standards`: resolved by cascade, pinned by SHA,
//! routed by the corpus's own ROUTER rules, injected in three tiers.
//!
//! ! This handler holds ZERO routing knowledge. Which standards bind a project
//! is decided by `index.json`, which Standards' CI emits and validates. An
//! earlier revision hardcoded a stack→category map here; it drifted out of date
//! the moment ROUTER changed. ✗ reintroduce one.

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use pipeline_standards as std_lib;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "brief" => brief().await,
        "list" => list().await,
        "show" => show(&req.args).await,
        "checklist" => checklist().await,
        "route" => route().await,
        "fetch" => fetch().await,
        "update" => update().await,
        "pin" => pin().await,
        "check" => check().await,
        other => err(format!(
            "unknown action 'pipeline_standards.{other}' · \
             try brief|list|show|checklist|route|fetch|update|pin|check"
        )),
    }
}

/// pipeline.yaml drives resolution + routing. Absent → cannot route.
fn config() -> Result<pipeline_config::PipelineConfig, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let path = cwd.join("pipeline.yaml");
    pipeline_config::PipelineConfig::load(&path)
        .map_err(|e| format!("load {}: {e}", path.display()))
}

/// resolve → index → route. `allow_clone` gates network access.
async fn load(
    allow_clone: bool,
) -> Result<
    (
        pipeline_config::PipelineConfig,
        std_lib::Index,
        std_lib::Resolved,
        std_lib::RoutedSet,
    ),
    String,
> {
    let cfg = config()?;
    let (index, resolved, routed) = std_lib::load(&cfg.standards, &cfg.stack.runtime, allow_clone)
        .await
        .map_err(|e| e.to_string())?;
    Ok((cfg, index, resolved, routed))
}

/// L0 — the packet injected at every session start.
async fn brief() -> ToolResponse {
    let (_cfg, index, resolved, routed) = match load(false).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let b = std_lib::inject::brief(&index, &routed, &resolved);

    // Drift/unpinned are advisory, not failures — the agent still gets its brief,
    // but it is told the ground may have moved.
    let mut next = vec!["pipeline_standards.show".to_owned()];
    if b.drifted || b.unpinned {
        next.insert(0, "pipeline_standards.pin".to_owned());
    }

    ToolResponse {
        ok: true,
        data: json!({
            "brief": b,
            "markdown": b.to_markdown(),
        }),
        next_suggested: next,
        memory_refs: vec![],
        error: None,
    }
}

/// Full catalog — every standard in the corpus, bound or not.
async fn list() -> ToolResponse {
    let (_cfg, index, resolved, routed) = match load(false).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let catalog: Vec<Value> = index
        .standards
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "tier": s.tier,
                "version": s.version,
                "path": s.path,
                "owns": s.owns,
                "lines": s.lines,
                "checklist_items": s.checklist.len(),
                "bound": routed.ids.contains(&s.id),
            })
        })
        .collect();

    ToolResponse::ok(json!({
        "sha": resolved.sha,
        "root": resolved.root.display().to_string(),
        "total": catalog.len(),
        "bound": routed.ids.len(),
        "standards": catalog,
    }))
}

/// L1 — one standard, in full. Accepts any id incl. splits (`testing/pressure`).
async fn show(args: &Value) -> ToolResponse {
    let id = match args
        .get("id")
        .or_else(|| args.get("category"))
        .and_then(Value::as_str)
    {
        Some(c) => c,
        None => return err("missing 'id' · e.g. rust | testing/pressure".into()),
    };
    let (_cfg, index, resolved, _routed) = match load(false).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let content = match std_lib::inject::doc(&index, &resolved.root, id).await {
        Ok(c) => c,
        Err(e) => return err(e.to_string()),
    };
    let Some(s) = index.get(id) else {
        return err(format!("unknown standard '{id}'"));
    };

    ToolResponse::ok(json!({
        "id": s.id,
        "tier": s.tier,
        "version": s.version,
        "path": s.path,
        "owns": s.owns,
        "defers_to": s.defers_to,
        "load_with": s.load_with,
        "sha": resolved.sha,
        "content": content,
    }))
}

/// L2 — the enforcement surface: every checklist item the routed set imposes.
async fn checklist() -> ToolResponse {
    let (_cfg, index, resolved, routed) = match load(false).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let lists = std_lib::inject::checklists(&index, &routed);
    let total: usize = lists.iter().map(|c| c.items.len()).sum();

    ToolResponse::ok(json!({
        "sha": resolved.sha,
        "standards": lists.len(),
        "total_items": total,
        "checklists": lists,
    }))
}

/// Why each standard binds — provenance, decisions, unknowns.
async fn route() -> ToolResponse {
    let (cfg, _index, resolved, routed) = match load(false).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    ToolResponse::ok(json!({
        "runtime": cfg.stack.runtime,
        "project_type": cfg.standards.project_type,
        "surfaces": cfg.standards.surfaces,
        "sha": resolved.sha,
        "routed": routed,
    }))
}

/// Populate the shared cache — the only action permitted to touch the network.
async fn fetch() -> ToolResponse {
    let (_cfg, index, resolved, routed) = match load(true).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    ToolResponse {
        ok: true,
        data: json!({
            "root": resolved.root.display().to_string(),
            "origin": format!("{:?}", resolved.origin).to_lowercase(),
            "sha": resolved.sha,
            "catalog": index.standards.len(),
            "bound": routed.ids.len(),
        }),
        next_suggested: vec![
            "pipeline_standards.brief".into(),
            "pipeline_standards.pin".into(),
        ],
        memory_refs: vec![],
        error: None,
    }
}

/// Move the cache to upstream HEAD, then report which BOUND standards changed.
/// ! Refuses a user-owned source — Pipeline never mutates your clone.
async fn update() -> ToolResponse {
    let (_cfg, index, resolved, routed) = match load(true).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let old = resolved.sha.clone();

    let new_sha = match std_lib::resolve::update(&resolved).await {
        Ok(s) => s,
        Err(e) => return err(e.to_string()),
    };

    // Only the routed set matters — a change to `ml` is noise for a Rust CLI.
    let paths: Vec<String> = routed
        .ids
        .iter()
        .filter_map(|id| index.get(id).map(|s| s.path.clone()))
        .collect();
    let commits = std_lib::resolve::changed_since(&resolved.root, &old, &paths)
        .await
        .unwrap_or_default();

    ToolResponse {
        ok: true,
        data: json!({
            "from": old,
            "to": new_sha,
            "changed_bound_standards": commits.len(),
            "commits": commits,
            "note": "pin still points at the old SHA · call pipeline_standards.pin to accept",
        }),
        next_suggested: vec![
            "pipeline_standards.pin".into(),
            "pipeline_standards.check".into(),
        ],
        memory_refs: vec![],
        error: None,
    }
}

/// Write the resolved SHA into pipeline.yaml — an explicit, reviewable act.
async fn pin() -> ToolResponse {
    let (_cfg, _index, resolved, _routed) = match load(false).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    match write_pin(&cwd.join("pipeline.yaml"), &resolved.sha).await {
        Ok(prev) => ToolResponse::ok(json!({
            "previous": prev,
            "pin": resolved.sha,
            "file": "pipeline.yaml",
        })),
        Err(e) => err(e),
    }
}

/// Surgical YAML edit — rewrites `standards.pin`, leaving comments + order intact.
/// A serde round-trip would reformat the file and drop every comment.
///
/// ! The pin must land on the standards block's last CONTENT line — not at EOF.
/// Trailing blank lines and unrelated top-level comments follow many blocks, and
/// appending past them strands the key under a comment it has nothing to do with.
async fn write_pin(path: &Path, sha: &str) -> Result<Option<String>, String> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("read {}: {e}", path.display()))?;

    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let indent_of = |l: &str| l.len() - l.trim_start().len();

    let Some(start) = lines
        .iter()
        .position(|l| indent_of(l) == 0 && l.trim().starts_with("standards:"))
    else {
        // No standards block → append a fresh one.
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("standards:".to_owned());
        lines.push(format!("  pin: {sha}"));
        return finish(path, lines, None).await;
    };

    // Walk the block. It ends at the next top-level key (a comment does not end it).
    let mut last_content = start;
    let mut child_indent = 2;
    let mut existing = None;

    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim();
        let indent = indent_of(line);

        if indent == 0 && !trimmed.is_empty() && !trimmed.starts_with('#') {
            break; // next top-level key — block is over
        }
        if trimmed.is_empty() || indent == 0 {
            continue; // blank line or a top-level comment — not block content
        }

        if last_content == start {
            child_indent = indent; // adopt the block's own indentation
        }
        last_content = i;

        if trimmed.starts_with("pin:") {
            existing = Some((i, trimmed.trim_start_matches("pin:").trim().to_owned()));
        }
    }

    let pad = " ".repeat(child_indent);
    if let Some((i, prev)) = existing {
        lines[i] = format!("{pad}pin: {sha}");
        return finish(path, lines, Some(prev).filter(|p| !p.is_empty())).await;
    }
    lines.insert(last_content + 1, format!("{pad}pin: {sha}"));
    finish(path, lines, None).await
}

async fn finish(
    path: &Path,
    lines: Vec<String>,
    previous: Option<String>,
) -> Result<Option<String>, String> {
    let joined = lines.join("\n") + "\n";
    // ! never write a pipeline.yaml we can no longer parse
    pipeline_config::PipelineConfig::parse(&joined)
        .map_err(|e| format!("refusing to write · result would not parse: {e}"))?;
    tokio::fs::write(path, joined)
        .await
        .map_err(|e| format!("write {}: {e}", path.display()))?;

    Ok(previous.filter(|p| !p.is_empty()))
}

/// Compliance gate — the routed set's obligations, plus pin health.
///
/// Checklist items are prose obligations; scoring them semantically is agent
/// work, not regex work. `check` returns the obligations plus the mechanical
/// signals it CAN evaluate, and the agent adjudicates. It ✗ report a score it
/// did not compute — the old implementation ran `cargo fmt` and called that
/// "standards compliance".
async fn check() -> ToolResponse {
    let (_cfg, index, resolved, routed) = match load(false).await {
        Ok(v) => v,
        Err(e) => return err(e),
    };

    let lists = std_lib::inject::checklists(&index, &routed);
    let total: usize = lists.iter().map(|c| c.items.len()).sum();

    let mut blocking: Vec<String> = Vec::new();
    if resolved.is_drifted() {
        blocking.push(format!(
            "standards drift · pinned {} but corpus is at {} · gates may have moved",
            resolved.pin.as_deref().unwrap_or("?"),
            resolved.short_sha()
        ));
    }
    if resolved.is_unpinned() {
        blocking.push("no standards.pin in pipeline.yaml · gates are unversioned".to_owned());
    }
    for d in &routed.decisions {
        blocking.push(format!(
            "unresolved route choice {:?} from {} · set standards.languages",
            d.options, d.from
        ));
    }
    for u in &routed.unknown_routes {
        blocking.push(format!("unknown route key · {u}"));
    }

    let ok = blocking.is_empty();
    ToolResponse {
        ok,
        data: json!({
            "sha": resolved.sha,
            "bound_standards": routed.ids.len(),
            "obligations": total,
            "blocking": blocking,
            "checklists": lists,
            "adjudication": "checklist items are prose obligations · agent scores them against the codebase",
        }),
        next_suggested: if ok {
            vec!["pipeline_run.preflight".into()]
        } else {
            vec![
                "pipeline_standards.pin".into(),
                "pipeline_standards.route".into(),
            ]
        },
        memory_refs: vec![],
        error: None,
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

    const YAML: &str = "\
# leading comment
project: pipeline
version: 0.0.1

stack:
  runtime: rust

standards:
  source: /root/Standards   # local clone
  project_type: MCP server
";

    async fn pin_roundtrip(input: &str, sha: &str) -> (String, Option<String>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pipeline.yaml");
        tokio::fs::write(&path, input).await.expect("write");
        let prev = write_pin(&path, sha).await.expect("write_pin");
        let out = tokio::fs::read_to_string(&path).await.expect("read");
        (out, prev)
    }

    #[tokio::test]
    async fn pin_inserts_into_existing_block_preserving_comments() {
        let (out, prev) = pin_roundtrip(YAML, "abc1234").await;
        assert_eq!(prev, None);
        assert!(out.contains("  pin: abc1234"));
        // ! comments + key order survive — a serde round-trip would eat them
        assert!(out.contains("# leading comment"));
        assert!(out.contains("# local clone"));

        let cfg = pipeline_config::PipelineConfig::parse(&out).expect("parses");
        assert_eq!(cfg.standards.pin.as_deref(), Some("abc1234"));
        assert_eq!(cfg.standards.source.as_deref(), Some("/root/Standards"));
        assert_eq!(cfg.standards.project_type.as_deref(), Some("MCP server"));
    }

    #[tokio::test]
    async fn pin_replaces_existing_and_reports_the_old_one() {
        let first = pin_roundtrip(YAML, "aaaaaaa").await.0;
        let (out, prev) = pin_roundtrip(&first, "bbbbbbb").await;
        assert_eq!(prev.as_deref(), Some("aaaaaaa"));
        assert_eq!(out.matches("pin:").count(), 1, "must not duplicate the key");
        assert_eq!(
            pipeline_config::PipelineConfig::parse(&out)
                .expect("parses")
                .standards
                .pin
                .as_deref(),
            Some("bbbbbbb")
        );
    }

    #[tokio::test]
    async fn pin_appends_a_standards_block_when_none_exists() {
        let bare = "project: p\nversion: 0.0.1\nstack:\n  runtime: rust\n";
        let (out, prev) = pin_roundtrip(bare, "ccc1234").await;
        assert_eq!(prev, None);
        assert_eq!(
            pipeline_config::PipelineConfig::parse(&out)
                .expect("parses")
                .standards
                .pin
                .as_deref(),
            Some("ccc1234")
        );
    }

    /// Regression: the pin used to be appended at EOF, stranding it *after* the
    /// trailing comment and outside its block. It must attach to the block's last
    /// content line.
    #[tokio::test]
    async fn pin_does_not_strand_itself_after_a_trailing_comment() {
        let trailing = "\
project: p
version: 0.0.1
stack:
  runtime: rust
standards:
  source: /root/Standards
  surfaces:
    - Command line

# deploy + maintenance wire in at v1 · see PLAN.md
";
        let (out, _) = pin_roundtrip(trailing, "eee1234").await;
        let lines: Vec<&str> = out.lines().collect();

        let pin_at = lines.iter().position(|l| l.contains("pin:")).expect("pin");
        let comment_at = lines
            .iter()
            .position(|l| l.starts_with("# deploy"))
            .expect("comment");
        assert!(
            pin_at < comment_at,
            "pin must sit inside its block, not after the trailing comment:\n{out}"
        );
        // ...directly after the last content line of the block.
        assert_eq!(lines[pin_at - 1].trim(), "- Command line");
        assert_eq!(lines[pin_at], "  pin: eee1234", "sibling indentation");

        let cfg = pipeline_config::PipelineConfig::parse(&out).expect("parses");
        assert_eq!(cfg.standards.pin.as_deref(), Some("eee1234"));
        assert_eq!(cfg.standards.surfaces, vec!["Command line"]);
    }

    #[tokio::test]
    async fn pin_lands_inside_a_mid_file_standards_block() {
        // ! the pin must not leak into the following top-level key
        let mid = "\
project: p
version: 0.0.1
stack:
  runtime: rust
standards:
  source: /root/Standards
gates:
  coverage: 70
";
        let (out, _) = pin_roundtrip(mid, "ddd1234").await;
        let cfg = pipeline_config::PipelineConfig::parse(&out).expect("parses");
        assert_eq!(cfg.standards.pin.as_deref(), Some("ddd1234"));
        assert_eq!(cfg.gates.coverage, Some(70), "gates block survived intact");
    }
}

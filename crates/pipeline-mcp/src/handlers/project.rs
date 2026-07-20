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
    // adopt · bring an existing repo under Pipeline instead of scaffolding a new
    // one. Writes only the missing files · ✗ overwrites anything already there.
    let adopt = args
        .get("adopt")
        .and_then(Value::as_bool)
        .unwrap_or_default();

    // Default parent: current working directory · agent can override with `parent`.
    let parent: PathBuf = match args.get("parent").and_then(Value::as_str) {
        Some(p) => PathBuf::from(p),
        None => match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => return err(format!("cwd: {e}")),
        },
    };

    match templates::init_project_with(&parent, &name, template, stack, adopt) {
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
        Err(InitError::NotEmpty(p)) => err(format!(
            "target '{p}' is non-empty · pass adopt=true to bring an existing \
             project under Pipeline (writes only what is missing)"
        )),
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
        // A workspace member · the unit of layering for any project big enough to
        // need one. Emitting the manifest + lib.rs is not enough on its own: an
        // unregistered crate is invisible to cargo, so `crate` also edits the
        // workspace members list (see add_workspace_member).
        "crate" => return scaffold_crate(&cwd, &component, args),
        other => return err(format!("unknown kind '{other}' · module|test|bin|crate")),
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

/// Scaffold a workspace member crate under `crates/<name>/`.
///
/// Writes the manifest + lib.rs (or main.rs for a bin) and registers the crate in
/// the workspace `members` list. ! Registration matters: a crate cargo has not been
/// told about builds fine in isolation and is silently absent from
/// `--workspace` — so it never reaches the gate.
fn scaffold_crate(cwd: &std::path::Path, name: &str, args: &Value) -> ToolResponse {
    let root = cwd.join("crates").join(name);
    if root.exists() {
        return err(format!("refusing to overwrite {}", root.display()));
    }
    let is_bin = args.get("bin").and_then(Value::as_bool).unwrap_or(false);
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Scaffolded by pipeline_project.scaffold.");
    // Inherit from the workspace so version/edition/lints stay in one place.
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion.workspace = true\nedition.workspace = true\n\
         rust-version.workspace = true\ndescription = \"{description}\"\n\n[dependencies]\n\n\
         [lints]\nworkspace = true\n"
    );
    let (src_rel, src_body) = if is_bin {
        (
            "src/main.rs",
            format!("//! {name} · {description}\n\nfn main() {{\n    println!(\"{name}\");\n}}\n"),
        )
    } else {
        ("src/lib.rs", format!("//! {name} · {description}\n"))
    };

    if let Err(e) = std::fs::create_dir_all(root.join("src")) {
        return err(format!("mkdir: {e}"));
    }
    if let Err(e) = std::fs::write(root.join("Cargo.toml"), manifest) {
        return err(format!("write manifest: {e}"));
    }
    if let Err(e) = std::fs::write(root.join(src_rel), src_body) {
        return err(format!("write {src_rel}: {e}"));
    }

    let registered = match add_workspace_member(cwd, name) {
        Ok(r) => r,
        Err(e) => return err(e),
    };

    ToolResponse {
        ok: true,
        data: json!({
            "crate": name,
            "root": root.display().to_string(),
            "manifest": root.join("Cargo.toml").display().to_string(),
            "source": root.join(src_rel).display().to_string(),
            "workspace_registered": registered,
        }),
        next_suggested: vec!["pipeline_run.stage(fast)".into()],
        memory_refs: vec![],
        error: None,
    }
}

/// Add `crates/<name>` to the root manifest's `[workspace] members`.
///
/// Line-oriented edit, ✗ a toml round-trip: the root manifest carries comments
/// and ordering a serializer would discard. Returns false when the member was
/// already listed (idempotent) or no workspace table exists.
fn add_workspace_member(cwd: &std::path::Path, name: &str) -> Result<bool, String> {
    use std::fmt::Write as _;

    let path = cwd.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(false); // no root manifest · standalone crate, nothing to register
    };
    let entry = format!("crates/{name}");
    if text.contains(&format!("\"{entry}\"")) {
        return Ok(true);
    }
    let mut out = String::with_capacity(text.len() + entry.len() + 8);
    let mut in_members = false;
    let mut done = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if !done && trimmed.starts_with("members") && trimmed.contains('[') {
            in_members = true;
            out.push_str(line);
            out.push('\n');
            // Single-line members = [...] · splice before the closing bracket.
            if trimmed.contains(']') {
                let spliced = out
                    .trim_end()
                    .rsplit_once(']')
                    .map(|(head, tail)| format!("{head}, \"{entry}\"]{tail}\n"));
                if let Some(s) = spliced {
                    out.truncate(out.trim_end().len() - trimmed.len());
                    out.push_str(s.trim_start_matches('\n'));
                }
                in_members = false;
                done = true;
            }
            continue;
        }
        if in_members && trimmed == "]" {
            let _ = writeln!(out, "    \"{entry}\",");
            in_members = false;
            done = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !done {
        // No [workspace] table yet · the project was scaffolded single-package.
        // Create one rather than silently declining to register: the first
        // `scaffold crate` call is exactly the moment a project becomes a workspace.
        // A workspace root may also be a package, so an existing [package] stays.
        let mut seeded = text.clone();
        if !seeded.ends_with('\n') {
            seeded.push('\n');
        }
        let _ = write!(
            seeded,
            "\n[workspace]\nresolver = \"3\"\nmembers = [\n    \"{entry}\",\n]\n"
        );
        std::fs::write(&path, seeded).map_err(|e| format!("write workspace manifest: {e}"))?;
        return Ok(true);
    }
    std::fs::write(&path, out).map_err(|e| format!("write workspace manifest: {e}"))?;
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const WS: &str = "[workspace]\nresolver = \"3\"\n# a comment that must survive\nmembers = [\n    \"crates/alpha\",\n]\n\n[workspace.package]\nversion = \"0.1.0\"\n";

    #[test]
    fn registers_a_new_member_and_keeps_comments() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), WS).unwrap();
        assert!(add_workspace_member(dir.path(), "beta").unwrap());
        let out = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert!(out.contains("\"crates/alpha\""));
        assert!(out.contains("\"crates/beta\""));
        assert!(out.contains("# a comment that must survive"));
        assert!(out.contains("[workspace.package]"));
    }

    #[test]
    fn registering_twice_is_idempotent() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), WS).unwrap();
        add_workspace_member(dir.path(), "beta").unwrap();
        let once = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        add_workspace_member(dir.path(), "beta").unwrap();
        let twice = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn handles_a_single_line_members_list() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/alpha\"]\n",
        )
        .unwrap();
        assert!(add_workspace_member(dir.path(), "beta").unwrap());
        let out = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert!(out.contains("\"crates/alpha\", \"crates/beta\""), "{out}");
    }

    #[test]
    fn seeds_a_workspace_table_when_the_project_was_single_package() {
        // The first `scaffold crate` call is the moment a project becomes a
        // workspace · declining to register would leave the crate invisible.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"vera\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert!(add_workspace_member(dir.path(), "vera-core").unwrap());
        let out = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert!(out.contains("[workspace]"));
        assert!(out.contains("\"crates/vera-core\""));
        assert!(
            out.contains("[package]"),
            "existing package table must survive"
        );
        // …and a second crate joins the table just created.
        assert!(add_workspace_member(dir.path(), "vera-store").unwrap());
        let out = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
        assert!(out.contains("\"crates/vera-core\""));
        assert!(out.contains("\"crates/vera-store\""));
    }

    #[test]
    fn no_root_manifest_is_not_an_error() {
        let dir = tempdir().unwrap();
        assert!(!add_workspace_member(dir.path(), "beta").unwrap());
    }
}

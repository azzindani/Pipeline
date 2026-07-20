//! `pipeline_project` handler · init · scaffold · templates.
//!
//! Three loops close here:
//! - `init` instantiates built-in **and** user-registered templates
//! - `scaffold` reads the project's stack before deciding what file to emit
//! - `template_register` validates its source, so a registered template resolves

use crate::server::ServerState;
use crate::templates::{self, InitError, RegisteredTemplate};
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "init" => init(&req.args).await,
        "template_list" => template_list(),
        "scaffold" => scaffold(&req.args),
        "template_register" => template_register(&req.args).await,
        other => err(format!("unknown action 'pipeline_project.{other}'")),
    }
}

async fn init(args: &Value) -> ToolResponse {
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

    // A registered template is instantiated for real · it is only reachable here
    // because built-ins win the name, so the lookup runs after that check.
    if !template.is_empty() && !templates::is_builtin(template) {
        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => return err(format!("cwd: {e}")),
        };
        if let Some(reg) = templates::find_registered(&cwd, template) {
            return init_registered(&cwd, &parent, &name, &reg, stack, adopt).await;
        }
    }

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
        Err(InitError::UnknownTemplate(t, valid)) => err(format!(
            "unknown template '{t}' · built-in: {valid} · register your own with \
             pipeline_project.template_register"
        )),
        Err(e) => err(e.to_string()),
    }
}

/// Materialize a registered template, then hand the local directory to templates.
///
/// ! Git work stays here, async and time-bounded · a clone that hangs would wedge
/// the MCP server for every other tool call.
async fn init_registered(
    cwd: &Path,
    parent: &Path,
    name: &str,
    reg: &RegisteredTemplate,
    stack: &str,
    adopt: bool,
) -> ToolResponse {
    let source_dir = if reg.kind == "git" {
        let cache = templates::registry_path(cwd)
            .with_file_name("cache")
            .join(&reg.name);
        // Re-clone rather than pull · a stale cache silently instantiates an old
        // template, and "which revision did I get" must have one answer.
        if cache.exists() {
            if let Err(e) = std::fs::remove_dir_all(&cache) {
                return err(format!("clear template cache: {e}"));
            }
        }
        if let Err(e) = clone_shallow(&reg.source, &cache).await {
            return err(e);
        }
        cache
    } else {
        PathBuf::from(&reg.source)
    };

    match templates::instantiate_registered(parent, name, &reg.name, &source_dir, stack, adopt) {
        Ok(outcome) => ToolResponse {
            ok: true,
            data: json!({
                "outcome": serde_json::to_value(&outcome).unwrap_or(json!({})),
                "template_origin": "registered",
                "source": reg.source,
            }),
            next_suggested: vec![
                "pipeline_session.lock".into(),
                "pipeline_run.stage(fast)".into(),
            ],
            memory_refs: vec![],
            error: None,
        },
        Err(InitError::NotEmpty(p)) => err(format!(
            "target '{p}' is non-empty · pass adopt=true to bring an existing \
             project under Pipeline (writes only what is missing)"
        )),
        Err(e) => err(e.to_string()),
    }
}

async fn clone_shallow(source: &str, dest: &Path) -> Result<(), String> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("mkdir template cache: {e}"))?;
    }
    let run = Command::new("git")
        .args(["clone", "--depth", "1", source])
        .arg(dest)
        // ✗ block on a credential prompt · an MCP server has no terminal to answer it.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match tokio::time::timeout(Duration::from_secs(180), run).await {
        Err(_) => Err(format!("git clone '{source}' timed out after 180s")),
        Ok(Err(e)) => Err(format!("git spawn: {e} · is git installed?")),
        Ok(Ok(o)) if o.status.success() => Ok(()),
        Ok(Ok(o)) => Err(format!(
            "git clone '{source}' exit {}: {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
    }
}

/// Built-ins **and** registered templates · `origin` says which, because the two
/// differ in everything that matters: who maintains it and whether it can move.
fn template_list() -> ToolResponse {
    let mut out: Vec<Value> = templates::list_templates()
        .into_iter()
        .map(|(name, desc)| json!({"name": name, "description": desc, "origin": "builtin"}))
        .collect();
    if let Ok(cwd) = std::env::current_dir() {
        for t in templates::load_registry(&cwd) {
            out.push(json!({
                "name": t.name,
                "description": format!("registered {} template · {}", t.kind, t.source),
                "origin": "registered",
                "source": t.source,
                "kind": t.kind,
                "registered_at": t.registered_at,
            }));
        }
    }
    ToolResponse::ok(json!({"templates": out}))
}

/// Where a component lands · and what must be edited so the toolchain sees it.
#[derive(Debug)]
struct Plan {
    rel: String,
    body: String,
    register: Register,
}

/// What a language needs before a new file is actually part of the build.
///
/// ! The old scaffold wrote the file and stopped. A bare `src/x.rs` is not
/// compiled, not linted, and never reaches the gate — so the agent was handed
/// `ok:true` for a file the toolchain cannot see.
#[derive(Debug)]
enum Register {
    /// Discovered by path · cargo `tests/` · cargo `src/bin/` · ES imports · go dirs.
    Autoloaded,
    /// Rust · `mod x;` in the crate root, else the module is dead weight.
    RustMod {
        root_rel: &'static str,
        module: String,
        public: bool,
    },
    /// Python · the directory needs `__init__.py` to be an importable package.
    PythonPackage { dir: String },
}

/// The project facts scaffolding depends on · read, ✗ assumed.
struct Ctx {
    stack: String,
    project: Option<String>,
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
    // A workspace member · the unit of layering for any project big enough to
    // need one. Emitting the manifest + lib.rs is not enough on its own: an
    // unregistered crate is invisible to cargo, so `crate` also edits the
    // workspace members list (see add_workspace_member).
    if kind == "crate" {
        return scaffold_crate(&cwd, &component, args);
    }
    // ! Stack matters. The old code wrote `src/<c>.rs` into python and bun projects
    // and reported success · nothing on those stacks ever reads a .rs file.
    let ctx = match project_ctx(args) {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    if let Err(e) = validate_component(&ctx.stack, &component) {
        return err(e);
    }
    let plan = match plan_component(&cwd, &ctx, kind, &component) {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    match emit(&cwd, &plan) {
        Ok(registered) => ToolResponse {
            ok: true,
            data: json!({
                "component": component,
                "kind": kind,
                "stack": ctx.stack,
                "path": cwd.join(&plan.rel).display().to_string(),
                "registered": registered,
            }),
            next_suggested: vec!["pipeline_run.stage(fast)".into()],
            memory_refs: vec![],
            error: None,
        },
        Err(e) => err(e),
    }
}

/// Write the planned file, then perform the language's registration step.
fn emit(cwd: &Path, plan: &Plan) -> Result<Value, String> {
    let path = cwd.join(&plan.rel);
    if path.exists() {
        return Err(format!("refusing to overwrite {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(&path, &plan.body).map_err(|e| format!("write: {e}"))?;
    apply_register(cwd, &plan.register).map_err(|e| {
        format!(
            "wrote {} but could not register it: {e} · the file will not build \
             until it is declared",
            path.display()
        )
    })
}

fn apply_register(cwd: &Path, register: &Register) -> Result<Value, String> {
    match register {
        Register::Autoloaded => Ok(json!({"needed": false, "reason": "discovered by path"})),
        Register::RustMod {
            root_rel,
            module,
            public,
        } => {
            let decl = add_rust_mod(&cwd.join(root_rel), module, *public)?;
            Ok(json!({"needed": true, "file": root_rel, "declaration": decl}))
        }
        Register::PythonPackage { dir } => {
            let marker = cwd.join(dir).join("__init__.py");
            let created = if marker.exists() {
                false
            } else {
                std::fs::write(&marker, "").map_err(|e| format!("write __init__.py: {e}"))?;
                true
            };
            Ok(json!({
                "needed": true,
                "file": marker.display().to_string(),
                "created": created,
            }))
        }
    }
}

/// Stack from the `stack` argument, else from `pipeline.yaml` · ✗ guessed.
fn project_ctx(args: &Value) -> Result<Ctx, String> {
    let cfg = super::load_config_in_cwd();
    if let Some(s) = args.get("stack").and_then(Value::as_str) {
        return Ok(Ctx {
            stack: s.to_owned(),
            project: cfg.ok().map(|c| c.project),
        });
    }
    match cfg {
        Ok(c) => Ok(Ctx {
            stack: c.stack.runtime,
            project: Some(c.project),
        }),
        Err(e) => Err(format!(
            "cannot determine stack · {e} · pass stack=rust|python-uv|bun|node|go, \
             or run pipeline_project.init first"
        )),
    }
}

/// Reject names that cannot be an identifier on the target stack.
///
/// ! A path separator would silently place the file somewhere the registration
/// step does not look, recreating the orphan-module defect one level down.
fn validate_component(stack: &str, component: &str) -> Result<(), String> {
    if component.is_empty() || component.contains(['/', '\\', '.', ' ']) {
        return Err(format!(
            "invalid component '{component}' · a bare name, ✗ a path or extension"
        ));
    }
    let rusty = matches!(stack, "rust") || matches!(stack, "go" | "golang");
    if rusty && component.contains('-') {
        return Err(format!(
            "invalid component '{component}' for {stack} · '-' is not legal in an \
             identifier · use '{}'",
            component.replace('-', "_")
        ));
    }
    Ok(())
}

fn plan_component(cwd: &Path, ctx: &Ctx, kind: &str, c: &str) -> Result<Plan, String> {
    match ctx.stack.as_str() {
        "rust" => plan_rust(cwd, kind, c),
        "python" | "python-uv" | "uv" => plan_python(cwd, ctx, kind, c),
        "bun" | "node" | "ts" | "typescript" => plan_js(cwd, &ctx.stack, kind, c),
        "go" | "golang" => plan_go(cwd, kind, c),
        // ✗ fall back to Rust. Emitting a .rs file into a stack that cannot read it
        // is the exact failure this refusal exists to prevent.
        other => Err(format!(
            "unsupported stack '{other}' · scaffold knows rust | python-uv | bun | \
             node | go · ✗ emitting a Rust file into a '{other}' project"
        )),
    }
}

fn plan_rust(cwd: &Path, kind: &str, c: &str) -> Result<Plan, String> {
    match kind {
        "module" => {
            // ! No crate root → no place to declare the module → the file would be
            // an orphan. Refuse and say what to do instead.
            let (root_rel, public) = rust_crate_root(cwd).ok_or_else(|| {
                "no src/lib.rs or src/main.rs to declare `mod` in · a bare src/*.rs is \
                 never compiled · use kind=crate for a workspace member"
                    .to_owned()
            })?;
            Ok(Plan {
                rel: format!("src/{c}.rs"),
                body: format!("//! `{c}` module · scaffolded by pipeline_project.scaffold\n"),
                register: Register::RustMod {
                    root_rel,
                    module: c.to_owned(),
                    public,
                },
            })
        }
        // cargo auto-discovers tests/*.rs and src/bin/*.rs · ✗ manifest edit needed.
        "test" => Ok(Plan {
            rel: format!("tests/{c}.rs"),
            body: format!(
                "//! `{c}` integration test · scaffolded by pipeline_project.scaffold\n\n\
                 #[test]\nfn placeholder() {{\n    assert_eq!(2 + 2, 4);\n}}\n"
            ),
            register: Register::Autoloaded,
        }),
        "bin" => Ok(Plan {
            rel: format!("src/bin/{c}.rs"),
            body: format!(
                "//! `{c}` binary · scaffolded by pipeline_project.scaffold\n\n\
                 fn main() {{\n    println!(\"{c}\");\n}}\n"
            ),
            register: Register::Autoloaded,
        }),
        other => Err(unknown_kind(other)),
    }
}

fn plan_python(cwd: &Path, ctx: &Ctx, kind: &str, c: &str) -> Result<Plan, String> {
    // pytest discovers tests/test_*.py · nothing to register.
    if kind == "test" {
        return Ok(Plan {
            rel: format!("tests/test_{c}.py"),
            body: format!(
                "\"\"\"Tests for {c} · scaffolded by pipeline_project.scaffold.\"\"\"\n\n\n\
                 def test_placeholder() -> None:\n    assert 2 + 2 == 4\n"
            ),
            register: Register::Autoloaded,
        });
    }
    let body = match kind {
        "module" => format!("\"\"\"{c} · scaffolded by pipeline_project.scaffold.\"\"\"\n"),
        // Runnable as `python -m <pkg>.<name>` · a real entry point, ✗ a console
        // script: that would need a pyproject edit this action does not claim.
        "bin" => format!(
            "\"\"\"{c} entry point · scaffolded by pipeline_project.scaffold.\"\"\"\n\n\n\
             def main() -> None:\n    print(\"{c}\")\n\n\n\
             if __name__ == \"__main__\":\n    main()\n"
        ),
        other => return Err(unknown_kind(other)),
    };
    match python_package(cwd, ctx) {
        Some(dir) => Ok(Plan {
            rel: format!("{dir}/{c}.py"),
            body,
            register: Register::PythonPackage { dir },
        }),
        // Flat repo · a root-level module is importable as-is, ✗ needs __init__.py.
        None => Ok(Plan {
            rel: format!("{c}.py"),
            body,
            register: Register::Autoloaded,
        }),
    }
}

fn plan_js(cwd: &Path, stack: &str, kind: &str, c: &str) -> Result<Plan, String> {
    // bun is TypeScript-native · a node project declares TS by shipping tsconfig.
    let ts = stack != "node" || cwd.join("tsconfig.json").exists();
    let ext = if ts { "ts" } else { "js" };
    let runner = if stack == "bun" { "bun" } else { "node" };
    // ES modules resolve by import path · nothing registers a file anywhere.
    match kind {
        "module" => Ok(Plan {
            rel: format!("src/{c}.{ext}"),
            body: format!("// `{c}` · scaffolded by pipeline_project.scaffold\n\nexport {{}};\n"),
            register: Register::Autoloaded,
        }),
        "test" => Ok(Plan {
            rel: format!("tests/{c}.test.{ext}"),
            body: js_test_body(runner, c),
            register: Register::Autoloaded,
        }),
        "bin" => Ok(Plan {
            rel: format!("src/bin/{c}.{ext}"),
            body: format!(
                "#!/usr/bin/env {runner}\n// `{c}` · scaffolded by pipeline_project.scaffold\n\n\
                 console.log(\"{c}\");\n"
            ),
            register: Register::Autoloaded,
        }),
        other => Err(unknown_kind(other)),
    }
}

fn js_test_body(runner: &str, c: &str) -> String {
    if runner == "bun" {
        format!(
            "import {{ expect, test }} from \"bun:test\";\n\n\
             test(\"{c} placeholder\", () => {{\n  expect(2 + 2).toBe(4);\n}});\n"
        )
    } else {
        format!(
            "import assert from \"node:assert/strict\";\nimport test from \"node:test\";\n\n\
             test(\"{c} placeholder\", () => {{\n  assert.equal(2 + 2, 4);\n}});\n"
        )
    }
}

fn plan_go(cwd: &Path, kind: &str, c: &str) -> Result<Plan, String> {
    // go discovers packages by directory · ✗ registration step exists.
    match kind {
        "module" => Ok(Plan {
            rel: format!("internal/{c}/{c}.go"),
            body: format!(
                "// Package {c} · scaffolded by pipeline_project.scaffold.\npackage {c}\n"
            ),
            register: Register::Autoloaded,
        }),
        // ! A directory holding only _test.go fails `go test ./...` with "no non-test
        // Go files" · refuse instead of handing back a package that cannot build.
        "test" => {
            let dir = cwd.join("internal").join(c);
            if !dir.is_dir() {
                return Err(format!(
                    "no package at internal/{c} · scaffold kind=module first · a \
                     directory with only _test.go files does not compile"
                ));
            }
            Ok(Plan {
                rel: format!("internal/{c}/{c}_test.go"),
                body: format!(
                    "package {c}\n\nimport \"testing\"\n\n\
                     func TestPlaceholder(t *testing.T) {{\n\tif 2+2 != 4 {{\n\t\t\
                     t.Fatal(\"arithmetic\")\n\t}}\n}}\n"
                ),
                register: Register::Autoloaded,
            })
        }
        "bin" => Ok(Plan {
            rel: format!("cmd/{c}/main.go"),
            body: format!(
                "// Command {c} · scaffolded by pipeline_project.scaffold.\npackage main\n\n\
                 import \"fmt\"\n\nfunc main() {{\n\tfmt.Println(\"{c}\")\n}}\n"
            ),
            register: Register::Autoloaded,
        }),
        other => Err(unknown_kind(other)),
    }
}

fn unknown_kind(kind: &str) -> String {
    format!("unknown kind '{kind}' · module|test|bin|crate")
}

/// The file a `mod` declaration has to land in · lib API is public, a bin's is not.
fn rust_crate_root(cwd: &Path) -> Option<(&'static str, bool)> {
    if cwd.join("src/lib.rs").exists() {
        return Some(("src/lib.rs", true));
    }
    if cwd.join("src/main.rs").exists() {
        return Some(("src/main.rs", false));
    }
    None
}

/// Directories that hold `__init__.py` but are ✗ the project's import package.
const NOT_A_PACKAGE: &[&str] = &["tests", "test", "docs", "examples", "scripts", "build"];

/// Locate the package a new python module belongs in.
///
/// src-layout (`src/<pkg>/__init__.py`) · flat layout (`<pkg>/__init__.py`) ·
/// otherwise `None`, meaning a root-level module, which is importable as-is.
fn python_package(cwd: &Path, ctx: &Ctx) -> Option<String> {
    for base in ["src", "."] {
        let Ok(rd) = std::fs::read_dir(cwd.join(base)) else {
            continue;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().join("__init__.py").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !NOT_A_PACKAGE.contains(&n.as_str()) && !n.starts_with('.'))
            .collect();
        names.sort(); // ! read_dir order is filesystem-defined · pick deterministically
        if let Some(n) = names.first() {
            return Some(if base == "src" {
                format!("src/{n}")
            } else {
                n.clone()
            });
        }
    }
    // A src/ directory means src-layout was intended · create the package rather
    // than dropping an orphan module next to it.
    if cwd.join("src").is_dir() {
        return ctx
            .project
            .as_ref()
            .map(|p| format!("src/{}", p.replace('-', "_")));
    }
    None
}

/// Declare `mod <name>;` in the crate root · returns the declaration written.
///
/// ! Without this the module file exists and is never compiled. Line-oriented,
/// ✗ a syn round-trip: the crate root carries comments and attribute order that
/// a parse-and-print would rewrite.
fn add_rust_mod(path: &Path, module: &str, public: bool) -> Result<String, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let decl = if public {
        format!("pub mod {module};")
    } else {
        format!("mod {module};")
    };
    let already = text.lines().any(|l| {
        let t = l.trim();
        t == decl
            || t == format!("mod {module};")
            || t == format!("pub mod {module};")
            || t == format!("pub(crate) mod {module};")
    });
    if already {
        return Ok(decl); // idempotent · already visible to the compiler
    }
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let header = mod_header_end(&lines);
    let at = mod_insert_point(&lines);
    lines.insert(at, decl.clone());
    // Landing straight after the `//!` header · keep the blank line that separated
    // the header from the code, so the result stays rustfmt-clean.
    if at == header && lines.get(at + 1).is_some_and(|l| !l.trim().is_empty()) {
        lines.insert(at + 1, String::new());
    }
    std::fs::write(path, lines.join("\n") + "\n")
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(decl)
}

/// After the last existing top-level `mod` declaration · else after the header.
fn mod_insert_point(lines: &[String]) -> usize {
    let is_mod = |l: &String| {
        let t = l.trim();
        // Top-level only · a `mod` nested inside a function is indented and ✗ a
        // place a new sibling module may be declared.
        !l.starts_with(char::is_whitespace)
            && t.ends_with(';')
            && (t.starts_with("mod ") || t.contains(" mod "))
    };
    lines
        .iter()
        .rposition(is_mod)
        .map_or_else(|| mod_header_end(lines), |i| i + 1)
}

/// Index just past the leading `//!` docs and `#![...]` inner attributes.
fn mod_header_end(lines: &[String]) -> usize {
    lines
        .iter()
        .position(|l| {
            let t = l.trim();
            !(t.is_empty() || t.starts_with("//!") || t.starts_with("#!["))
        })
        .unwrap_or(lines.len())
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
    // ✗ inherit a table the root does not define · cargo refuses to load the whole
    // workspace if a member points at a missing [workspace.lints]. An existing
    // workspace that never configured lints is a normal case, not an error.
    let root_manifest = std::fs::read_to_string(cwd.join("Cargo.toml")).unwrap_or_default();
    let has_ws_lints = root_manifest.contains("[workspace.lints")
        || root_manifest.trim().is_empty()  // absent → we are about to seed it
        || !root_manifest.contains("[workspace]");
    let lints = if has_ws_lints {
        "\n[lints]\nworkspace = true\n"
    } else {
        ""
    };
    // Inherit from the workspace so version/edition/lints stay in one place.
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion.workspace = true\nedition.workspace = true\n\
         rust-version.workspace = true\ndescription = \"{description}\"\n\n[dependencies]\n{lints}"
    );
    // ! The description belongs in the manifest, ✗ in a `//!` doc comment.
    // Free prose in doc position trips clippy::doc_markdown on any CamelCase word
    // (a product name like OpenRouter is enough), so scaffold → run fast came back
    // red by construction. A scaffold must pass the gate it hands you.
    let (src_rel, src_body) = if is_bin {
        (
            "src/main.rs",
            format!("//! `{name}`\n\nfn main() {{\n    println!(\"{name}\");\n}}\n"),
        )
    } else {
        ("src/lib.rs", format!("//! `{name}`\n"))
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
        // ! Scaffolded members use `edition.workspace = true`, so the table they
        // inherit from has to exist or cargo cannot even load the manifest.
        // Values are lifted from the existing [package], ✗ invented.
        let inherit = [
            "version",
            "edition",
            "rust-version",
            "license",
            "repository",
        ];
        let carried: Vec<String> = inherit
            .iter()
            .filter_map(|k| package_key(&text, k).map(|v| format!("{k} = {v}")))
            .collect();
        if !carried.is_empty() {
            let _ = write!(seeded, "\n[workspace.package]\n{}\n", carried.join("\n"));
        }
        // Members declare `[lints] workspace = true`, which likewise needs a table
        // to point at. Defaults follow rust/STANDARDS: deny unsafe, pedantic on.
        seeded.push_str(WORKSPACE_LINTS);
        std::fs::write(&path, seeded).map_err(|e| format!("write workspace manifest: {e}"))?;
        return Ok(true);
    }
    std::fs::write(&path, out).map_err(|e| format!("write workspace manifest: {e}"))?;
    Ok(true)
}

/// Lint policy seeded into a new workspace · rust/STANDARDS defaults.
const WORKSPACE_LINTS: &str = "\n[workspace.lints.rust]\nunsafe_code = \"forbid\"\n\n\
     [workspace.lints.clippy]\npedantic = { level = \"warn\", priority = -1 }\n";

/// Read `key = value` from the manifest's `[package]` table · returns the raw
/// right-hand side (quotes intact) so it can be re-emitted verbatim.
fn package_key(manifest: &str, key: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

/// Register a user template · validated at registration, instantiable at init.
///
/// ! The source is checked here, ✗ at init. An unreachable URL discovered during
/// `init` surfaces after the agent already committed to the template, and the old
/// implementation never checked at all — it stored any string and reported success.
async fn template_register(args: &Value) -> ToolResponse {
    let name = match args.get("name").and_then(Value::as_str) {
        Some(n) => n.to_owned(),
        None => return err("missing 'name'".into()),
    };
    let source = match args.get("source").and_then(Value::as_str) {
        Some(s) => s.to_owned(),
        None => return err("missing 'source' (path or git url)".into()),
    };
    if templates::is_builtin(&name) {
        return err(format!(
            "'{name}' is a built-in template · built-ins win at init, so this entry \
             could never be used · pick another name"
        ));
    }
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let kind = match validate_source(&source).await {
        Ok(k) => k,
        Err(e) => return err(e),
    };
    let entry = RegisteredTemplate {
        name: name.clone(),
        source: source.clone(),
        kind: kind.clone(),
        registered_at: pipeline_memory::now_rfc3339(),
    };
    let replaced = match templates::upsert_registered(&cwd, entry) {
        Ok(r) => r,
        Err(e) => return err(e.to_string()),
    };
    ToolResponse {
        ok: true,
        data: json!({
            "name": name,
            "source": source,
            "kind": kind,
            "replaced": replaced,
            "registry": templates::registry_path(&cwd).display().to_string(),
        }),
        next_suggested: vec![
            "pipeline_project.template_list".into(),
            format!("pipeline_project.init(type={name})"),
        ],
        memory_refs: vec![],
        error: None,
    }
}

/// Classify + prove the source resolves · returns `path` | `git`.
async fn validate_source(source: &str) -> Result<String, String> {
    if !looks_like_git(source) {
        let p = Path::new(source);
        if !p.exists() {
            return Err(format!("source path '{source}' does not exist"));
        }
        if !p.is_dir() {
            return Err(format!(
                "source path '{source}' is not a directory · a template is a tree"
            ));
        }
        return Ok("path".to_owned());
    }
    let run = Command::new("git")
        .args(["ls-remote", "--exit-code", source, "HEAD"])
        // ✗ block on a credential prompt · an MCP server has no terminal to answer it.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match tokio::time::timeout(Duration::from_secs(30), run).await {
        Err(_) => Err(format!(
            "git ls-remote '{source}' timed out after 30s · unreachable or needs credentials"
        )),
        Ok(Err(e)) => Err(format!("git spawn: {e} · git is required for a git source")),
        Ok(Ok(o)) if o.status.success() => Ok("git".to_owned()),
        Ok(Ok(o)) => Err(format!(
            "git ls-remote '{source}' exit {} · {}",
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
    }
}

fn looks_like_git(source: &str) -> bool {
    Path::new(source)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("git"))
        || ["http://", "https://", "git@", "ssh://", "git://"]
            .iter()
            .any(|p| source.starts_with(p))
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
            "[package]\nname = \"vera\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
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

#[cfg(test)]
mod scaffold_tests {
    use super::*;
    use tempfile::tempdir;

    fn ctx(stack: &str) -> Ctx {
        Ctx {
            stack: stack.to_owned(),
            project: Some("vera".to_owned()),
        }
    }

    fn ext_of(rel: &str) -> &str {
        rel.rsplit('.').next().unwrap_or("")
    }

    #[test]
    fn a_module_is_registered_so_the_compiler_sees_it() {
        // ! The whole point. A file at src/widget.rs that no `mod` declares is not
        // compiled, not linted, and never reaches the gate — yet scaffold used to
        // write it and report ok:true.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "//! vera core\n#![forbid(unsafe_code)]\n\npub mod alpha;\n\npub fn go() {}\n",
        )
        .unwrap();

        let plan = plan_component(dir.path(), &ctx("rust"), "module", "widget").expect("plan");
        assert_eq!(plan.rel, "src/widget.rs");
        emit(dir.path(), &plan).expect("emit");

        let root = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(root.contains("pub mod widget;"), "not declared:\n{root}");
        assert!(root.contains("pub mod alpha;"), "existing mod lost");
        assert!(root.contains("//! vera core"), "header lost");
        assert!(root.contains("pub fn go()"), "code lost");
        // Declared next to its siblings, ✗ stranded after the code.
        let lines: Vec<&str> = root.lines().collect();
        let widget = lines.iter().position(|l| l.contains("mod widget")).unwrap();
        let go = lines.iter().position(|l| l.contains("fn go")).unwrap();
        assert!(widget < go, "declaration must precede the code:\n{root}");
    }

    #[test]
    fn registering_the_same_module_twice_is_idempotent() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let root = dir.path().join("src/lib.rs");
        std::fs::write(&root, "//! c\n\npub mod widget;\n").unwrap();
        add_rust_mod(&root, "widget", true).unwrap();
        let text = std::fs::read_to_string(&root).unwrap();
        assert_eq!(text.matches("mod widget;").count(), 1, "{text}");
    }

    #[test]
    fn a_binary_crate_gets_a_private_mod_declaration() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let plan = plan_component(dir.path(), &ctx("rust"), "module", "widget").expect("plan");
        emit(dir.path(), &plan).expect("emit");
        let root = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
        assert!(root.contains("mod widget;"), "{root}");
        assert!(!root.contains("pub mod widget;"), "a bin has no public API");
    }

    #[test]
    fn a_module_with_nowhere_to_be_declared_is_refused_not_orphaned() {
        // A workspace root has no src/ · writing src/widget.rs there produces a file
        // cargo never reads. Refuse and name the alternative.
        let dir = tempdir().unwrap();
        let e = plan_component(dir.path(), &ctx("rust"), "module", "widget").expect_err("refuse");
        assert!(e.contains("kind=crate"), "{e}");
    }

    #[test]
    fn scaffold_refuses_rather_than_writing_rust_into_a_python_project() {
        // Regression: the layout was hardcoded, so every stack got src/<c>.rs and
        // ok:true — a file nothing on that stack compiles, imports, or runs.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/vera")).unwrap();
        std::fs::write(dir.path().join("src/vera/__init__.py"), "").unwrap();

        let plan = plan_component(dir.path(), &ctx("python-uv"), "module", "auth").expect("plan");
        assert_eq!(plan.rel, "src/vera/auth.py");
        assert_ne!(ext_of(&plan.rel), "rs", "a Rust file in a python project");

        // …and a stack scaffold does not understand is refused by name, ✗ silently
        // handed a Rust file.
        let e = plan_component(dir.path(), &ctx("elixir"), "module", "auth").expect_err("refuse");
        assert!(e.contains("elixir"), "must name the stack: {e}");
        assert!(e.contains("unsupported"), "{e}");
    }

    #[test]
    fn each_stack_gets_its_own_extension_and_layout() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "//! c\n").unwrap();
        let cases: &[(&str, &str, &str)] = &[
            ("rust", "module", "rs"),
            ("rust", "test", "rs"),
            ("python-uv", "module", "py"),
            ("python-uv", "test", "py"),
            ("bun", "module", "ts"),
            ("bun", "test", "ts"),
            ("node", "module", "js"),
            ("go", "module", "go"),
            ("go", "bin", "go"),
        ];
        for (stack, kind, ext) in cases {
            let plan = plan_component(dir.path(), &ctx(stack), kind, "auth")
                .unwrap_or_else(|e| panic!("{stack}/{kind}: {e}"));
            assert_eq!(
                &ext_of(&plan.rel),
                ext,
                "{stack}/{kind} produced {}",
                plan.rel
            );
        }
    }

    #[test]
    fn a_node_project_with_a_tsconfig_gets_typescript() {
        let dir = tempdir().unwrap();
        let js = plan_component(dir.path(), &ctx("node"), "module", "auth").expect("plan");
        assert_eq!(ext_of(&js.rel), "js", "{}", js.rel);
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        let ts = plan_component(dir.path(), &ctx("node"), "module", "auth").expect("plan");
        assert_eq!(ext_of(&ts.rel), "ts", "{}", ts.rel);
    }

    #[test]
    fn a_python_module_lands_in_the_package_that_exists() {
        let dir = tempdir().unwrap();
        // Flat layout · tests/ carries __init__.py too and must not be mistaken for
        // the project package.
        std::fs::create_dir_all(dir.path().join("vera")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("vera/__init__.py"), "").unwrap();
        std::fs::write(dir.path().join("tests/__init__.py"), "").unwrap();
        let plan = plan_component(dir.path(), &ctx("python-uv"), "module", "auth").expect("plan");
        assert_eq!(plan.rel, "vera/auth.py");

        // No package at all · a root-level module is importable as-is.
        let bare = tempdir().unwrap();
        let plan = plan_component(bare.path(), &ctx("python-uv"), "module", "auth").expect("plan");
        assert_eq!(plan.rel, "auth.py");
    }

    #[test]
    fn a_new_python_package_gets_its_init_file() {
        // src/ exists but holds no package · src-layout was intended, so create it.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let plan = plan_component(dir.path(), &ctx("python-uv"), "module", "auth").expect("plan");
        assert_eq!(plan.rel, "src/vera/auth.py");
        emit(dir.path(), &plan).expect("emit");
        assert!(
            dir.path().join("src/vera/__init__.py").is_file(),
            "package marker missing · the module would not be importable"
        );
    }

    #[test]
    fn a_go_test_without_its_package_is_refused() {
        // ! A directory holding only _test.go fails `go test ./...`.
        let dir = tempdir().unwrap();
        let e = plan_component(dir.path(), &ctx("go"), "test", "auth").expect_err("refuse");
        assert!(e.contains("kind=module"), "{e}");
        std::fs::create_dir_all(dir.path().join("internal/auth")).unwrap();
        let plan = plan_component(dir.path(), &ctx("go"), "test", "auth").expect("plan");
        assert_eq!(plan.rel, "internal/auth/auth_test.go");
    }

    #[test]
    fn a_name_that_cannot_be_an_identifier_is_refused() {
        assert!(validate_component("rust", "my-mod").is_err(), "hyphen");
        assert!(validate_component("go", "my-pkg").is_err(), "hyphen");
        assert!(validate_component("rust", "a/b").is_err(), "path");
        assert!(validate_component("rust", "auth.rs").is_err(), "extension");
        // Hyphens are ordinary in JS/TS filenames.
        assert!(validate_component("bun", "my-mod").is_ok());
        assert!(validate_component("rust", "auth_v2").is_ok());
    }

    #[test]
    fn scaffold_never_overwrites_an_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "//! c\n").unwrap();
        std::fs::write(dir.path().join("src/widget.rs"), "// mine\n").unwrap();
        let plan = plan_component(dir.path(), &ctx("rust"), "module", "widget").expect("plan");
        let e = emit(dir.path(), &plan).expect_err("refuse");
        assert!(e.contains("refusing to overwrite"), "{e}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/widget.rs")).unwrap(),
            "// mine\n"
        );
    }

    #[test]
    fn scaffolded_rust_passes_the_gate_it_hands_you() {
        // A scaffold that fails `pipeline run fast` on the first call teaches the
        // agent to distrust the gate · that includes the edited crate root.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "//! vera\n\npub fn go() {}\n",
        )
        .unwrap();
        for kind in ["module", "test", "bin"] {
            let plan = plan_component(dir.path(), &ctx("rust"), kind, &format!("m_{kind}"))
                .unwrap_or_else(|e| panic!("{kind}: {e}"));
            emit(dir.path(), &plan).unwrap_or_else(|e| panic!("{kind}: {e}"));
        }
        for rel in [
            "src/lib.rs",
            "src/m_module.rs",
            "tests/m_test.rs",
            "src/bin/m_bin.rs",
        ] {
            let out = std::process::Command::new("rustfmt")
                .args(["--edition", "2024", "--check"])
                .arg(dir.path().join(rel))
                .output()
                .expect("rustfmt must be installed");
            assert!(
                out.status.success(),
                "{rel} is not rustfmt-clean:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }

    #[test]
    fn a_git_source_is_told_apart_from_a_path() {
        assert!(looks_like_git("https://github.com/org/tpl"));
        assert!(looks_like_git("git@github.com:org/tpl.git"));
        assert!(looks_like_git("/srv/templates/tpl.git"));
        assert!(!looks_like_git("/srv/templates/tpl"));
        assert!(!looks_like_git("./tpl"));
    }
}

//! Project templates · scaffold a fresh project rooted at a given path.
//!
//! Day-5 templates (rust-first since Pipeline itself is Rust):
//! - `cli-rust`         · single binary · clap · Cargo.toml · Dockerfile
//! - `lib-rust`         · single library crate
//! - `microservice-rust`· axum stub + Dockerfile + docker-compose.yml
//! - `mcp-server-rust`  · pipeline-mcp-style server stub
//! - `custom`           · pipeline.yaml + .gitignore + README only
//!
//! Other stacks (python · ts · go) currently fall through to `custom`
//! with a `stack: <stack>` recorded in pipeline.yaml. Real templates land
//! at MVP week 5.

#![allow(clippy::doc_markdown)]

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("project root '{0}' already exists and is not empty")]
    NotEmpty(String),
    #[error("unknown template '{0}' · valid: {1}")]
    UnknownTemplate(String, String),
    #[error("template registry: {0}")]
    Registry(String),
    #[error("template source unusable: {0}")]
    Source(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitOutcome {
    pub name: String,
    pub template: String,
    pub stack: String,
    pub root: PathBuf,
    pub files_written: Vec<PathBuf>,
    /// Adopt mode only · files that already existed and were left untouched.
    pub files_skipped: Vec<PathBuf>,
    pub adopted: bool,
}

/// Write accumulator · carries the one policy bit that separates the two modes.
///
/// ```text
/// scaffold (adopt = false) → root must be empty · every file is written
/// adopt    (adopt = true)  → root may hold a live repo · existing files are
///                            recorded and skipped, ✗ overwritten
/// ```
struct Scaffold {
    adopt: bool,
    written: Vec<PathBuf>,
    skipped: Vec<PathBuf>,
}

impl Scaffold {
    fn new(adopt: bool) -> Self {
        Self {
            adopt,
            written: Vec::new(),
            skipped: Vec::new(),
        }
    }

    fn push(&mut self, path: PathBuf) {
        self.written.push(path);
    }
}

/// Templates available at scaffold time. Stable order · agents iterate.
pub const TEMPLATES: &[(&str, &str)] = &[
    ("cli-rust", "Single-binary CLI in Rust · clap-based"),
    ("lib-rust", "Rust library crate"),
    ("microservice-rust", "Rust HTTP microservice · axum stub"),
    (
        "mcp-server-rust",
        "Rust MCP server scaffold · stdio transport",
    ),
    (
        "custom",
        "Generic scaffold · just pipeline.yaml + .gitignore + README",
    ),
];

pub fn list_templates() -> Vec<(&'static str, &'static str)> {
    TEMPLATES.to_vec()
}

/// Scaffold a project at `parent.join(name)` using the given template.
/// Errors if the directory exists and is non-empty.
pub fn init_project(
    parent: &Path,
    name: &str,
    template: &str,
    stack: &str,
) -> Result<InitOutcome, InitError> {
    init_project_with(parent, name, template, stack, false)
}

/// Scaffold, or **adopt** an existing project.
///
/// Adopt exists because greenfield is the rare case: a real project usually
/// already has a git history, docs, and source before Pipeline ever sees it.
/// Adopt writes only what is missing — pipeline.yaml, the CI lane, the
/// scaffold gaps — and ✗ touches a file that is already there.
pub fn init_project_with(
    parent: &Path,
    name: &str,
    template: &str,
    stack: &str,
    adopt: bool,
) -> Result<InitOutcome, InitError> {
    let root = prepare_root(parent, name, adopt)?;
    let template = template_or_default(template)?;
    let stack = if stack.is_empty() {
        infer_stack(template)
    } else {
        stack.to_owned()
    };

    let mut sc = Scaffold::new(adopt);

    write(&root, ".gitignore", GITIGNORE, &mut sc)?;
    write(&root, "README.md", &readme(name, template), &mut sc)?;
    write(
        &root,
        "pipeline.yaml",
        &pipeline_yaml(name, &stack, template),
        &mut sc,
    )?;

    match template {
        "cli-rust" => scaffold_cli_rust(&root, name, &mut sc)?,
        "lib-rust" => scaffold_lib_rust(&root, name, &mut sc)?,
        "microservice-rust" => scaffold_microservice_rust(&root, name, &mut sc)?,
        "mcp-server-rust" => scaffold_mcp_server_rust(&root, name, &mut sc)?,
        "custom" => {} // base files already written
        _ => unreachable!("template_or_default narrows to known set"),
    }

    Ok(InitOutcome {
        name: name.to_owned(),
        template: template.to_owned(),
        stack,
        root,
        files_written: sc.written,
        files_skipped: sc.skipped,
        adopted: adopt,
    })
}

// ---------- user-registered templates ----------

/// A template the user registered · persisted in `.pipeline/templates/registry.json`.
///
/// `kind` is decided once, at registration, by the reachability check — ✗ re-sniffed
/// at init. A source that was never validated is a template that fails at the moment
/// the agent has already committed to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredTemplate {
    pub name: String,
    /// Local directory | git URL, exactly as registered.
    pub source: String,
    /// `path` | `git`.
    pub kind: String,
    pub registered_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    templates: Vec<RegisteredTemplate>,
}

pub fn registry_path(cwd: &Path) -> PathBuf {
    cwd.join(".pipeline")
        .join("templates")
        .join("registry.json")
}

/// Read the registry · a missing or corrupt file reads as empty, ✗ as an error:
/// the built-in templates must stay usable when the user registry is broken.
pub fn load_registry(cwd: &Path) -> Vec<RegisteredTemplate> {
    let Ok(text) = std::fs::read_to_string(registry_path(cwd)) else {
        return Vec::new();
    };
    serde_json::from_str::<RegistryFile>(&text)
        .map(|f| f.templates)
        .unwrap_or_default()
}

/// Insert or replace by name · returns true when an existing entry was replaced.
///
/// ! Upsert, ✗ append. The old implementation appended with no dedup, so two
/// entries could share a name and nothing defined which one `init` would use.
pub fn upsert_registered(cwd: &Path, entry: RegisteredTemplate) -> Result<bool, InitError> {
    let mut all = load_registry(cwd);
    let replaced = all.iter().any(|t| t.name == entry.name);
    all.retain(|t| t.name != entry.name);
    all.push(entry);
    all.sort_by(|a, b| a.name.cmp(&b.name));
    let path = registry_path(cwd);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let body = serde_json::to_string_pretty(&RegistryFile { templates: all })
        .map_err(|e| InitError::Registry(e.to_string()))?;
    std::fs::write(&path, body)?;
    Ok(replaced)
}

pub fn find_registered(cwd: &Path, name: &str) -> Option<RegisteredTemplate> {
    load_registry(cwd).into_iter().find(|t| t.name == name)
}

/// ! Built-ins win at `init` (see `template_or_default`), so a registered template
/// shadowing one would silently never be used. Registration refuses the collision.
pub fn is_builtin(name: &str) -> bool {
    let lower = name.to_lowercase();
    TEMPLATES.iter().any(|(n, _)| *n == lower)
}

/// Never copied out of a template · build output and VCS/tool state belong to the
/// source checkout, ✗ to the project being created from it.
const COPY_SKIP: &[&str] = &[
    ".git",
    ".pipeline",
    "target",
    "node_modules",
    ".venv",
    "dist",
    "build",
    "__pycache__",
];

/// Instantiate a registered template from an already-materialized local directory.
///
/// Git sources are cloned by the caller · this function is pure filesystem work so
/// the network path stays async and bounded in the handler.
pub fn instantiate_registered(
    parent: &Path,
    name: &str,
    template: &str,
    source_dir: &Path,
    stack: &str,
    adopt: bool,
) -> Result<InitOutcome, InitError> {
    if !source_dir.is_dir() {
        return Err(InitError::Source(format!(
            "'{}' is not a directory",
            source_dir.display()
        )));
    }
    let root = prepare_root(parent, name, adopt)?;
    // ! The template's own files always win · Pipeline fills the gaps afterwards.
    // Scaffold runs in adopt mode regardless of the caller's flag: overwriting a
    // template's Dockerfile with a generic one defeats the point of the template.
    let mut sc = Scaffold::new(true);
    copy_tree(source_dir, &root, &mut sc)?;

    let stack = if stack.is_empty() {
        "unknown".to_owned()
    } else {
        stack.to_owned()
    };
    write(&root, ".gitignore", GITIGNORE, &mut sc)?;
    write(&root, "README.md", &readme(name, template), &mut sc)?;
    write(
        &root,
        "pipeline.yaml",
        &pipeline_yaml(name, &stack, "custom"),
        &mut sc,
    )?;
    // ! A copied pipeline.yaml carries the TEMPLATE's project name · every memory
    // row, report, and lock keys off `project`, so leaving it is silent corruption.
    set_project_name(&root.join("pipeline.yaml"), name)?;

    Ok(InitOutcome {
        name: name.to_owned(),
        template: template.to_owned(),
        stack,
        root,
        files_written: sc.written,
        files_skipped: sc.skipped,
        adopted: adopt,
    })
}

fn copy_tree(src: &Path, dst: &Path, sc: &mut Scaffold) -> Result<(), InitError> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_name = entry.file_name();
        if COPY_SKIP.contains(&file_name.to_string_lossy().as_ref()) {
            continue;
        }
        let to = dst.join(&file_name);
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_tree(&entry.path(), &to, sc)?;
        } else if sc.adopt && to.exists() {
            sc.skipped.push(to);
        } else {
            std::fs::copy(entry.path(), &to)?;
            sc.push(to);
        }
    }
    Ok(())
}

/// Rewrite the top-level `project:` key · surgical, so template comments survive.
fn set_project_name(path: &Path, name: &str) -> Result<(), InitError> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(()); // no pipeline.yaml · nothing to rename
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    match lines.iter_mut().find(|l| l.starts_with("project:")) {
        Some(line) => *line = format!("project: {name}"),
        None => lines.insert(0, format!("project: {name}")),
    }
    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}

/// Create the project root, enforcing the empty-unless-adopt rule.
fn prepare_root(parent: &Path, name: &str, adopt: bool) -> Result<PathBuf, InitError> {
    let root = parent.join(name);
    if root.exists() {
        let mut rd = std::fs::read_dir(&root)?;
        if rd.next().is_some() && !adopt {
            return Err(InitError::NotEmpty(root.display().to_string()));
        }
    } else {
        std::fs::create_dir_all(&root)?;
    }
    Ok(root)
}

// ---------- per-template scaffolding ----------

fn scaffold_cli_rust(root: &Path, name: &str, sc: &mut Scaffold) -> Result<(), InitError> {
    write(
        root,
        "Cargo.toml",
        &cargo_bin_manifest(
            name,
            &["clap = { version = \"4\", features = [\"derive\"] }"],
        ),
        sc,
    )?;
    std::fs::create_dir_all(root.join("src"))?;
    write(root, "src/main.rs", &cli_rust_main(name), sc)?;
    write(root, "Dockerfile", &rust_dockerfile(name), sc)?;
    Ok(())
}

fn scaffold_lib_rust(root: &Path, name: &str, sc: &mut Scaffold) -> Result<(), InitError> {
    write(root, "Cargo.toml", &cargo_lib_manifest(name), sc)?;
    std::fs::create_dir_all(root.join("src"))?;
    write(
        root,
        "src/lib.rs",
        "//! Crate documentation lives here.\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it_works() {\n        assert_eq!(2 + 2, 4);\n    }\n}\n",
        sc,
    )?;
    Ok(())
}

fn scaffold_microservice_rust(root: &Path, name: &str, sc: &mut Scaffold) -> Result<(), InitError> {
    write(
        root,
        "Cargo.toml",
        &cargo_bin_manifest(
            name,
            &[
                "tokio = { version = \"1\", features = [\"macros\", \"rt-multi-thread\"] }",
                "axum = \"0.7\"",
                "serde = { version = \"1\", features = [\"derive\"] }",
                "serde_json = \"1\"",
            ],
        ),
        sc,
    )?;
    std::fs::create_dir_all(root.join("src"))?;
    write(root, "src/main.rs", MICROSERVICE_MAIN, sc)?;
    write(root, "Dockerfile", &rust_dockerfile(name), sc)?;
    write(
        root,
        "docker-compose.yml",
        &format!(
            "services:\n  {name}:\n    build: .\n    ports:\n      - \"8080:8080\"\n    healthcheck:\n      test: [\"CMD\", \"curl\", \"-f\", \"http://localhost:8080/health\"]\n      interval: 5s\n      timeout: 2s\n      retries: 5\n"
        ),
        sc,
    )?;
    Ok(())
}

fn scaffold_mcp_server_rust(root: &Path, name: &str, sc: &mut Scaffold) -> Result<(), InitError> {
    write(
        root,
        "Cargo.toml",
        &cargo_bin_manifest(
            name,
            &[
                "tokio = { version = \"1\", features = [\"macros\", \"rt-multi-thread\", \"io-std\", \"io-util\"] }",
                "serde = { version = \"1\", features = [\"derive\"] }",
                "serde_json = \"1\"",
                "anyhow = \"1\"",
            ],
        ),
        sc,
    )?;
    std::fs::create_dir_all(root.join("src"))?;
    write(root, "src/main.rs", MCP_SERVER_MAIN, sc)?;
    Ok(())
}

// ---------- helpers ----------

fn template_or_default(t: &str) -> Result<&'static str, InitError> {
    let valid: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
    let lower = t.to_lowercase();
    for v in &valid {
        if *v == lower {
            return Ok(v);
        }
    }
    if lower.is_empty() {
        return Ok("custom");
    }
    Err(InitError::UnknownTemplate(t.to_owned(), valid.join(" · ")))
}

fn infer_stack(template: &str) -> String {
    match template {
        "cli-rust" | "lib-rust" | "microservice-rust" | "mcp-server-rust" => "rust".into(),
        _ => "unknown".into(),
    }
}

fn write(root: &Path, rel: &str, content: &str, sc: &mut Scaffold) -> Result<(), InitError> {
    let path = root.join(rel);
    // ! Adopt never clobbers. The user's file wins; Pipeline records the skip so
    // the agent can see what it did NOT get to configure.
    if sc.adopt && path.exists() {
        sc.skipped.push(path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    sc.push(path);
    Ok(())
}

fn readme(name: &str, template: &str) -> String {
    format!(
        "# {name}\n\nScaffolded by Pipeline · template `{template}`.\n\n## Run\n\n```\npipeline run fast\n```\n\n## Docs\n\nProject context lives in `pipeline.yaml`. See https://github.com/azzindani/Pipeline.\n"
    )
}

fn pipeline_yaml(name: &str, stack: &str, template: &str) -> String {
    let services = if template == "microservice-rust" {
        "  services:\n    - postgres:16\n"
    } else {
        "  services: []\n"
    };
    let standards = standards_block(stack, template);
    format!(
        "project: {name}\nversion: 0.0.1\n\nstack:\n  runtime: {stack}\n{services}\nstages:\n  fast:\n    - static\n    - unit\n  full:\n    - static\n    - unit\n    - container\n    - integration\n  preflight:\n    - static\n    - unit\n    - container\n    - integration\n    - security\n\ngates:\n  coverage: 70\n  image_size_mb: 200\n  critical_vulns: 0\n\n{standards}"
    )
}

/// The `standards:` block · what binds a scaffolded project to the corpus.
///
/// ! Without this every scaffolded project is born unable to route standards —
/// `pipeline_standards.route` fails on a fresh `pipeline.yaml`. The template
/// seeds the routing keys it can infer; the agent refines them.
///
/// The `project_type` + `surfaces` values are ROUTER keys owned by the Standards
/// corpus, ✗ by Pipeline. If the corpus renames one, `route` reports it under
/// `unknown_routes` rather than silently dropping the binding.
fn standards_block(stack: &str, template: &str) -> String {
    use std::fmt::Write as _;

    let (project_type, surfaces) = match template {
        "mcp-server-rust" => ("MCP server", &["Command line"][..]),
        "microservice-rust" => (
            "REST/gRPC service (Go/Rust)",
            &["HTTP / gRPC API", "Deployed service"][..],
        ),
        "lib-rust" => ("Library / SDK", &["Public package"][..]),
        // cli-rust + custom: the corpus has no matching type key · surface +
        // language routes still bind, and the agent can set project_type later.
        "cli-rust" => ("", &["Command line"][..]),
        _ => ("", &[][..]),
    };

    let mut out = String::from(
        "standards:\n  # Corpus location · unset → Pipeline's own cache.\n  \
         # Run `pipeline standards fetch`, or point at a local clone:\n  \
         # source: /path/to/Standards\n",
    );
    if !project_type.is_empty() {
        let _ = writeln!(out, "  project_type: {project_type}");
    }
    if !stack.is_empty() && stack != "unknown" {
        let _ = writeln!(out, "  languages: [{stack}]");
    }
    if surfaces.is_empty() {
        out.push_str("  surfaces: []\n");
    } else {
        out.push_str("  surfaces:\n");
        for s in surfaces {
            let _ = writeln!(out, "    - {s}");
        }
    }
    // ✗ a `# pin: <sha>` placeholder — pipeline_standards.pin appends the real key
    // below it, leaving a dangling comment. A plain hint reads correctly either way.
    out.push_str("  # `pipeline_standards.pin` records the corpus commit here\n");
    out
}

fn cargo_bin_manifest(name: &str, deps: &[&str]) -> String {
    let mut out = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nedition = \"2024\"\nrust-version = \"1.85\"\n\n[[bin]]\nname = \"{name}\"\npath = \"src/main.rs\"\n\n[dependencies]\n"
    );
    for d in deps {
        out.push_str(d);
        out.push('\n');
    }
    out
}

fn cargo_lib_manifest(name: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.0.1\"\nedition = \"2024\"\nrust-version = \"1.85\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[dependencies]\n"
    )
}

fn rust_dockerfile(name: &str) -> String {
    format!(
        "FROM rust:1.94-slim-bookworm AS builder\nWORKDIR /usr/src/app\nCOPY . .\nRUN cargo build --release\n\nFROM debian:bookworm-slim\nRUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \\\n && useradd -r -u 10001 app\nCOPY --from=builder /usr/src/app/target/release/{name} /usr/local/bin/{name}\nUSER app\nENTRYPOINT [\"/usr/local/bin/{name}\"]\n"
    )
}

fn cli_rust_main(name: &str) -> String {
    format!(
        "use clap::Parser;\n\n#[derive(Parser)]\n#[command(name = \"{name}\", version)]\nstruct Cli {{\n    /// Optional name to greet\n    name: Option<String>,\n}}\n\nfn main() {{\n    let cli = Cli::parse();\n    let target = cli.name.as_deref().unwrap_or(\"world\");\n    println!(\"hello, {{target}} · from {name}\");\n}}\n"
    )
}

const MICROSERVICE_MAIN: &str = "use axum::{Json, Router, routing::get};\nuse serde::Serialize;\n\n#[derive(Serialize)]\nstruct Health {\n    status: &'static str,\n}\n\n#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {\n    let app = Router::new()\n        .route(\"/\", get(|| async { \"hello\" }))\n        .route(\"/health\", get(|| async { Json(Health { status: \"ok\" }) }));\n    let listener = tokio::net::TcpListener::bind(\"0.0.0.0:8080\").await?;\n    axum::serve(listener, app).await?;\n    Ok(())\n}\n";

// ! rustfmt-clean as written. A scaffold that fails `pipeline run fast` on
// the very first call teaches the agent to distrust the gate.
const MCP_SERVER_MAIN: &str = "use anyhow::Result;\nuse serde_json::{Value, json};\nuse tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};\n\n#[tokio::main]\nasync fn main() -> Result<()> {\n    let stdin = tokio::io::stdin();\n    let mut reader = BufReader::new(stdin).lines();\n    let mut stdout = tokio::io::stdout();\n    while let Some(line) = reader.next_line().await? {\n        if line.trim().is_empty() {\n            continue;\n        }\n        let req: Value = match serde_json::from_str(&line) {\n            Ok(v) => v,\n            Err(_) => continue,\n        };\n        let id = req.get(\"id\").cloned();\n        let method = req.get(\"method\").and_then(Value::as_str).unwrap_or(\"\");\n        let resp = match method {\n            \"initialize\" => {\n                json!({\"jsonrpc\":\"2.0\",\"id\":id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"serverInfo\":{\"name\":\"my-mcp\",\"version\":\"0.0.1\"},\"capabilities\":{\"tools\":{}}}})\n            }\n            \"tools/list\" => json!({\"jsonrpc\":\"2.0\",\"id\":id,\"result\":{\"tools\":[]}}),\n            _ => {\n                json!({\"jsonrpc\":\"2.0\",\"id\":id,\"error\":{\"code\":-32601,\"message\":\"method not found\"}})\n            }\n        };\n        let mut bytes = serde_json::to_vec(&resp)?;\n        bytes.push(b'\\n');\n        stdout.write_all(&bytes).await?;\n        stdout.flush().await?;\n    }\n    Ok(())\n}\n";

const GITIGNORE: &str = "# Build artifacts\ntarget/\nnode_modules/\ndist/\nbuild/\n\n# Pipeline runtime data\n.pipeline/\n\n# Editors\n.vscode/\n.idea/\n*.swp\n.DS_Store\n\n# Env\n.env\n.env.local\n*.pem\n*.key\n";

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lists_known_templates() {
        let names: Vec<&str> = list_templates().into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"cli-rust"));
        assert!(names.contains(&"microservice-rust"));
        assert!(names.contains(&"custom"));
    }

    #[test]
    fn cli_rust_writes_main_and_dockerfile() {
        let dir = tempdir().unwrap();
        let outcome = init_project(dir.path(), "myapp", "cli-rust", "").unwrap();
        assert_eq!(outcome.template, "cli-rust");
        assert_eq!(outcome.stack, "rust");
        let written: Vec<String> = outcome
            .files_written
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(written.iter().any(|n| n == "main.rs"));
        assert!(written.iter().any(|n| n == "Cargo.toml"));
        assert!(written.iter().any(|n| n == "Dockerfile"));
        assert!(written.iter().any(|n| n == "pipeline.yaml"));
    }

    #[test]
    fn microservice_includes_compose() {
        let dir = tempdir().unwrap();
        let outcome = init_project(dir.path(), "svc", "microservice-rust", "").unwrap();
        let written: Vec<String> = outcome
            .files_written
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(written.iter().any(|n| n == "docker-compose.yml"));
    }

    #[test]
    fn adopt_writes_the_gaps_and_never_clobbers() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("existing");
        std::fs::create_dir_all(root.join("src")).unwrap();
        // A live project: hand-written source + a README the user cares about.
        std::fs::write(root.join("README.md"), "# hand written\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { /* mine */ }\n").unwrap();

        let outcome = init_project_with(dir.path(), "existing", "cli-rust", "", true).unwrap();
        assert!(outcome.adopted);

        let name_of = |v: &Vec<PathBuf>| -> Vec<String> {
            v.iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        let written = name_of(&outcome.files_written);
        let skipped = name_of(&outcome.files_skipped);

        // The gaps got filled…
        assert!(written.iter().any(|n| n == "pipeline.yaml"));
        assert!(written.iter().any(|n| n == "Cargo.toml"));
        // …and the user's files were reported, not rewritten.
        assert!(skipped.iter().any(|n| n == "README.md"));
        assert!(skipped.iter().any(|n| n == "main.rs"));
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "# hand written\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "fn main() { /* mine */ }\n"
        );
    }

    #[test]
    fn init_without_adopt_still_refuses_a_non_empty_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("existing");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "mine\n").unwrap();
        let result = init_project(dir.path(), "existing", "cli-rust", "");
        assert!(matches!(result, Err(InitError::NotEmpty(_))));
    }

    #[test]
    fn scaffolded_config_parses_and_carries_routing_keys() {
        // Regression: a scaffolded pipeline.yaml used to omit `standards:` entirely,
        // so pipeline_standards.route failed on every project Pipeline created.
        let dir = tempdir().unwrap();
        init_project(dir.path(), "srv", "mcp-server-rust", "").unwrap();
        let text = std::fs::read_to_string(dir.path().join("srv/pipeline.yaml")).unwrap();

        let cfg =
            pipeline_config::PipelineConfig::parse(&text).expect("scaffolded yaml must parse");
        let std_cfg = cfg.standards;
        assert_eq!(std_cfg.project_type.as_deref(), Some("MCP server"));
        assert_eq!(std_cfg.languages, vec!["rust".to_owned()]);
        assert_eq!(std_cfg.surfaces, vec!["Command line".to_owned()]);
    }

    #[test]
    fn every_template_emits_parseable_config() {
        for (template, _) in list_templates() {
            let dir = tempdir().unwrap();
            init_project(dir.path(), "p", template, "").unwrap();
            let text = std::fs::read_to_string(dir.path().join("p/pipeline.yaml")).unwrap();
            pipeline_config::PipelineConfig::parse(&text)
                .unwrap_or_else(|e| panic!("template '{template}' emits invalid yaml: {e}"));
        }
    }

    #[test]
    fn every_template_emits_rustfmt_clean_code() {
        // Regression: mcp-server-rust's stub was not formatted, so `init` followed
        // by `run fast` was RED on a brand-new project. A scaffold that fails the
        // gate it hands you teaches the agent to distrust the gate.
        for (template, _) in list_templates() {
            let dir = tempdir().unwrap();
            let outcome = init_project(dir.path(), "p", template, "").unwrap();
            for file in outcome
                .files_written
                .iter()
                .filter(|f| f.extension().is_some_and(|e| e == "rs"))
            {
                let out = std::process::Command::new("rustfmt")
                    .args(["--edition", "2024", "--check"])
                    .arg(file)
                    .output()
                    .expect("rustfmt must be installed");
                assert!(
                    out.status.success(),
                    "template '{template}' emits unformatted {}:\n{}",
                    file.display(),
                    String::from_utf8_lossy(&out.stdout)
                );
            }
        }
    }

    #[test]
    fn unknown_template_errors() {
        let dir = tempdir().unwrap();
        let result = init_project(dir.path(), "x", "rocket-launcher", "");
        assert!(matches!(result, Err(InitError::UnknownTemplate(_, _))));
    }

    #[test]
    fn empty_template_falls_back_to_custom() {
        let dir = tempdir().unwrap();
        let outcome = init_project(dir.path(), "x", "", "").unwrap();
        assert_eq!(outcome.template, "custom");
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(name: &str, source: &str) -> RegisteredTemplate {
        RegisteredTemplate {
            name: name.to_owned(),
            source: source.to_owned(),
            kind: "path".to_owned(),
            registered_at: "2026-07-20T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn a_registered_template_survives_a_round_trip() {
        let dir = tempdir().unwrap();
        assert!(load_registry(dir.path()).is_empty());
        assert!(!upsert_registered(dir.path(), entry("svc", "/srv/tpl")).unwrap());
        let found = find_registered(dir.path(), "svc").expect("registered");
        assert_eq!(found.source, "/srv/tpl");
        assert_eq!(found.kind, "path");
    }

    #[test]
    fn re_registering_a_name_replaces_it_rather_than_appending() {
        // Regression: entries were appended with no dedup, so two rows could share a
        // name and nothing defined which one init would pick.
        let dir = tempdir().unwrap();
        upsert_registered(dir.path(), entry("svc", "/old")).unwrap();
        assert!(upsert_registered(dir.path(), entry("svc", "/new")).unwrap());
        let all = load_registry(dir.path());
        assert_eq!(all.len(), 1, "duplicate name persisted: {all:?}");
        assert_eq!(all[0].source, "/new");
    }

    #[test]
    fn a_corrupt_registry_does_not_break_the_built_ins() {
        // ! The built-in templates must stay usable when the user registry is junk.
        let dir = tempdir().unwrap();
        let path = registry_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_registry(dir.path()).is_empty());
        assert!(is_builtin("cli-rust"));
    }

    #[test]
    fn built_ins_are_recognized_case_insensitively() {
        assert!(is_builtin("cli-rust"));
        assert!(is_builtin("CLI-Rust"));
        assert!(!is_builtin("my-template"));
    }

    #[test]
    fn a_registered_template_is_instantiated_for_real() {
        // The loop that was open: register → list → init. This is the init half —
        // the template's own files land in the new project.
        let dir = tempdir().unwrap();
        let source = dir.path().join("tpl");
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::create_dir_all(source.join(".git")).unwrap();
        std::fs::write(source.join("src/app.py"), "print('hi')\n").unwrap();
        std::fs::write(source.join("Dockerfile"), "FROM python:3.12\n").unwrap();
        std::fs::write(source.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let parent = dir.path().join("out");
        std::fs::create_dir_all(&parent).unwrap();
        let outcome =
            instantiate_registered(&parent, "myapp", "py-svc", &source, "python-uv", false)
                .expect("instantiate");

        let root = parent.join("myapp");
        assert_eq!(
            std::fs::read_to_string(root.join("src/app.py")).unwrap(),
            "print('hi')\n"
        );
        assert!(root.join("Dockerfile").is_file());
        // ✗ carry the template's VCS state into the new project.
        assert!(!root.join(".git").exists(), "source .git was copied");
        assert_eq!(outcome.template, "py-svc");
        assert_eq!(outcome.stack, "python-uv");

        // Pipeline filled the gap the template left.
        let cfg = pipeline_config::PipelineConfig::parse(
            &std::fs::read_to_string(root.join("pipeline.yaml")).unwrap(),
        )
        .expect("parses");
        assert_eq!(cfg.project, "myapp");
        assert_eq!(cfg.stack.runtime, "python-uv");
    }

    #[test]
    fn a_template_that_ships_its_own_config_keeps_it_but_gets_the_new_name() {
        // ! A copied pipeline.yaml carries the TEMPLATE's project name · every memory
        // row and lock keys off `project`, so leaving it is silent corruption.
        let dir = tempdir().unwrap();
        let source = dir.path().join("tpl");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("pipeline.yaml"),
            "# template config · keep this comment\nproject: TEMPLATE\nversion: 2.0.0\nstack:\n  runtime: go\n\ngates:\n  coverage: 90\n",
        )
        .unwrap();

        let parent = dir.path().join("out");
        std::fs::create_dir_all(&parent).unwrap();
        instantiate_registered(&parent, "myapp", "go-svc", &source, "", false)
            .expect("instantiate");

        let text = std::fs::read_to_string(parent.join("myapp/pipeline.yaml")).unwrap();
        let cfg = pipeline_config::PipelineConfig::parse(&text).expect("parses");
        assert_eq!(cfg.project, "myapp", "project name not rewritten");
        assert_eq!(
            cfg.gates.coverage,
            Some(90),
            "template's gate was clobbered"
        );
        assert_eq!(cfg.stack.runtime, "go", "template's stack was clobbered");
        assert!(text.contains("# template config"), "comment lost:\n{text}");
    }

    #[test]
    fn instantiating_into_a_non_empty_root_is_refused_unless_adopting() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("tpl");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.txt"), "from template\n").unwrap();

        let parent = dir.path().join("out");
        std::fs::create_dir_all(parent.join("myapp")).unwrap();
        std::fs::write(parent.join("myapp/a.txt"), "mine\n").unwrap();

        let refused = instantiate_registered(&parent, "myapp", "t", &source, "", false);
        assert!(matches!(refused, Err(InitError::NotEmpty(_))));

        // Adopt fills gaps and ✗ clobbers what is already there.
        instantiate_registered(&parent, "myapp", "t", &source, "", true).expect("adopt");
        assert_eq!(
            std::fs::read_to_string(parent.join("myapp/a.txt")).unwrap(),
            "mine\n"
        );
    }

    #[test]
    fn a_source_that_is_not_a_directory_is_refused() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("not-a-tree");
        std::fs::write(&file, "x").unwrap();
        let e = instantiate_registered(dir.path(), "p", "t", &file, "", false);
        assert!(matches!(e, Err(InitError::Source(_))), "{e:?}");
    }
}

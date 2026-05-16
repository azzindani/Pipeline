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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitOutcome {
    pub name: String,
    pub template: String,
    pub stack: String,
    pub root: PathBuf,
    pub files_written: Vec<PathBuf>,
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
    let root = parent.join(name);
    if root.exists() {
        let mut rd = std::fs::read_dir(&root)?;
        if rd.next().is_some() {
            return Err(InitError::NotEmpty(root.display().to_string()));
        }
    } else {
        std::fs::create_dir_all(&root)?;
    }

    let template = template_or_default(template)?;
    let stack = if stack.is_empty() {
        infer_stack(template)
    } else {
        stack.to_owned()
    };

    let mut written: Vec<PathBuf> = Vec::new();

    write(&root, ".gitignore", GITIGNORE, &mut written)?;
    write(&root, "README.md", &readme(name, template), &mut written)?;
    write(
        &root,
        "pipeline.yaml",
        &pipeline_yaml(name, &stack, template),
        &mut written,
    )?;

    match template {
        "cli-rust" => scaffold_cli_rust(&root, name, &mut written)?,
        "lib-rust" => scaffold_lib_rust(&root, name, &mut written)?,
        "microservice-rust" => scaffold_microservice_rust(&root, name, &mut written)?,
        "mcp-server-rust" => scaffold_mcp_server_rust(&root, name, &mut written)?,
        "custom" => {} // base files already written
        _ => unreachable!("template_or_default narrows to known set"),
    }

    Ok(InitOutcome {
        name: name.to_owned(),
        template: template.to_owned(),
        stack,
        root,
        files_written: written,
    })
}

// ---------- per-template scaffolding ----------

fn scaffold_cli_rust(root: &Path, name: &str, written: &mut Vec<PathBuf>) -> Result<(), InitError> {
    write(
        root,
        "Cargo.toml",
        &cargo_bin_manifest(
            name,
            &["clap = { version = \"4\", features = [\"derive\"] }"],
        ),
        written,
    )?;
    std::fs::create_dir_all(root.join("src"))?;
    write(root, "src/main.rs", &cli_rust_main(name), written)?;
    write(root, "Dockerfile", &rust_dockerfile(name), written)?;
    Ok(())
}

fn scaffold_lib_rust(root: &Path, name: &str, written: &mut Vec<PathBuf>) -> Result<(), InitError> {
    write(root, "Cargo.toml", &cargo_lib_manifest(name), written)?;
    std::fs::create_dir_all(root.join("src"))?;
    write(
        root,
        "src/lib.rs",
        "//! Crate documentation lives here.\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn it_works() {\n        assert_eq!(2 + 2, 4);\n    }\n}\n",
        written,
    )?;
    Ok(())
}

fn scaffold_microservice_rust(
    root: &Path,
    name: &str,
    written: &mut Vec<PathBuf>,
) -> Result<(), InitError> {
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
        written,
    )?;
    std::fs::create_dir_all(root.join("src"))?;
    write(root, "src/main.rs", MICROSERVICE_MAIN, written)?;
    write(root, "Dockerfile", &rust_dockerfile(name), written)?;
    write(
        root,
        "docker-compose.yml",
        &format!(
            "services:\n  {name}:\n    build: .\n    ports:\n      - \"8080:8080\"\n    healthcheck:\n      test: [\"CMD\", \"curl\", \"-f\", \"http://localhost:8080/health\"]\n      interval: 5s\n      timeout: 2s\n      retries: 5\n"
        ),
        written,
    )?;
    Ok(())
}

fn scaffold_mcp_server_rust(
    root: &Path,
    name: &str,
    written: &mut Vec<PathBuf>,
) -> Result<(), InitError> {
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
        written,
    )?;
    std::fs::create_dir_all(root.join("src"))?;
    write(root, "src/main.rs", MCP_SERVER_MAIN, written)?;
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

fn write(
    root: &Path,
    rel: &str,
    content: &str,
    written: &mut Vec<PathBuf>,
) -> Result<(), InitError> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;
    written.push(path);
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
    format!(
        "project: {name}\nversion: 0.0.1\n\nstack:\n  runtime: {stack}\n{services}\nstages:\n  fast:\n    - static\n    - unit\n  full:\n    - static\n    - unit\n    - container\n    - integration\n  preflight:\n    - static\n    - unit\n    - container\n    - integration\n    - security\n\ngates:\n  coverage: 70\n  image_size_mb: 200\n  critical_vulns: 0\n"
    )
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

const MICROSERVICE_MAIN: &str = "use axum::{Router, Json, routing::get};\nuse serde::Serialize;\n\n#[derive(Serialize)]\nstruct Health { status: &'static str }\n\n#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {\n    let app = Router::new()\n        .route(\"/\", get(|| async { \"hello\" }))\n        .route(\"/health\", get(|| async { Json(Health { status: \"ok\" }) }));\n    let listener = tokio::net::TcpListener::bind(\"0.0.0.0:8080\").await?;\n    axum::serve(listener, app).await?;\n    Ok(())\n}\n";

const MCP_SERVER_MAIN: &str = "use anyhow::Result;\nuse serde_json::{Value, json};\nuse tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};\n\n#[tokio::main]\nasync fn main() -> Result<()> {\n    let stdin = tokio::io::stdin();\n    let mut reader = BufReader::new(stdin).lines();\n    let mut stdout = tokio::io::stdout();\n    while let Some(line) = reader.next_line().await? {\n        if line.trim().is_empty() { continue; }\n        let req: Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };\n        let id = req.get(\"id\").cloned();\n        let method = req.get(\"method\").and_then(Value::as_str).unwrap_or(\"\");\n        let resp = match method {\n            \"initialize\" => json!({\"jsonrpc\":\"2.0\",\"id\":id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"serverInfo\":{\"name\":\"my-mcp\",\"version\":\"0.0.1\"},\"capabilities\":{\"tools\":{}}}}),\n            \"tools/list\" => json!({\"jsonrpc\":\"2.0\",\"id\":id,\"result\":{\"tools\":[]}}),\n            _ => json!({\"jsonrpc\":\"2.0\",\"id\":id,\"error\":{\"code\":-32601,\"message\":\"method not found\"}}),\n        };\n        let mut bytes = serde_json::to_vec(&resp)?;\n        bytes.push(b'\\n');\n        stdout.write_all(&bytes).await?;\n        stdout.flush().await?;\n    }\n    Ok(())\n}\n";

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

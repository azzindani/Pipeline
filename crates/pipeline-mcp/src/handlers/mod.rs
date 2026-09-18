//! Per-tool handler modules · one async `handle(req, state)` each.

pub mod data;
pub mod deploy;
pub mod docker;
pub mod docs;
pub mod e2e;
pub mod env;
pub mod fleet;
pub mod memory;
pub mod meta;
pub mod observe;
pub mod plan;
pub mod project;
pub mod repo;
pub mod report;
pub mod run;
pub mod security;
pub mod session;
pub mod simulate;
pub mod standards;
pub mod test;

use crate::server::ServerState;
use std::path::PathBuf;
use std::sync::Arc;

/// Open or reuse the shared Memory handle, anchored at `cwd/.pipeline/memory.db`.
pub(crate) async fn ensure_memory(
    state: &Arc<ServerState>,
) -> Result<pipeline_memory::Memory, String> {
    let mut guard = state.memory.lock().await;
    if let Some(m) = guard.as_ref() {
        return Ok(m.clone());
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let path: PathBuf = cwd.join(".pipeline").join("memory.db");
    let mem = pipeline_memory::Memory::open(&path)
        .await
        .map_err(|e| e.to_string())?;
    *guard = Some(mem.clone());
    Ok(mem)
}

pub(crate) fn load_config_in_cwd() -> Result<pipeline_config::PipelineConfig, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let path = cwd.join("pipeline.yaml");
    pipeline_config::PipelineConfig::load(&path).map_err(|e| e.to_string())
}

/// Refuse a caller-supplied git positional (revision · remote · branch · URL) that git
/// would parse as an option.
///
/// ! None of these legitimately starts with `-`, but git reads anything that does as a
/// flag. `--output=<file>` on `git log` / `git diff` turned a read-only history query
/// into a file write; `--upload-pack=<cmd>` on `clone` / `ls-remote` and
/// `--receive-pack=<cmd>` on `push` run a command.
pub(crate) fn refuse_git_option(field: &str, value: &str) -> Result<(), String> {
    if value.starts_with('-') {
        return Err(format!(
            "'{field}' = '{value}' starts with '-' · git would read it as an option, ✗ a {field}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::refuse_git_option;

    #[test]
    fn a_value_shaped_like_an_option_is_refused_naming_the_field() {
        let e = refuse_git_option("base", "--output=/tmp/x").expect_err("option");
        assert!(e.contains("'base'"), "{e}");
        assert!(refuse_git_option("from", "-p").is_err());
        assert!(refuse_git_option("url", "--upload-pack=touch /tmp/x").is_err());
    }

    #[test]
    fn ordinary_positionals_pass() {
        for v in [
            "HEAD",
            "origin/main",
            "v0.2.0",
            "a1b2c3d",
            "HEAD~3",
            "main@{1}",
            "https://github.com/o/r.git",
            "feature/x-y",
        ] {
            assert!(refuse_git_option("base", v).is_ok(), "{v}");
        }
    }
}

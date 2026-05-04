//! Per-tool handler modules · one async `handle(req, state)` each.

pub mod memory;
pub mod meta;
pub mod report;
pub mod run;
pub mod session;

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

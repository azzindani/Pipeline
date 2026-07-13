//! Standards integration — Pipeline's binding to an external, separately-versioned
//! standards corpus (`github.com/azzindani/Standards`).
//!
//! Standards is NOT vendored and NOT a monorepo path. It is a **dependency**:
//!
//! ```text
//! resolve  → find the corpus (config → env → cache → clone) · record its SHA
//! index    → read index.json, the contract its CI emits
//! route    → execute ROUTER's rules → the standards binding THIS project
//! inject   → L0 brief (always) · L1 doc (on demand) · L2 checklists (gates)
//! ```
//!
//! ! Invariant: Pipeline ✗ restates a standard or a routing rule. Every id, tier,
//! route and checklist item originates in `index.json`. When Standards changes,
//! this crate does not — that is the whole point of the split.

pub mod index;
pub mod inject;
pub mod resolve;
pub mod route;

pub use index::{Index, Standard};
pub use inject::{Brief, Checklist};
pub use resolve::{Origin, Resolved};
pub use route::RoutedSet;

#[derive(Debug, thiserror::Error)]
pub enum StandardsError {
    #[error("standards source not found: {path}")]
    SourceNotFound { path: String },

    #[error(
        "no standards cache at {path} and cloning is disabled · \
         run `pipeline standards fetch`, or set standards.source in pipeline.yaml"
    )]
    CacheMissing { path: String },

    #[error(
        "{path} has no index.json · not a Standards repo, or it predates the index \
         (regenerate upstream with `tools/validate.py --emit-index`)"
    )]
    NotAStandardsRepo { path: String },

    #[error("read index.json at {path}: {source}")]
    IndexMissing {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "index.json schema v{found}, this Pipeline understands v{supported} · \
         upgrade Pipeline or pin standards to a compatible commit"
    )]
    SchemaMismatch { found: u32, supported: u32 },

    #[error("unknown standard '{id}' · call pipeline_standards.list to see the catalog")]
    UnknownStandard { id: String },

    #[error(
        "{path} is a user-owned standards clone · Pipeline will not modify it. \
         Update it yourself, or switch standards.source to a git URL so Pipeline \
         manages its own cache"
    )]
    SourceReadOnly { path: String },

    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("parse index.json: {0}")]
    Json(#[from] serde_json::Error),
}

/// The whole pipeline in one call: resolve → index → route → brief.
///
/// This is what a session start needs; everything else is drill-down.
pub async fn load(
    cfg: &pipeline_config::Standards,
    runtime: &str,
    allow_clone: bool,
) -> Result<(Index, Resolved, RoutedSet), StandardsError> {
    let resolved = resolve::resolve(cfg, allow_clone).await?;
    let index = Index::load(&resolved.root).await?;
    let routed = route::route(&index, cfg, runtime, true);
    Ok((index, resolved, routed))
}

//! Pipeline stages · concrete `Stage` implementations + profile runner.
//!
//! Day-2 ships:
//! - `StaticStage`: `cargo fmt --check` + `cargo clippy -D warnings`
//! - `UnitStage`: `cargo test --workspace`
//!
//! Container · integration · security stages land later (POC week 2+).

mod runner;
mod static_stage;
mod unit_stage;

pub use runner::{Runner, RunnerSummary};
pub use static_stage::StaticStage;
pub use unit_stage::UnitStage;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

use pipeline_core::{Stage, StageKind};

/// Build a `Stage` impl for `kind`, or `None` if not yet implemented.
pub fn stage_for(kind: StageKind) -> Option<Box<dyn Stage>> {
    match kind {
        StageKind::Static => Some(Box::new(StaticStage)),
        StageKind::Unit => Some(Box::new(UnitStage)),
        StageKind::Container | StageKind::Integration | StageKind::Security => None,
    }
}

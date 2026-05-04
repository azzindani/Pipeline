//! Pipeline stages · concrete `Stage` implementations + profile runner.
//!
//! - `StaticStage`: `cargo fmt --check` + `cargo clippy -D warnings`
//! - `UnitStage`: `cargo test --workspace`
//! - `ContainerStage`: `docker build` + image-size gate
//! - `IntegrationStage`: `docker compose up --wait` then `compose down`
//!
//! Container/Integration auto-skip when no `Dockerfile` / `docker-compose.yml`
//! exists or the docker daemon is unreachable.
//!
//! Security stage lands at MVP week 6.

mod container_stage;
mod integration_stage;
mod runner;
mod static_stage;
mod unit_stage;

pub use container_stage::ContainerStage;
pub use integration_stage::IntegrationStage;
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
        StageKind::Container => Some(Box::new(ContainerStage)),
        StageKind::Integration => Some(Box::new(IntegrationStage)),
        StageKind::Security => None,
    }
}

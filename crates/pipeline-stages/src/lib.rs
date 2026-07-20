//! Pipeline stages · concrete `Stage` implementations + profile runner.
//!
//! - `StaticStage`: `cargo fmt --check` + `cargo clippy -D warnings`
//! - `UnitStage`: `cargo test --workspace`
//! - `ContainerStage`: `docker build` + image-size gate
//! - `IntegrationStage`: `docker compose up --wait` then `compose down`
//! - `SecurityStage`: trufflehog secret scan + `cargo audit`
//!
//! Container/Integration auto-skip when no `Dockerfile` / `docker-compose.yml`
//! exists or the docker daemon is unreachable.
//!
//! ! A skip is not a pass. `Runner` records every skip with its reason and
//! fails outright on a strict profile (`preflight`) — see `runner`.

mod container_stage;
mod integration_stage;
mod runner;
mod security_stage;
mod static_stage;
mod unit_stage;

pub use container_stage::ContainerStage;
pub use integration_stage::IntegrationStage;
pub use runner::{Runner, RunnerSummary, SkippedStage};
pub use security_stage::{ScanStatus, SecretFinding, SecurityStage};
pub use static_stage::StaticStage;
pub use unit_stage::UnitStage;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

use pipeline_core::{Stage, StageKind};

/// Build a `Stage` impl for `kind`.
///
/// ! Total, ✗ `Option`. The previous signature let `Security` return `None`,
/// the runner turned that into `Skipped`, and `Skipped` folded into `Pass` —
/// so an unimplemented stage silently disappeared from the gate. Making this
/// total means adding a `StageKind` fails to compile until it has an executor,
/// rather than quietly widening the hole.
pub fn stage_for(kind: StageKind) -> Box<dyn Stage> {
    match kind {
        StageKind::Static => Box::new(StaticStage),
        StageKind::Unit => Box::new(UnitStage),
        StageKind::Container => Box::new(ContainerStage),
        StageKind::Integration => Box::new(IntegrationStage),
        StageKind::Security => Box::new(SecurityStage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::StageProfile;

    #[test]
    fn every_stage_in_every_profile_has_an_executor() {
        // ! Regression: `stage_for(Security)` returned None, the runner turned
        // that into Skipped, and Skipped folded into Pass — so preflight was
        // green while the only stage that distinguishes it never ran.
        for profile in [
            StageProfile::Fast,
            StageProfile::Full,
            StageProfile::Preflight,
            StageProfile::Confirm,
        ] {
            for kind in profile.stages() {
                assert_eq!(
                    stage_for(*kind).kind(),
                    *kind,
                    "{} resolved to the wrong executor",
                    kind.as_str()
                );
            }
        }
    }

    #[test]
    fn the_security_stage_is_wired_into_preflight() {
        assert!(
            StageProfile::Preflight
                .stages()
                .contains(&StageKind::Security)
        );
        assert_eq!(stage_for(StageKind::Security).kind(), StageKind::Security);
    }
}

//! `pipeline-digest` — structural analysis of a source tree.
//!
//! kind: component
//!
//! First capability: the primitive registry the primitives standard requires —
//! generated from source, never hand-maintained, because a hand-maintained
//! index drifts within one sprint.
//!
//! Repo digestion of *external* repos (clone · capability index · digest JSON)
//! lands on the same analysis core. See `PLAN.md`.

pub mod registry;

/// Crate version, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

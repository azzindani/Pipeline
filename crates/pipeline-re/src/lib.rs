//! `pipeline-re` — STUB. Layout locked at POC scaffold, ✗ implemented.
//!
//! kind: component
//! status: stub
//! charter: reverse engineering: codebase · binary · service · docker · infra
//! implemented-in: handlers/repo.rs
//!
//! ! `implemented-in` is the honest part. The charter above is live behaviour
//! today — it just lives in a `pipeline-mcp` handler rather than here, so the
//! workspace layout advertises a separation that does not exist yet. Moving it
//! down is the activation, ✗ writing new code here.
//!
//! See `PLAN.md` for the milestone that activates this crate.

/// Crate version, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

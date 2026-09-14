//! Generates the registry for this repo and enforces it.
//!
//! ! The CI gate the primitives standard calls for. Both assertions have to
//! survive adoption: the tree must parse, and declared units must not invert
//! the dependency direction. Undeclared units are counted and reported, ✗
//! failed — failing on them on day one would mean deleting the gate to get CI
//! green, which is how enforcement dies.

use pipeline_digest::registry;
use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
}

#[test]
fn registry_generates_and_direction_holds() {
    let reg = registry::generate(repo_root()).expect("generate registry");

    assert!(!reg.units.is_empty(), "walked the tree and found no units");

    let direction: Vec<_> = reg
        .violations
        .iter()
        .filter(|v| v.rule == "dependency-direction")
        .collect();
    assert!(
        direction.is_empty(),
        "dependency direction inverted — a unit depends on a kind above it:\n{direction:#?}"
    );

    let external: Vec<_> = reg
        .violations
        .iter()
        .filter(|v| v.rule == "primitive-stdlib-only")
        .collect();
    assert!(
        external.is_empty(),
        "a primitive imports third-party crates:\n{external:#?}"
    );

    let undeclared = reg.units.len() - reg.declared();
    eprintln!(
        "registry: {} units · {} declared · {} undeclared · {} violations",
        reg.units.len(),
        reg.declared(),
        undeclared,
        reg.violations.len()
    );
}

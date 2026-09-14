//! Generates the registry for this repo and enforces it.
//!
//! ! The CI gate the primitives standard calls for. Both assertions have to
//! survive adoption: the tree must parse, and declared units must not invert
//! the dependency direction. Undeclared units are counted and reported, ✗
//! failed — failing on them on day one would mean deleting the gate to get CI
//! green, which is how enforcement dies.

use pipeline_digest::registry;
use std::collections::BTreeMap;
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

/// A crate that is nearly empty must say so in its own header.
///
/// ! The defect this prevents is a workspace that advertises an architecture it
/// does not have. Seven crates carry a charter their code does not implement —
/// the behaviour lives in a `pipeline-mcp` handler instead. That is survivable
/// while it is declared and visible; it is not survivable silently, because a
/// reader (or an agent planning work) takes the crate list as the design.
#[test]
fn a_stub_crate_declares_that_it_is_a_stub() {
    const STUB_LINE_LIMIT: usize = 25;

    let reg = registry::generate(repo_root()).expect("generate registry");

    // ! Size is measured per CRATE, ✗ per lib.rs. A thin lib.rs that re-exports
    // submodules is a normal crate, not a stub — measuring the entry file alone
    // would flag it and teach the next reader to ignore this test.
    let mut crate_lines: BTreeMap<String, usize> = BTreeMap::new();
    for unit in &reg.units {
        let krate = unit.path.split('/').nth(1).unwrap_or_default().to_owned();
        *crate_lines.entry(krate).or_default() += unit.lines;
    }

    let mut undeclared_stubs = Vec::new();
    for (krate, lines) in &crate_lines {
        if *lines > STUB_LINE_LIMIT {
            continue;
        }
        let path = format!("crates/{krate}/src/lib.rs");
        let source = std::fs::read_to_string(repo_root().join(&path)).expect("read");
        if !source.contains("//! status: stub") {
            undeclared_stubs.push(path);
        }
    }

    assert!(
        undeclared_stubs.is_empty(),
        "crate is effectively empty but does not declare `//! status: stub` — \
         the workspace layout would advertise an architecture that does not exist:\n{undeclared_stubs:#?}"
    );
}

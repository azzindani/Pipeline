//! Primitive registry · generated from source, ✗ hand-maintained.
//!
//! Implements the registry the primitives standard requires. The standard's
//! claim is that a hand-maintained index drifts within one sprint, so the index
//! is derived from the tree on every run and CI fails when the checked-in copy
//! disagrees.
//!
//! Unit granularity is the **module file**. Finer granularity (per-function)
//! was rejected for v1: kind is declared in a module header, so a file is the
//! smallest thing that can carry a declaration, and a registry that infers kind
//! rather than reading it would be guessing at the one field everything else
//! keys off.
//!
//! ! Direction is the load-bearing check. Everything else here is bookkeeping;
//! a primitive importing a part is the violation that makes the model a lie.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("walk {path}: {source}")]
    Walk {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: syn::Error,
    },
    #[error("serialize registry: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Unit kinds, ordered by depth. `Primitive` is the deepest — it may depend on
/// nothing in the tree above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Primitive,
    Part,
    Component,
    Composite,
    /// No declaration in the module header. Not a kind — an omission the
    /// checklist requires closing, carried explicitly so it cannot hide.
    Undeclared,
}

impl Kind {
    /// Depth in the dependency order · lower may not depend on higher.
    fn depth(self) -> u8 {
        match self {
            Self::Primitive => 0,
            Self::Part => 1,
            Self::Component => 2,
            Self::Composite => 3,
            Self::Undeclared => u8::MAX,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "primitive" => Some(Self::Primitive),
            "part" => Some(Self::Part),
            "component" => Some(Self::Component),
            "composite" => Some(Self::Composite),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primitive => "primitive",
            Self::Part => "part",
            Self::Component => "component",
            Self::Composite => "composite",
            Self::Undeclared => "undeclared",
        }
    }
}

/// One registered unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    pub id: String,
    pub kind: Kind,
    pub path: String,
    /// Public item names this unit exposes — its contract surface.
    pub exports: Vec<String>,
    /// Crate-internal and workspace dependencies, by crate | module name.
    pub deps: Vec<String>,
    /// Third-party crates imported. Non-empty on a primitive → violation.
    pub external_deps: Vec<String>,
    pub lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub units: Vec<Unit>,
    pub violations: Vec<Violation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Violation {
    pub unit: String,
    pub rule: String,
    pub detail: String,
}

/// Crates in the workspace · anything else imported is third-party.
fn workspace_crates(root: &Path) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let crates_dir = root.join("crates");
    if let Ok(entries) = std::fs::read_dir(&crates_dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    set.insert(name.replace('-', "_"));
                }
            }
        }
    }
    set
}

/// Kind declared in the module header · `//! kind: primitive`.
fn declared_kind(source: &str) -> Kind {
    source
        .lines()
        .take_while(|l| l.starts_with("//!") || l.trim().is_empty())
        .find_map(|l| {
            l.strip_prefix("//!")
                .and_then(|r| r.trim().strip_prefix("kind:"))
                .and_then(Kind::parse)
        })
        .unwrap_or(Kind::Undeclared)
}

/// Root segment of a `use` path · `std::fmt::Debug` → `std`.
fn use_roots(file: &syn::File) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();
    for item in &file.items {
        if let syn::Item::Use(u) = item {
            collect_use_root(&u.tree, &mut roots);
        }
    }
    roots
}

fn collect_use_root(tree: &syn::UseTree, out: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Path(p) => {
            out.insert(p.ident.to_string());
        }
        syn::UseTree::Name(n) => {
            out.insert(n.ident.to_string());
        }
        syn::UseTree::Rename(r) => {
            out.insert(r.ident.to_string());
        }
        syn::UseTree::Group(g) => {
            for t in &g.items {
                collect_use_root(t, out);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

fn public_exports(file: &syn::File) -> Vec<String> {
    let is_pub = |v: &syn::Visibility| matches!(v, syn::Visibility::Public(_));
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Fn(f) if is_pub(&f.vis) => Some(f.sig.ident.to_string()),
            syn::Item::Struct(s) if is_pub(&s.vis) => Some(s.ident.to_string()),
            syn::Item::Enum(e) if is_pub(&e.vis) => Some(e.ident.to_string()),
            syn::Item::Trait(t) if is_pub(&t.vis) => Some(t.ident.to_string()),
            syn::Item::Type(t) if is_pub(&t.vis) => Some(t.ident.to_string()),
            _ => None,
        })
        .collect()
}

/// Build the registry by walking `root/crates/**/src/**.rs`.
pub fn generate(root: &Path) -> Result<Registry, RegistryError> {
    let ws = workspace_crates(root);
    let mut units: Vec<Unit> = Vec::new();

    for entry in walkdir::WalkDir::new(root.join("crates"))
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // Tests are call sites, ✗ units. Registering them would let a test file
        // satisfy the call-site count that justifies a promotion.
        if path.components().any(|c| c.as_os_str() == "tests") {
            continue;
        }
        let source = std::fs::read_to_string(path).map_err(|source| RegistryError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let parsed = syn::parse_file(&source).map_err(|source| RegistryError::Parse {
            path: path.display().to_string(),
            source,
        })?;

        let roots = use_roots(&parsed);
        let (deps, external_deps): (Vec<String>, Vec<String>) = roots
            .into_iter()
            .filter(|r| {
                !matches!(
                    r.as_str(),
                    "std" | "core" | "alloc" | "crate" | "self" | "super"
                )
            })
            .partition(|r| ws.contains(r));

        units.push(Unit {
            id: unit_id(root, path),
            kind: declared_kind(&source),
            path: rel(root, path),
            exports: public_exports(&parsed),
            deps,
            external_deps,
            lines: source.lines().count(),
        });
    }

    units.sort_by(|a, b| a.id.cmp(&b.id));
    let violations = check(&units);
    Ok(Registry { units, violations })
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// `crates/pipeline-mcp/src/handlers/run.rs` → `pipeline_mcp::handlers::run`.
fn unit_id(root: &Path, path: &Path) -> String {
    let rel = rel(root, path);
    let trimmed = rel
        .trim_start_matches("crates/")
        .replace("/src/", "::")
        .replace(".rs", "");
    trimmed.replace('/', "::").replace('-', "_")
}

/// The checks that make the model enforceable rather than descriptive.
fn check(units: &[Unit]) -> Vec<Violation> {
    let by_crate: BTreeMap<String, Kind> = units
        .iter()
        .filter(|u| u.id.ends_with("::lib"))
        .map(|u| (crate_of(&u.id), u.kind))
        .collect();

    let mut out = Vec::new();
    for u in units {
        if u.kind == Kind::Undeclared {
            out.push(Violation {
                unit: u.id.clone(),
                rule: "kind-declared".to_owned(),
                detail: "module header carries no `//! kind:` line".to_owned(),
            });
            continue;
        }

        // ! The load-bearing rule. A primitive importing a part inverts the
        // dependency direction the whole model rests on.
        for dep in &u.deps {
            let Some(dep_kind) = by_crate.get(dep) else {
                continue;
            };
            if *dep_kind != Kind::Undeclared && dep_kind.depth() > u.kind.depth() {
                out.push(Violation {
                    unit: u.id.clone(),
                    rule: "dependency-direction".to_owned(),
                    detail: format!(
                        "{} depends on {dep} ({}) — direction is downward only",
                        u.kind.as_str(),
                        dep_kind.as_str()
                    ),
                });
            }
        }

        if u.kind == Kind::Primitive && !u.external_deps.is_empty() {
            out.push(Violation {
                unit: u.id.clone(),
                rule: "primitive-stdlib-only".to_owned(),
                detail: format!("third-party imports: {}", u.external_deps.join(" · ")),
            });
        }
    }
    out
}

fn crate_of(id: &str) -> String {
    id.split("::").next().unwrap_or(id).to_owned()
}

impl Registry {
    pub fn to_json(&self) -> Result<String, RegistryError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Units carrying a declared kind · the denominator for adoption.
    pub fn declared(&self) -> usize {
        self.units
            .iter()
            .filter(|u| u.kind != Kind::Undeclared)
            .count()
    }
}

/// Where the checked-in copy lives, relative to the repo root.
pub fn registry_path(root: &Path) -> PathBuf {
    root.join(".pipeline").join("primitives.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_read_from_the_module_header() {
        assert_eq!(
            declared_kind("//! a unit\n//! kind: primitive\n"),
            Kind::Primitive
        );
        assert_eq!(declared_kind("//! kind: component\n"), Kind::Component);
    }

    #[test]
    fn a_missing_declaration_is_undeclared_not_a_default_kind() {
        // ! Defaulting to any real kind would silently satisfy the direction
        // check for every unregistered file — the check would pass by ignorance.
        assert_eq!(
            declared_kind("//! no kind here\npub fn x() {}"),
            Kind::Undeclared
        );
        assert_eq!(declared_kind(""), Kind::Undeclared);
    }

    #[test]
    fn a_kind_line_below_the_header_is_not_a_declaration() {
        // Only the header counts · a `kind:` in prose further down is text.
        let src = "//! header\n\npub fn x() {}\n//! kind: primitive\n";
        assert_eq!(declared_kind(src), Kind::Undeclared);
    }

    #[test]
    fn direction_violation_is_reported() {
        let units = vec![
            Unit {
                id: "prim_crate::lib".to_owned(),
                kind: Kind::Primitive,
                path: "crates/prim-crate/src/lib.rs".to_owned(),
                exports: vec![],
                deps: vec!["part_crate".to_owned()],
                external_deps: vec![],
                lines: 10,
            },
            Unit {
                id: "part_crate::lib".to_owned(),
                kind: Kind::Part,
                path: "crates/part-crate/src/lib.rs".to_owned(),
                exports: vec![],
                deps: vec![],
                external_deps: vec![],
                lines: 10,
            },
        ];
        let v = check(&units);
        assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
        assert_eq!(v[0].rule, "dependency-direction");
    }

    #[test]
    fn downward_dependency_is_allowed() {
        let units = vec![
            Unit {
                id: "comp_crate::lib".to_owned(),
                kind: Kind::Component,
                path: "crates/comp-crate/src/lib.rs".to_owned(),
                exports: vec![],
                deps: vec!["prim_crate".to_owned()],
                external_deps: vec![],
                lines: 10,
            },
            Unit {
                id: "prim_crate::lib".to_owned(),
                kind: Kind::Primitive,
                path: "crates/prim-crate/src/lib.rs".to_owned(),
                exports: vec![],
                deps: vec![],
                external_deps: vec![],
                lines: 10,
            },
        ];
        assert!(
            check(&units).is_empty(),
            "skipping levels downward is legal"
        );
    }

    #[test]
    fn a_primitive_with_third_party_imports_is_reported() {
        let units = vec![Unit {
            id: "p::lib".to_owned(),
            kind: Kind::Primitive,
            path: "crates/p/src/lib.rs".to_owned(),
            exports: vec![],
            deps: vec![],
            external_deps: vec!["tokio".to_owned()],
            lines: 10,
        }];
        let v = check(&units);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "primitive-stdlib-only");
    }

    #[test]
    fn unit_id_is_the_module_path() {
        let root = Path::new("/repo");
        assert_eq!(
            unit_id(
                root,
                Path::new("/repo/crates/pipeline-mcp/src/handlers/run.rs")
            ),
            "pipeline_mcp::handlers::run"
        );
    }
}

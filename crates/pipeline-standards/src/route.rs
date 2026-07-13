//! Route a project → the standards that bind it.
//!
//! Executes ROUTER's rules; ✗ restates them. Every id here comes from
//! `index.json`. When ROUTER changes, this code does not.
//!
//! ```text
//! always-on (§3)  +  language route  +  project-type route (§5)  +  surface routes (§6)
//!                                     ↓ dedupe · tier load order
//!                                 RoutedSet
//! ```

use serde::Serialize;

use crate::index::Index;

/// A choose-one group the project has not settled, e.g. `go` | `rust` with no
/// matching language. Surfaced, ✗ guessed — picking silently would bind the
/// project to a standard it never opted into.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub options: Vec<String>,
    pub from: String,
}

/// A conditional add whose predicate Pipeline cannot evaluate, e.g.
/// "(if tool-serving)". Reported so the agent/user can opt in.
#[derive(Debug, Clone, Serialize)]
pub struct Conditional {
    pub add: Vec<String>,
    pub when: String,
    pub from: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RoutedSet {
    /// Bound standards, in tier load order.
    pub ids: Vec<String>,
    /// Why each id is in the set — `id → reasons`.
    pub because: Vec<Because>,
    pub decisions: Vec<Decision>,
    pub conditional: Vec<Conditional>,
    /// Route keys named in pipeline.yaml that ROUTER does not define.
    pub unknown_routes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Because {
    pub id: String,
    pub reasons: Vec<String>,
}

/// `stack.runtime` → language standard id. Unknown → no language route.
pub fn language_for_runtime(runtime: &str) -> Option<&'static str> {
    match runtime.trim().to_ascii_lowercase().as_str() {
        "rust" => Some("rust"),
        "python" | "python-uv" | "uv" => Some("python"),
        "bun" | "node" | "nodejs" | "typescript" | "ts" | "javascript" | "js" => Some("typescript"),
        "go" | "golang" => Some("go"),
        "shell" | "bash" | "sh" => Some("shell"),
        _ => None,
    }
}

/// Accumulates ids with provenance, deduping as it goes.
#[derive(Default)]
struct Acc {
    ids: Vec<String>,
    because: Vec<Because>,
}

impl Acc {
    fn add(&mut self, id: &str, reason: &str) {
        if let Some(b) = self.because.iter_mut().find(|b| b.id == id) {
            if !b.reasons.iter().any(|r| r == reason) {
                b.reasons.push(reason.to_owned());
            }
            return; // already selected — routing is idempotent (ROUTER §1)
        }
        self.ids.push(id.to_owned());
        self.because.push(Because {
            id: id.to_owned(),
            reasons: vec![reason.to_owned()],
        });
    }
}

/// Resolve the standards binding a project.
///
/// `languages` — explicit override; empty → derived from `stack.runtime`.
pub fn route(
    index: &Index,
    cfg: &pipeline_config::Standards,
    runtime: &str,
    warn_unknown_language: bool,
) -> RoutedSet {
    let mut acc = Acc::default();
    let mut out = RoutedSet::default();

    // 1. Always-On Set — non-negotiable, every project (ROUTER §3).
    for id in &index.always_on {
        acc.add(id, "always-on");
    }

    // 2. Language routes — explicit config wins over runtime inference.
    let languages: Vec<String> = if cfg.languages.is_empty() {
        language_for_runtime(runtime)
            .map(|l| vec![l.to_owned()])
            .unwrap_or_default()
    } else {
        cfg.languages.clone()
    };
    if languages.is_empty() && warn_unknown_language {
        out.unknown_routes
            .push(format!("stack.runtime '{runtime}' → no language standard"));
    }
    for lang in &languages {
        if index.get(lang).is_some() {
            acc.add(lang, "language");
        } else {
            out.unknown_routes.push(format!("language '{lang}'"));
        }
    }

    // 3. Project-type route (ROUTER §5).
    if let Some(key) = cfg.project_type.as_deref().filter(|k| !k.is_empty()) {
        match index.routes.by_type.get(key) {
            Some(r) => apply(&mut acc, &mut out, r, &languages, &format!("type:{key}")),
            None => out.unknown_routes.push(format!("project_type '{key}'")),
        }
    }

    // 4. Surface routes — one per interface the system exposes (ROUTER §6).
    for key in &cfg.surfaces {
        match index.routes.by_surface.get(key) {
            Some(r) => apply(&mut acc, &mut out, r, &languages, &format!("surface:{key}")),
            None => out.unknown_routes.push(format!("surface '{key}'")),
        }
    }

    out.ids = acc.ids;
    out.because = acc.because;
    index.sort_by_load_order(&mut out.ids);
    out.because.sort_by_key(|b| {
        out.ids
            .iter()
            .position(|i| *i == b.id)
            .unwrap_or(usize::MAX)
    });
    out
}

fn apply(
    acc: &mut Acc,
    out: &mut RoutedSet,
    r: &crate::index::Route,
    languages: &[String],
    reason: &str,
) {
    for id in &r.add {
        acc.add(id, reason);
    }

    // Alternatives: if the project's language already picks one, that settles it.
    // Otherwise it is a real decision — surface it, ✗ guess.
    for group in &r.alternatives {
        let picked: Vec<&String> = group.iter().filter(|g| languages.contains(g)).collect();
        if picked.is_empty() {
            out.decisions.push(Decision {
                options: group.clone(),
                from: reason.to_owned(),
            });
        } else {
            for id in picked {
                acc.add(id, reason);
            }
        }
    }

    for c in &r.conditional {
        out.conditional.push(Conditional {
            add: c.add.clone(),
            when: c.when.clone(),
            from: reason.to_owned(),
        });
    }
}

/// Fixtures shared with `inject`'s tests — a tiny stand-in for the real index.
#[cfg(test)]
pub mod tests_support {
    use crate::index::{Index, Route, Routes, Standard};
    use std::collections::BTreeMap;

    fn std_entry(id: &str, tier: &str) -> Standard {
        Standard {
            id: id.into(),
            domain: id.split('/').next().unwrap_or(id).into(),
            path: format!("{id}/STANDARDS.md"),
            title: format!("{id} Standards"),
            purpose: String::new(),
            tier: Some(tier.into()),
            version: Some("1.0".into()),
            lines: 100,
            owns: vec![],
            defers_to: vec![],
            load_with: vec![],
            checklist: vec![],
        }
    }

    pub fn fixture() -> Index {
        let mut by_type = BTreeMap::new();
        by_type.insert(
            "MCP server".into(),
            Route {
                add: vec!["local_mcp".into(), "cli".into()],
                alternatives: vec![vec!["python".into(), "typescript".into()]],
                conditional: vec![],
                raw: "`local_mcp` · `python` (or `typescript`) · `cli`".into(),
                trigger: None,
            },
        );
        by_type.insert(
            "REST service".into(),
            Route {
                add: vec!["api".into()],
                alternatives: vec![vec!["go".into(), "rust".into()]],
                conditional: vec![],
                raw: "`go` \\| `rust` · `api`".into(),
                trigger: None,
            },
        );
        let mut by_surface = BTreeMap::new();
        by_surface.insert(
            "Command line".into(),
            Route {
                add: vec!["cli".into()],
                ..Route::default()
            },
        );

        Index {
            schema: 1,
            tier_order: [
                "Foundation",
                "Core",
                "Delivery",
                "Interface",
                "Domain",
                "Language",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            always_on: vec!["architecture".into(), "testing".into()],
            routes: Routes {
                by_type,
                by_surface,
            },
            standards: vec![
                std_entry("architecture", "Foundation"),
                std_entry("testing", "Core"),
                std_entry("cli", "Interface"),
                std_entry("api", "Interface"),
                std_entry("local_mcp", "Domain"),
                std_entry("rust", "Language"),
                std_entry("python", "Language"),
                std_entry("typescript", "Language"),
                std_entry("go", "Language"),
            ],
        }
    }

    pub fn cfg(project_type: Option<&str>, surfaces: &[&str]) -> pipeline_config::Standards {
        pipeline_config::Standards {
            source: None,
            pin: None,
            project_type: project_type.map(str::to_owned),
            surfaces: surfaces.iter().map(|s| (*s).to_string()).collect(),
            languages: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{cfg, fixture};
    use super::*;

    #[test]
    fn always_on_binds_even_with_no_routes() {
        let r = route(&fixture(), &cfg(None, &[]), "rust", true);
        assert!(r.ids.contains(&"architecture".to_string()));
        assert!(r.ids.contains(&"testing".to_string()));
        assert!(r.ids.contains(&"rust".to_string()));
    }

    #[test]
    fn ids_come_back_in_tier_load_order() {
        let r = route(&fixture(), &cfg(Some("MCP server"), &[]), "rust", true);
        let rank: Vec<usize> = r.ids.iter().map(|i| fixture().tier_rank(i)).collect();
        let mut sorted = rank.clone();
        sorted.sort_unstable();
        assert_eq!(rank, sorted, "ids must be in Foundation→Language order");
        // Foundation first, Language last.
        assert_eq!(r.ids.first().unwrap(), "architecture");
        assert_eq!(r.ids.last().unwrap(), "rust");
    }

    #[test]
    fn language_settles_an_alternatives_group() {
        // `go` | `rust` + runtime=rust → rust selected, no decision surfaced.
        let r = route(&fixture(), &cfg(Some("REST service"), &[]), "rust", true);
        assert!(r.ids.contains(&"rust".to_string()));
        assert!(!r.ids.contains(&"go".to_string()));
        assert!(r.decisions.is_empty());
    }

    #[test]
    fn unsettled_alternatives_surface_as_a_decision_not_a_guess() {
        // MCP server offers python|typescript; runtime=rust matches neither.
        let r = route(&fixture(), &cfg(Some("MCP server"), &[]), "rust", true);
        assert_eq!(r.decisions.len(), 1);
        assert_eq!(r.decisions[0].options, vec!["python", "typescript"]);
        // ! neither is silently bound
        assert!(!r.ids.contains(&"python".to_string()));
        assert!(!r.ids.contains(&"typescript".to_string()));
    }

    #[test]
    fn routing_is_idempotent_and_records_every_reason() {
        // `cli` arrives from both the type route and the surface route.
        let r = route(
            &fixture(),
            &cfg(Some("MCP server"), &["Command line"]),
            "rust",
            true,
        );
        assert_eq!(r.ids.iter().filter(|i| *i == "cli").count(), 1);
        let cli = r.because.iter().find(|b| b.id == "cli").expect("cli bound");
        assert_eq!(
            cli.reasons.len(),
            2,
            "both routes recorded: {:?}",
            cli.reasons
        );
    }

    #[test]
    fn unknown_route_keys_are_reported_not_ignored() {
        let r = route(
            &fixture(),
            &cfg(Some("Fax machine"), &["Telegraph"]),
            "rust",
            true,
        );
        assert_eq!(r.unknown_routes.len(), 2);
        // ...and the always-on set still binds.
        assert!(r.ids.contains(&"architecture".to_string()));
    }

    #[test]
    fn unknown_runtime_yields_no_language_route() {
        let r = route(&fixture(), &cfg(None, &[]), "cobol", true);
        assert!(!r.unknown_routes.is_empty());
        assert!(!r.ids.iter().any(|i| i == "rust" || i == "python"));
    }

    #[test]
    fn runtime_aliases_map_to_one_language() {
        assert_eq!(language_for_runtime("python-uv"), Some("python"));
        assert_eq!(language_for_runtime("bun"), Some("typescript"));
        assert_eq!(language_for_runtime("Golang"), Some("go"));
        assert_eq!(language_for_runtime("cobol"), None);
    }
}

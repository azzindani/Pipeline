//! Context injection — three tiers, because the corpus does not fit a context window.
//!
//! The full corpus is ~19k lines (~250k tokens). Even one project's routed set
//! is far too large to paste. So inject by tier, cheapest first:
//!
//! | Tier | What | Cost | When |
//! |---|---|---|---|
//! | L0 `brief` | which standards bind · what each **Owns** | ~1–2 KB | every session, always |
//! | L1 `doc` | one full standard | ~15 KB | agent is about to touch that surface |
//! | L2 `checklists` | the routed set's Checklist sections | ~10% of full | gates · review · `check` |
//!
//! L0 is the load-bearing one: it tells an agent *which* rules bind it and *what
//! each governs* — enough to know when to pull L1. TEMPLATE.md mandates a
//! Checklist final section on every standard, which is what makes L2 mechanical.

use serde::Serialize;
use std::path::Path;

use crate::StandardsError;
use crate::index::Index;
use crate::resolve::Resolved;
use crate::route::RoutedSet;

/// L0 — the always-injected packet. Small enough to sit in every handover.
#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub sha: String,
    pub origin: String,
    pub root: String,
    /// Pin set but HEAD moved → gates may have shifted under this project.
    pub drifted: bool,
    /// No pin yet → caller should record `sha` into pipeline.yaml.
    pub unpinned: bool,
    pub count: usize,
    pub entries: Vec<BriefEntry>,
    pub decisions: Vec<crate::route::Decision>,
    pub conditional: Vec<crate::route::Conditional>,
    pub unknown_routes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefEntry {
    pub id: String,
    pub tier: String,
    pub version: String,
    pub path: String,
    /// What this standard is authoritative for — the routing signal for L1.
    pub owns: String,
    pub why: Vec<String>,
    pub checklist_items: usize,
}

pub fn brief(index: &Index, routed: &RoutedSet, resolved: &Resolved) -> Brief {
    let entries = routed
        .ids
        .iter()
        .filter_map(|id| {
            let s = index.get(id)?;
            let why = routed
                .because
                .iter()
                .find(|b| b.id == *id)
                .map(|b| b.reasons.clone())
                .unwrap_or_default();
            Some(BriefEntry {
                id: s.id.clone(),
                tier: s.tier.clone().unwrap_or_default(),
                version: s.version.clone().unwrap_or_default(),
                path: s.path.clone(),
                owns: s.owns.join(" · "),
                why,
                checklist_items: s.checklist.len(),
            })
        })
        .collect::<Vec<_>>();

    Brief {
        sha: resolved.sha.clone(),
        origin: format!("{:?}", resolved.origin).to_lowercase(),
        root: resolved.root.display().to_string(),
        drifted: resolved.is_drifted(),
        unpinned: resolved.is_unpinned(),
        count: entries.len(),
        entries,
        decisions: routed.decisions.clone(),
        conditional: routed.conditional.clone(),
        unknown_routes: routed.unknown_routes.clone(),
    }
}

/// Backtick-wrap and join — `a` · `b`.
fn ticked(items: &[String], sep: &str) -> String {
    items
        .iter()
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(sep)
}

impl Brief {
    /// Markdown rendering — what actually gets pasted into an agent's context.
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write as _;

        let mut s = String::new();
        s.push_str("# Standards in force\n\n");
        let _ = writeln!(
            s,
            "Source `{}` @ `{}` ({}). {} standards bind this project.\n",
            self.root,
            &self.sha[..self.sha.len().min(7)],
            self.origin,
            self.count
        );

        if self.drifted {
            s.push_str(
                "> ! DRIFT — the pinned commit and the checked-out corpus differ. \
                 Gates may have moved. Run `pipeline standards update` to move the pin \
                 deliberately, or restore the pin.\n\n",
            );
        }
        if self.unpinned {
            s.push_str(
                "> ! UNPINNED — no `standards.pin` in pipeline.yaml. \
                 Upstream changes will silently move this project's gates.\n\n",
            );
        }

        s.push_str("| Standard | Tier | Owns | Why |\n|---|---|---|---|\n");
        for e in &self.entries {
            let _ = writeln!(
                s,
                "| `{}` | {} | {} | {} |",
                e.id,
                e.tier,
                truncate(&e.owns, 90),
                e.why.join(", ")
            );
        }

        if !self.decisions.is_empty() {
            s.push_str("\n## Unresolved choices\n\n");
            for d in &self.decisions {
                let _ = writeln!(
                    s,
                    "- choose one of {} (from {}) — set `standards.languages` in pipeline.yaml",
                    ticked(&d.options, " | "),
                    d.from
                );
            }
        }

        if !self.conditional.is_empty() {
            s.push_str("\n## Conditional\n\n");
            for c in &self.conditional {
                let _ = writeln!(
                    s,
                    "- if **{}** → also load {}",
                    c.when,
                    ticked(&c.add, " · ")
                );
            }
        }

        if !self.unknown_routes.is_empty() {
            s.push_str("\n## Unknown route keys\n\n");
            for u in &self.unknown_routes {
                let _ = writeln!(s, "- {u} — not defined in ROUTER");
            }
        }

        s.push_str(
            "\nPull a full standard with `pipeline_standards.show(id)` before working on \
             the surface it owns.\n",
        );
        s
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// L1 — one standard, in full.
pub async fn doc(index: &Index, root: &Path, id: &str) -> Result<String, StandardsError> {
    let s = index
        .get(id)
        .ok_or_else(|| StandardsError::UnknownStandard { id: id.to_owned() })?;
    Ok(tokio::fs::read_to_string(root.join(&s.path)).await?)
}

#[derive(Debug, Clone, Serialize)]
pub struct Checklist {
    pub id: String,
    pub tier: String,
    pub items: Vec<String>,
}

/// L2 — the enforcement surface of the routed set. What `check` runs against.
pub fn checklists(index: &Index, routed: &RoutedSet) -> Vec<Checklist> {
    routed
        .ids
        .iter()
        .filter_map(|id| index.get(id))
        .filter(|s| !s.checklist.is_empty())
        .map(|s| Checklist {
            id: s.id.clone(),
            tier: s.tier.clone().unwrap_or_default(),
            items: s.checklist.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::Origin;
    use std::path::PathBuf;

    fn resolved(pin: Option<&str>) -> Resolved {
        Resolved {
            root: PathBuf::from("/root/Standards"),
            origin: Origin::Config,
            sha: "0828bd8aaaabbbbccccddddeeeeffff0000111122".into(),
            pin: pin.map(str::to_owned),
        }
    }

    #[test]
    fn truncate_is_char_safe_on_multibyte() {
        // Owns lines are full of · and → — byte slicing would panic here.
        let s = "ownership · borrowing → idioms · Result/Option/? mechanism";
        let t = truncate(s, 10);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 11);
    }

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("short", 90), "short");
    }

    #[test]
    fn drift_and_unpinned_are_shouted_in_the_brief() {
        let index = crate::route::tests_support::fixture();
        let routed = crate::route::route(
            &index,
            &crate::route::tests_support::cfg(None, &[]),
            "rust",
            true,
        );

        let b = brief(&index, &routed, &resolved(Some("deadbeef")));
        assert!(b.drifted);
        assert!(b.to_markdown().contains("DRIFT"));

        let b = brief(&index, &routed, &resolved(None));
        assert!(b.unpinned);
        assert!(b.to_markdown().contains("UNPINNED"));

        let b = brief(&index, &routed, &resolved(Some("0828bd8")));
        assert!(!b.drifted && !b.unpinned);
        let md = b.to_markdown();
        assert!(!md.contains("DRIFT") && !md.contains("UNPINNED"));
    }

    #[test]
    fn brief_stays_small_enough_to_always_inject() {
        let index = crate::route::tests_support::fixture();
        let routed = crate::route::route(
            &index,
            &crate::route::tests_support::cfg(Some("MCP server"), &["Command line"]),
            "rust",
            true,
        );
        let md = brief(&index, &routed, &resolved(Some("0828bd8"))).to_markdown();
        // ! L0 is injected on EVERY session — it must never balloon.
        assert!(md.len() < 4096, "brief was {} bytes", md.len());
        assert!(md.contains("Standards in force"));
    }
}

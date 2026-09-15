//! `index.json` — the machine-readable contract emitted by Standards' CI.
//!
//! Produced by `tools/validate.py --emit-index` in the Standards repo, off a
//! corpus that already passed TEMPLATE.md conformance. CI runs `--check-index`,
//! so a stale index fails the Standards build → what Pipeline reads here is
//! always in lockstep with the markdown.
//!
//! ✗ parse ROUTER.md or STANDARDS.md headers from Rust. The producer owns the
//! schema; this crate is a consumer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::StandardsError;

/// index.json schema this crate understands. Bump → handle migration explicitly.
pub const SUPPORTED_SCHEMA: u32 = 1;

pub const INDEX_FILE: &str = "index.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Index {
    pub schema: u32,
    /// Foundation → Core → Delivery → Interface → Domain → Language (ROUTER §2).
    #[serde(default)]
    pub tier_order: Vec<String>,
    /// Non-negotiable set — every project, every size (ROUTER §3).
    #[serde(default)]
    pub always_on: Vec<String>,
    #[serde(default)]
    pub routes: Routes,
    #[serde(default)]
    pub standards: Vec<Standard>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Routes {
    /// ROUTER §5 — keyed by project type, e.g. "MCP server".
    #[serde(default)]
    pub by_type: BTreeMap<String, Route>,
    /// ROUTER §6 — keyed by surface, e.g. "Command line".
    #[serde(default)]
    pub by_surface: BTreeMap<String, Route>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Route {
    /// Unconditional additions.
    #[serde(default)]
    pub add: Vec<String>,
    /// Choose-one groups, e.g. `go` | `rust` — resolved against the project's
    /// languages, or surfaced as a decision the agent must make.
    #[serde(default)]
    pub alternatives: Vec<Vec<String>>,
    /// Applies only when a named condition holds, e.g. "tool-serving".
    #[serde(default)]
    pub conditional: Vec<Conditional>,
    /// Original ROUTER cell — the escape hatch when structure loses nuance.
    #[serde(default)]
    pub raw: String,
    /// ROUTER §6 only: what makes this surface apply.
    #[serde(default)]
    pub trigger: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Conditional {
    pub add: Vec<String>,
    pub when: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Standard {
    /// `rust` · `testing/pressure` — path-derived, unique.
    pub id: String,
    pub domain: String,
    /// Repo-relative, e.g. `rust/STANDARDS.md`.
    pub path: String,
    pub title: String,
    pub purpose: String,
    pub tier: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub lines: u32,
    /// Topics this standard is authoritative for (ROUTER §8 conflict resolution).
    #[serde(default)]
    pub owns: Vec<String>,
    #[serde(default)]
    pub defers_to: Vec<Defer>,
    #[serde(default)]
    pub load_with: Vec<String>,
    /// The enforcement surface — TEMPLATE.md mandates a Checklist final section.
    #[serde(default)]
    pub checklist: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Defer {
    pub topic: String,
    pub to: String,
}

impl Index {
    /// Load + schema-check `<root>/index.json`.
    pub async fn load(root: &Path) -> Result<Self, StandardsError> {
        let path = root.join(INDEX_FILE);
        let text =
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| StandardsError::IndexMissing {
                    path: path.display().to_string(),
                    source: e,
                })?;

        let index: Self = serde_json::from_str(&text)?;
        if index.schema != SUPPORTED_SCHEMA {
            return Err(StandardsError::SchemaMismatch {
                found: index.schema,
                supported: SUPPORTED_SCHEMA,
            });
        }
        index.validate(&path.display().to_string())?;
        Ok(index)
    }

    /// Refuse an index that parses but holds nothing to route with.
    ///
    /// ! The schema number alone proved nothing. Every field below carries
    /// `#[serde(default)]`, so `{"schema":1}` deserialized cleanly and routing
    /// then bound zero standards and reported it as a SUCCESS — `list` returned
    /// `total: 0`, `brief` returned `count: 0`, and an agent read "no standards
    /// apply" as an answer rather than as a broken corpus.
    ///
    /// ! Absent ✗ empty. A Standards repo always has standards, always has a
    /// tier order, and always has an always-on set — those are invariants of the
    /// producer, so their absence is a malformed index, ✗ a project without
    /// obligations.
    fn validate(&self, path: &str) -> Result<(), StandardsError> {
        let missing: Vec<&str> = [
            ("standards", self.standards.is_empty()),
            ("tier_order", self.tier_order.is_empty()),
            ("always_on", self.always_on.is_empty()),
        ]
        .iter()
        .filter(|(_, empty)| *empty)
        .map(|(name, _)| *name)
        .collect();

        if missing.is_empty() {
            return Ok(());
        }
        Err(StandardsError::IndexHollow {
            path: path.to_owned(),
            missing: missing.join(" · "),
        })
    }

    pub fn get(&self, id: &str) -> Option<&Standard> {
        self.standards.iter().find(|s| s.id == id)
    }

    /// Rank of a standard's tier in load order. Unknown tier → last.
    pub fn tier_rank(&self, id: &str) -> usize {
        let tier = self.get(id).and_then(|s| s.tier.as_deref()).unwrap_or("");
        self.tier_order
            .iter()
            .position(|t| t == tier)
            .unwrap_or(self.tier_order.len())
    }

    /// Sort ids into ROUTER's load order — later tiers assume earlier ones hold.
    pub fn sort_by_load_order(&self, ids: &mut [String]) {
        ids.sort_by(|a, b| {
            self.tier_rank(a)
                .cmp(&self.tier_rank(b))
                .then_with(|| a.cmp(b))
        });
    }
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    /// A corpus that parses but binds nothing.
    ///
    /// ! The regression: every field here carries `#[serde(default)]`, so
    /// `{"schema":1}` deserialized cleanly, routing bound zero standards, and
    /// `list` reported `total: 0` with `ok: true`. An agent read "no standards
    /// apply" as an answer instead of as a broken corpus.
    #[test]
    fn a_hollow_index_is_refused_rather_than_read_as_zero_obligations() {
        let index: Index = serde_json::from_str(r#"{"schema":1}"#).expect("parses");
        let err = index.validate("/x/index.json").expect_err("must refuse");
        let msg = err.to_string();
        for field in ["standards", "tier_order", "always_on"] {
            assert!(msg.contains(field), "refusal must name '{field}': {msg}");
        }
    }

    /// Each invariant is checked separately · a corpus missing only one of them
    /// is just as unroutable as one missing all three.
    #[test]
    fn each_missing_invariant_is_named_on_its_own() {
        let cases = [
            (
                r#"{"schema":1,"tier_order":["Core"],"always_on":["a"]}"#,
                "standards",
            ),
            (
                r#"{"schema":1,"always_on":["a"],"standards":[{"id":"a","domain":"a","path":"a/STANDARDS.md","title":"A","purpose":"p"}]}"#,
                "tier_order",
            ),
            (
                r#"{"schema":1,"tier_order":["Core"],"standards":[{"id":"a","domain":"a","path":"a/STANDARDS.md","title":"A","purpose":"p"}]}"#,
                "always_on",
            ),
        ];
        for (json, expected) in cases {
            let index: Index = serde_json::from_str(json).expect("parses");
            let msg = index
                .validate("/x/index.json")
                .expect_err("must refuse")
                .to_string();
            assert!(msg.contains(expected), "expected '{expected}' in: {msg}");
        }
    }

    #[test]
    fn a_populated_index_validates() {
        let index: Index = serde_json::from_str(
            r#"{"schema":1,"tier_order":["Foundation","Core"],"always_on":["architecture"],
                "standards":[{"id":"architecture","domain":"architecture",
                "path":"architecture/STANDARDS.md","title":"A","purpose":"p"}]}"#,
        )
        .expect("parses");
        index.validate("/x/index.json").expect("must accept");
    }
}

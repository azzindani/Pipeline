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
        Ok(index)
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

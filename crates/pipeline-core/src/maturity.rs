//! Maturity level · computed from evidence present.
//!
//! kind: component
//!
//! Implements the level model from the maturity standard. The whole point is
//! that the level is **computed**, ✗ declared: a team asserting level 4 with no
//! chaos evidence is the failure mode the model exists to prevent.
//!
//! ! Absent evidence is never a pass. A gate configured but never executed
//! provides nothing, so `Unknown` and `Failed` are treated identically here —
//! both mean the level was not reached.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// State of one piece of evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Evidence {
    Present,
    Failed,
    /// Never captured. Distinct from `Failed` for reporting — identical for
    /// scoring, because neither is a pass.
    Absent,
}

impl Evidence {
    fn is_pass(self) -> bool {
        self == Self::Present
    }
}

/// The six levels, from the standard's §2.
pub const LEVEL_REQUIREMENTS: [(u8, &str, &[&str]); 6] = [
    (0, "compiles", &["build", "lint", "format", "typecheck"]),
    (1, "unit-proven", &["unit_tests", "coverage_gate"]),
    (
        2,
        "assembled",
        &["image_build", "services_healthy", "integration_tests"],
    ),
    (
        3,
        "observed",
        &[
            "e2e",
            "endpoint_contract",
            "visual_baseline",
            "a11y",
            "real_fixtures",
        ],
    ),
    (
        4,
        "pressured",
        &["load", "stress_to_failure", "chaos", "real_data_eval"],
    ),
    (
        5,
        "measured",
        &[
            "motion_baseline",
            "motion_compare",
            "resource_metrics",
            "budgets_enforced",
        ],
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelReport {
    /// Highest level whose requirements — and every lower level's — all hold.
    pub level: u8,
    pub level_name: String,
    /// Exactly what is missing for `level + 1`. ✗ "more testing needed".
    pub missing_for_next: Vec<String>,
    /// Requirements failing at | below the current level, which would lower it.
    pub regressions: Vec<String>,
}

/// Compute the level from the evidence present.
///
/// ! A level is a floor, ✗ a badge: level 4 requires every level 0–3
/// requirement still passing. Scanning upward and stopping at the first
/// incomplete level is what enforces that.
pub fn level(evidence: &BTreeMap<String, Evidence>) -> LevelReport {
    let state = |k: &str| evidence.get(k).copied().unwrap_or(Evidence::Absent);

    let mut attained = 0u8;
    let mut name = "compiles";
    let mut first_gap: Vec<String> = Vec::new();

    for (lvl, lvl_name, reqs) in LEVEL_REQUIREMENTS {
        let gaps: Vec<String> = reqs
            .iter()
            .filter(|r| !state(r).is_pass())
            .map(|r| (*r).to_owned())
            .collect();

        if gaps.is_empty() {
            attained = lvl;
            name = lvl_name;
            continue;
        }
        // Level 0 incomplete → the project is below every level. Reported as 0
        // with its gaps, ✗ as a negative or an error.
        first_gap = gaps;
        if lvl == 0 {
            name = "below level 0";
        }
        break;
    }

    let regressions: Vec<String> = LEVEL_REQUIREMENTS
        .iter()
        .filter(|(lvl, _, _)| *lvl <= attained)
        .flat_map(|(_, _, reqs)| reqs.iter())
        .filter(|r| state(r) == Evidence::Failed)
        .map(|r| (*r).to_owned())
        .collect();

    LevelReport {
        level: attained,
        level_name: name.to_owned(),
        missing_for_next: first_gap,
        regressions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn through(level: u8) -> BTreeMap<String, Evidence> {
        let mut m = BTreeMap::new();
        for (lvl, _, reqs) in LEVEL_REQUIREMENTS {
            if lvl > level {
                break;
            }
            for r in reqs {
                m.insert((*r).to_owned(), Evidence::Present);
            }
        }
        m
    }

    #[test]
    fn a_project_with_nothing_is_below_level_zero() {
        let r = level(&BTreeMap::new());
        assert_eq!(r.level, 0);
        assert_eq!(r.level_name, "below level 0");
        assert!(r.missing_for_next.contains(&"build".to_owned()));
    }

    #[test]
    fn the_level_is_the_highest_complete_one() {
        let r = level(&through(2));
        assert_eq!(r.level, 2);
        assert_eq!(r.level_name, "assembled");
    }

    #[test]
    fn a_level_is_a_floor_not_a_badge() {
        // ! Every level 5 requirement present, but chaos (level 4) missing.
        // Claiming 5 here is exactly the assertion the model exists to refuse.
        let mut m = through(5);
        m.remove("chaos");
        let r = level(&m);
        assert_eq!(r.level, 3, "a gap at level 4 caps the project at 3");
        assert_eq!(r.missing_for_next, vec!["chaos".to_owned()]);
    }

    #[test]
    fn absent_evidence_is_never_a_pass() {
        let mut m = through(1);
        m.remove("coverage_gate");
        assert_eq!(level(&m).level, 0, "a missing gate cannot count as passing");
    }

    #[test]
    fn a_configured_but_failing_gate_is_not_a_pass() {
        let mut m = through(1);
        m.insert("coverage_gate".to_owned(), Evidence::Failed);
        let r = level(&m);
        assert_eq!(r.level, 0);
        assert!(r.missing_for_next.contains(&"coverage_gate".to_owned()));
    }

    #[test]
    fn a_failure_below_the_attained_level_is_reported_as_a_regression() {
        let mut m = through(3);
        m.insert("lint".to_owned(), Evidence::Failed);
        let r = level(&m);
        // Level drops, and the specific failing requirement is named.
        assert_eq!(r.level, 0);
        assert!(r.missing_for_next.contains(&"lint".to_owned()));
    }

    #[test]
    fn missing_evidence_is_named_specifically() {
        // "more testing needed" is ✗ a finding · the exact keys are.
        let m = through(2);
        let r = level(&m);
        assert_eq!(r.level, 2);
        assert!(r.missing_for_next.contains(&"e2e".to_owned()));
        assert!(r.missing_for_next.contains(&"a11y".to_owned()));
    }

    #[test]
    fn every_requirement_key_is_unique_across_levels() {
        // A key reused across levels would make one level's pass satisfy
        // another's requirement by accident.
        let mut seen = std::collections::BTreeSet::new();
        for (_, _, reqs) in LEVEL_REQUIREMENTS {
            for r in reqs {
                assert!(seen.insert(*r), "duplicate requirement key: {r}");
            }
        }
    }
}

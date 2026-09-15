//! Motion metrics · baselines · regression comparison.
//!
//! kind: component
//!
//! Motion reduced to scalars, per the maturity standard's numeric-evidence
//! rule. A reviewing agent has 2D static vision and video costs more per frame
//! than it returns, so anything that lives in movement is measured and compared
//! as numbers before it is reviewed.
//!
//! ! The schema is deliberately independent of how frames are captured. Browser
//! frame timings, a rendered scene, and a physical actuator produce the same
//! fields, so a second capture backend is a backend — ✗ a second action, ✗ a
//! changed contract.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which direction is an improvement. A gate that does not know this cannot
/// fail correctly — it would flag a latency drop as a regression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Lower is better · latency · jank · dropped frames.
    Lower,
    /// Higher is better · throughput · frame rate.
    Higher,
}

/// One measurement. A bare number is ✗ a measurement — the unit travels with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub value: f64,
    pub unit: String,
    pub direction: Direction,
}

impl Metric {
    pub fn ms(value: f64, direction: Direction) -> Self {
        Self {
            value,
            unit: "ms".to_owned(),
            direction,
        }
    }

    pub fn count(value: f64) -> Self {
        Self {
            value,
            unit: "count".to_owned(),
            direction: Direction::Lower,
        }
    }
}

/// What the measurement was taken under. Two numbers captured under different
/// conditions are ✗ comparable, so the conditions are part of the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conditions {
    /// Free-form label for the machine class · comparison across classes is
    /// invalid and is refused, ✗ warned about.
    pub hardware_class: String,
    /// Declared target frame interval in ms · 60 fps → 16.7. Declared, ✗
    /// inferred from the capture, or a slow capture would lower its own bar.
    pub target_frame_interval_ms: f64,
    pub viewport: Option<String>,
    pub concurrency: Option<u32>,
}

/// One capture of one route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionRecord {
    pub route: String,
    pub environment: String,
    pub captured_at: String,
    pub conditions: Conditions,
    pub metrics: BTreeMap<String, Metric>,
    /// Paths to screenshots | traces. Supporting evidence, referenced by the
    /// record — ✗ the finding itself.
    #[serde(default)]
    pub artifacts: Vec<String>,
}

/// Allowed movement for one metric. Both bounds may apply; the wider one wins,
/// so a tiny absolute value cannot be tripped by percentage noise.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub absolute: Option<f64>,
    pub percent: Option<f64>,
}

impl Budget {
    pub fn percent(p: f64) -> Self {
        Self {
            absolute: None,
            percent: Some(p),
        }
    }

    /// How far a metric may move from `baseline` before it counts as a
    /// regression. No bound declared → any worsening is a regression.
    fn tolerance(self, baseline: f64) -> f64 {
        let abs = self.absolute.unwrap_or(0.0);
        let pct = self
            .percent
            .map_or(0.0, |p| (baseline.abs() * p / 100.0).abs());
        abs.max(pct)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDelta {
    pub metric: String,
    pub baseline: f64,
    pub current: f64,
    pub delta: f64,
    pub unit: String,
    pub regressed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ComparisonOutcome {
    Pass,
    Regressed,
    /// Comparison could not be made · reported as a refusal, ✗ as a pass.
    Incomparable(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub outcome: ComparisonOutcome,
    pub deltas: Vec<MetricDelta>,
    /// Metrics in the baseline with no counterpart in the current run.
    pub missing: Vec<String>,
}

/// Compare a capture against a baseline.
///
/// ! Refuses rather than passes in every ambiguous case. A comparison that
/// cannot be trusted must be visible: reporting `Pass` when the hardware class
/// differs, or when a metric vanished, is how a gate silently stops gating.
pub fn compare(
    baseline: &MotionRecord,
    current: &MotionRecord,
    budgets: &BTreeMap<String, Budget>,
) -> Comparison {
    if baseline.conditions.hardware_class != current.conditions.hardware_class {
        return Comparison {
            outcome: ComparisonOutcome::Incomparable(format!(
                "hardware class differs · baseline '{}' vs current '{}'",
                baseline.conditions.hardware_class, current.conditions.hardware_class
            )),
            deltas: Vec::new(),
            missing: Vec::new(),
        };
    }
    if (baseline.conditions.target_frame_interval_ms - current.conditions.target_frame_interval_ms)
        .abs()
        > f64::EPSILON
    {
        return Comparison {
            outcome: ComparisonOutcome::Incomparable(format!(
                "target frame interval differs · baseline {} ms vs current {} ms",
                baseline.conditions.target_frame_interval_ms,
                current.conditions.target_frame_interval_ms
            )),
            deltas: Vec::new(),
            missing: Vec::new(),
        };
    }

    let mut deltas = Vec::new();
    let mut missing = Vec::new();

    for (name, base) in &baseline.metrics {
        let Some(now) = current.metrics.get(name) else {
            // A metric present in the baseline and absent now is missing
            // evidence, ✗ an improvement worth zero.
            missing.push(name.clone());
            continue;
        };

        let delta = now.value - base.value;
        let worsened = match base.direction {
            Direction::Lower => delta,
            Direction::Higher => -delta,
        };
        let tolerance = budgets
            .get(name)
            .copied()
            .unwrap_or(Budget {
                absolute: None,
                percent: None,
            })
            .tolerance(base.value);

        deltas.push(MetricDelta {
            metric: name.clone(),
            baseline: base.value,
            current: now.value,
            delta,
            unit: now.unit.clone(),
            regressed: worsened > tolerance,
        });
    }

    let outcome = if !missing.is_empty() {
        ComparisonOutcome::Incomparable(format!(
            "metrics absent from the current capture: {}",
            missing.join(" · ")
        ))
    } else if deltas.iter().any(|d| d.regressed) {
        ComparisonOutcome::Regressed
    } else {
        ComparisonOutcome::Pass
    };

    Comparison {
        outcome,
        deltas,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conditions() -> Conditions {
        Conditions {
            hardware_class: "ci-standard".to_owned(),
            target_frame_interval_ms: 16.7,
            viewport: Some("1280x720".to_owned()),
            concurrency: None,
        }
    }

    fn record(values: &[(&str, f64, Direction)]) -> MotionRecord {
        MotionRecord {
            route: "/".to_owned(),
            environment: "staging".to_owned(),
            captured_at: "2026-09-14T00:00:00Z".to_owned(),
            conditions: conditions(),
            metrics: values
                .iter()
                .map(|(n, v, d)| ((*n).to_owned(), Metric::ms(*v, *d)))
                .collect(),
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn a_worsening_beyond_budget_regresses() {
        let base = record(&[("input_latency_p95", 100.0, Direction::Lower)]);
        let now = record(&[("input_latency_p95", 130.0, Direction::Lower)]);
        let budgets = BTreeMap::from([("input_latency_p95".to_owned(), Budget::percent(10.0))]);

        let c = compare(&base, &now, &budgets);
        assert_eq!(c.outcome, ComparisonOutcome::Regressed);
        assert!(c.deltas[0].regressed);
        assert!((c.deltas[0].delta - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_worsening_inside_budget_passes() {
        let base = record(&[("input_latency_p95", 100.0, Direction::Lower)]);
        let now = record(&[("input_latency_p95", 105.0, Direction::Lower)]);
        let budgets = BTreeMap::from([("input_latency_p95".to_owned(), Budget::percent(10.0))]);
        assert_eq!(
            compare(&base, &now, &budgets).outcome,
            ComparisonOutcome::Pass
        );
    }

    #[test]
    fn direction_decides_what_counts_as_worse() {
        // ! Without direction, a throughput DROP reads as an improvement because
        // the raw delta is negative. This is the check that catches it.
        let base = record(&[("frames_per_second", 60.0, Direction::Higher)]);
        let now = record(&[("frames_per_second", 40.0, Direction::Higher)]);
        let budgets = BTreeMap::from([("frames_per_second".to_owned(), Budget::percent(5.0))]);

        let c = compare(&base, &now, &budgets);
        assert_eq!(
            c.outcome,
            ComparisonOutcome::Regressed,
            "a 20 fps drop must regress"
        );
    }

    #[test]
    fn improving_never_regresses_however_large() {
        let base = record(&[("input_latency_p95", 100.0, Direction::Lower)]);
        let now = record(&[("input_latency_p95", 10.0, Direction::Lower)]);
        let c = compare(&base, &now, &BTreeMap::new());
        assert_eq!(c.outcome, ComparisonOutcome::Pass);
        assert!(!c.deltas[0].regressed);
    }

    #[test]
    fn no_budget_means_any_worsening_regresses() {
        let base = record(&[("jank_events", 1.0, Direction::Lower)]);
        let now = record(&[("jank_events", 2.0, Direction::Lower)]);
        assert_eq!(
            compare(&base, &now, &BTreeMap::new()).outcome,
            ComparisonOutcome::Regressed
        );
    }

    #[test]
    fn a_different_hardware_class_is_incomparable_not_a_pass() {
        let base = record(&[("input_latency_p95", 100.0, Direction::Lower)]);
        let mut now = record(&[("input_latency_p95", 100.0, Direction::Lower)]);
        now.conditions.hardware_class = "dev-laptop".to_owned();

        match compare(&base, &now, &BTreeMap::new()).outcome {
            ComparisonOutcome::Incomparable(why) => assert!(why.contains("hardware class")),
            other => panic!("cross-class comparison must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_different_target_interval_is_incomparable() {
        let base = record(&[("frame_interval_p95", 16.0, Direction::Lower)]);
        let mut now = record(&[("frame_interval_p95", 16.0, Direction::Lower)]);
        now.conditions.target_frame_interval_ms = 33.3;
        assert!(matches!(
            compare(&base, &now, &BTreeMap::new()).outcome,
            ComparisonOutcome::Incomparable(_)
        ));
    }

    #[test]
    fn a_vanished_metric_is_missing_evidence_not_a_pass() {
        // ! The quiet failure mode: capture stops emitting a metric, every
        // comparison goes green, and the gate has stopped gating.
        let base = record(&[
            ("input_latency_p95", 100.0, Direction::Lower),
            ("jank_events", 0.0, Direction::Lower),
        ]);
        let now = record(&[("input_latency_p95", 90.0, Direction::Lower)]);

        let c = compare(&base, &now, &BTreeMap::new());
        assert_eq!(c.missing, vec!["jank_events".to_owned()]);
        assert!(matches!(c.outcome, ComparisonOutcome::Incomparable(_)));
    }

    #[test]
    fn the_wider_of_two_bounds_wins() {
        // A 1 ms absolute floor protects a small metric from percentage noise.
        let base = record(&[("time_to_first_paint", 4.0, Direction::Lower)]);
        let now = record(&[("time_to_first_paint", 5.0, Direction::Lower)]);
        let budgets = BTreeMap::from([(
            "time_to_first_paint".to_owned(),
            Budget {
                absolute: Some(2.0),
                percent: Some(5.0),
            },
        )]);
        assert_eq!(
            compare(&base, &now, &budgets).outcome,
            ComparisonOutcome::Pass
        );
    }
}

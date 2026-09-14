//! Resource and efficiency metrics.
//!
//! kind: component
//!
//! The cost half of the maturity standard's level 5. Motion says whether the
//! system feels right; this says what that costs and how much headroom is left.
//!
//! ! Efficiency is a ratio, ✗ an absolute. A change that is faster but consumes
//! more per unit of work is a regression wearing an improvement's clothes, and
//! only the ratio shows it.

use serde::{Deserialize, Serialize};

/// What one unit of work consumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceRecord {
    /// Which stage | command this describes. Whole-run totals cannot locate a
    /// regression, so capture is per stage.
    pub label: String,
    pub wall_time_ms: u64,
    pub cpu_seconds: f64,
    pub memory_peak_mb: f64,
    /// Constraint profile in force · `None` = unconstrained.
    pub constraint: Option<String>,
    /// Units of work completed · tests run, requests served, rows processed.
    /// Absent → efficiency is not computable and is reported as such, ✗ as 0.
    pub work_units: Option<f64>,
}

/// Efficiency ratios. Every field is `None` when its inputs are missing —
/// a zero would read as "measured, and terrible".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Efficiency {
    pub label: String,
    pub work_per_cpu_second: Option<f64>,
    pub work_per_wall_second: Option<f64>,
    pub memory_per_work_unit_mb: Option<f64>,
}

impl ResourceRecord {
    /// Ratios for this record · `None` where the inputs do not support one.
    pub fn efficiency(&self) -> Efficiency {
        let work = self.work_units.filter(|w| *w > 0.0);
        Efficiency {
            label: self.label.clone(),
            work_per_cpu_second: work
                .filter(|_| self.cpu_seconds > 0.0)
                .map(|w| w / self.cpu_seconds),
            work_per_wall_second: work
                .filter(|_| self.wall_time_ms > 0)
                // ! Precision: a wall time large enough to lose f64 mantissa
                // bits is ~10^5 years. The cast is exact for every real value.
                .map(|w| w / (f64::from(u32::try_from(self.wall_time_ms).unwrap_or(u32::MAX)) / 1000.0)),
            memory_per_work_unit_mb: work.map(|w| self.memory_peak_mb / w),
        }
    }
}

/// Verdict when comparing two resource captures of the same work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EfficiencyVerdict {
    Improved,
    Unchanged,
    /// Faster in wall time but worse per unit resource — the case an
    /// absolute-only comparison would wave through.
    RegressedDespiteSpeedup,
    Regressed,
    Incomparable(String),
}

/// Compare efficiency, ✗ raw speed.
///
/// ! Constraint profiles must match. Comparing a throttled run against an
/// unconstrained one measures the throttle, not the change.
pub fn compare_efficiency(
    baseline: &ResourceRecord,
    current: &ResourceRecord,
    tolerance_percent: f64,
) -> EfficiencyVerdict {
    if baseline.constraint != current.constraint {
        return EfficiencyVerdict::Incomparable(format!(
            "constraint profile differs · baseline {:?} vs current {:?}",
            baseline.constraint, current.constraint
        ));
    }
    let (Some(base), Some(now)) = (
        baseline.efficiency().work_per_cpu_second,
        current.efficiency().work_per_cpu_second,
    ) else {
        return EfficiencyVerdict::Incomparable(
            "work_units missing · efficiency is a ratio and needs a denominator".to_owned(),
        );
    };

    let change_pct = (now - base) / base * 100.0;
    if change_pct.abs() <= tolerance_percent {
        return EfficiencyVerdict::Unchanged;
    }
    if change_pct > 0.0 {
        return EfficiencyVerdict::Improved;
    }
    // Efficiency fell. Whether wall time improved decides which failure this is.
    if current.wall_time_ms < baseline.wall_time_ms {
        EfficiencyVerdict::RegressedDespiteSpeedup
    } else {
        EfficiencyVerdict::Regressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(cpu: f64, wall: u64, work: Option<f64>) -> ResourceRecord {
        ResourceRecord {
            label: "unit".to_owned(),
            wall_time_ms: wall,
            cpu_seconds: cpu,
            memory_peak_mb: 100.0,
            constraint: None,
            work_units: work,
        }
    }

    #[test]
    fn efficiency_is_work_over_resource() {
        let e = record(2.0, 1000, Some(100.0)).efficiency();
        assert_eq!(e.work_per_cpu_second, Some(50.0));
        assert_eq!(e.work_per_wall_second, Some(100.0));
    }

    #[test]
    fn missing_work_units_yields_none_not_zero() {
        // ! Zero would read as "measured, and terrible" and could trip a gate on
        // a run that simply did not report its denominator.
        let e = record(2.0, 1000, None).efficiency();
        assert_eq!(e.work_per_cpu_second, None);
        assert_eq!(e.memory_per_work_unit_mb, None);
    }

    #[test]
    fn zero_work_units_is_treated_as_missing() {
        let e = record(2.0, 1000, Some(0.0)).efficiency();
        assert_eq!(
            e.work_per_cpu_second, None,
            "division by zero must not surface as a ratio"
        );
    }

    #[test]
    fn faster_but_less_efficient_is_a_regression() {
        // ! The case this module exists for. Wall time improved 20%, but the
        // work done per CPU-second fell by a third — more cores burned for a
        // smaller total. An absolute-only comparison calls this a win.
        let base = record(2.0, 1000, Some(100.0)); // 50 work/cpu-s
        let now = record(3.0, 800, Some(100.0)); // 33.3 work/cpu-s
        assert_eq!(
            compare_efficiency(&base, &now, 5.0),
            EfficiencyVerdict::RegressedDespiteSpeedup
        );
    }

    #[test]
    fn slower_and_less_efficient_is_a_plain_regression() {
        let base = record(2.0, 1000, Some(100.0));
        let now = record(3.0, 1400, Some(100.0));
        assert_eq!(
            compare_efficiency(&base, &now, 5.0),
            EfficiencyVerdict::Regressed
        );
    }

    #[test]
    fn a_small_change_inside_tolerance_is_unchanged() {
        let base = record(2.0, 1000, Some(100.0));
        let now = record(2.02, 1000, Some(100.0));
        assert_eq!(
            compare_efficiency(&base, &now, 5.0),
            EfficiencyVerdict::Unchanged
        );
    }

    #[test]
    fn a_throttled_run_is_incomparable_to_an_unconstrained_one() {
        // Comparing across constraint profiles measures the throttle, ✗ the change.
        let base = record(2.0, 1000, Some(100.0));
        let mut now = record(2.0, 1000, Some(100.0));
        now.constraint = Some("cpu-50pct".to_owned());
        assert!(matches!(
            compare_efficiency(&base, &now, 5.0),
            EfficiencyVerdict::Incomparable(_)
        ));
    }

    #[test]
    fn an_improvement_is_reported_as_one() {
        let base = record(4.0, 1000, Some(100.0));
        let now = record(2.0, 1000, Some(100.0));
        assert_eq!(
            compare_efficiency(&base, &now, 5.0),
            EfficiencyVerdict::Improved
        );
    }
}

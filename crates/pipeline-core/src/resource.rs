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
    fn measuring_a_real_command_yields_real_numbers() {
        let m = measure(
            "sleep",
            "sh",
            &["-c".to_owned(), "sleep 0.2".to_owned()],
            None,
            None,
        )
        .expect("spawn");
        assert_eq!(m.exit_code, Some(0));
        assert!(
            m.record.wall_time_ms >= 180,
            "wall time {} ms is below the 200 ms the command slept",
            m.record.wall_time_ms
        );
    }

    #[test]
    fn a_failing_command_is_measured_rather_than_erroring() {
        // ! A non-zero exit is a result, ✗ a measurement failure. Returning Err
        // here would lose the resource numbers for exactly the runs worth
        // investigating.
        let m = measure(
            "fail",
            "sh",
            &["-c".to_owned(), "exit 3".to_owned()],
            None,
            None,
        )
        .expect("spawn");
        assert_eq!(m.exit_code, Some(3));
    }

    #[test]
    fn stdout_is_captured_for_the_caller_to_parse_work_units_from() {
        let m = measure(
            "echo",
            "sh",
            &["-c".to_owned(), "echo 42".to_owned()],
            None,
            Some(42.0),
        )
        .expect("spawn");
        assert_eq!(m.stdout.trim(), "42");
        assert_eq!(m.record.work_units, Some(42.0));
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

// ── capture ─────────────────────────────────────────────────────────────────

/// Cumulative CPU seconds this process's *reaped children* have consumed.
///
/// ! `cutime` + `cstime` from `/proc/self/stat`, fields 16 and 17 after `comm`.
/// Only reaped children count, so a delta taken around a `wait` is that child's
/// CPU time — and only if no other child is reaped concurrently, which is why
/// [`measure`] takes `&mut` access to the notion of "one measurement at a time".
///
/// Linux only. Elsewhere → `None`, and the record reports CPU as unmeasured
/// rather than as zero: a zero would read as "measured, and free".
fn children_cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // `comm` is parenthesised and may contain spaces — split after the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After comm, field 1 is state · cutime is the 13th, cstime the 14th.
    let child_user: f64 = fields.get(12)?.parse().ok()?;
    let child_system: f64 = fields.get(13)?.parse().ok()?;
    Some((child_user + child_system) / clock_ticks_per_second())
}

/// `sysconf(_SC_CLK_TCK)` · 100 on every mainstream Linux. Read from the
/// environment where a caller needs to override it, ✗ hardcoded silently.
fn clock_ticks_per_second() -> f64 {
    std::env::var("PIPELINE_CLK_TCK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100.0)
}

/// Peak resident memory of a running pid, in MB · `VmHWM` from `/proc/<pid>/status`.
fn peak_memory_mb(pid: u32) -> Option<f64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

/// What a capture could not measure · reported, ✗ silently zeroed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unmeasured {
    CpuSeconds,
    MemoryPeak,
}

/// One measured command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub record: ResourceRecord,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Fields the platform would not yield. Non-empty → the record's
    /// corresponding numbers are placeholders, ✗ measurements.
    pub unmeasured: Vec<Unmeasured>,
}

/// Run a command and measure what it consumed.
///
/// ! Peak memory is sampled while the child runs, so a command that exits
/// faster than the first sample reports no peak. That is recorded in
/// `unmeasured`, ✗ reported as 0 MB — the difference matters when the number
/// feeds a budget.
pub fn measure(
    label: &str,
    program: &str,
    args: &[String],
    constraint: Option<String>,
    work_units: Option<f64>,
) -> std::io::Result<Measurement> {
    use std::process::{Command, Stdio};

    let cpu_before = children_cpu_seconds();
    let started = std::time::Instant::now();

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let pid = child.id();
    let mut peak_mb: Option<f64> = None;
    // Sample until the child is gone. Cheap: a read of one small procfs file.
    while child.try_wait()?.is_none() {
        if let Some(mb) = peak_memory_mb(pid) {
            peak_mb = Some(peak_mb.map_or(mb, |p: f64| p.max(mb)));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let out = child.wait_with_output()?;
    let wall_time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let cpu_after = children_cpu_seconds();

    let mut unmeasured = Vec::new();
    let cpu_seconds = if let (Some(before), Some(after)) = (cpu_before, cpu_after) {
        (after - before).max(0.0)
    } else {
        unmeasured.push(Unmeasured::CpuSeconds);
        0.0
    };
    let memory_peak_mb = peak_mb.unwrap_or_else(|| {
        unmeasured.push(Unmeasured::MemoryPeak);
        0.0
    });

    Ok(Measurement {
        record: ResourceRecord {
            label: label.to_owned(),
            wall_time_ms,
            cpu_seconds,
            memory_peak_mb,
            constraint,
            work_units,
        },
        exit_code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        unmeasured,
    })
}

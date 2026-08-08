//! The `--soak` harness: the instrument two perf budgets have named since P4.
//!
//! [`docs/perf-budgets.md`] cites `--soak` twice — as the method for "Idle RSS
//! (flyout closed) <= 35 MB private" and as the whole of "Soak (24 h) RSS growth
//! < 5 MB; flat GDI/USER handle counts" — and until P8 wave 3 there was no such
//! flag. Two hard budgets were unmeasurable by the method their own row named,
//! which is the false-assurance shape this project has a rule against: a
//! maintainer reads the row, believes the budget is checked, and it never was.
//!
//! # What it runs, and what that does and does not prove
//!
//! The **real** pipeline, assembled exactly as `--headless` does — platform
//! event pump, engine, controllers — and then left **idle**. That is deliberate
//! and it is the budget's own definition: ADR-0005 says threads park on `recv`
//! and `docs/perf-budgets.md` says "0 periodic wakeups ... no polling loops
//! anywhere". An idle soak is the test of that design. A leak in the event pump,
//! a timer somebody added, a channel that accumulates, or a handle taken per
//! wake and never released all show up here.
//!
//! What it does **not** prove is that a *busy* Duja is leak-free. A soak that
//! drives level changes and hot-plug for hours is a different harness and does
//! not exist; `--stress` floods for seconds, which is a throughput test rather
//! than a leak test. Said plainly here because "soak passed" is otherwise read
//! as more than it is.
//!
//! # The split
//!
//! Everything that decides — the warm-up window, the growth arithmetic, the
//! verdict — is pure and unit-tested on every lane. Only the sampling touches
//! the OS ([`duja_platform::process`]), and the report is honest about the
//! platform that cannot sample: macOS reports "unavailable" rather than zero.
//!
//! [`docs/perf-budgets.md`]: https://github.com/itabajah/duja/blob/main/docs/perf-budgets.md

use std::fmt;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use duja_platform::process::{self, ProcessMetrics};

use crate::bin_support::{backend, run};

/// `docs/perf-budgets.md`: "Idle RSS (flyout closed) <= 35 MB private".
///
/// Read as **MiB**, and written in bytes for the reason ADR-0012's P8 section
/// spells out at length: "35 MB" is two numbers 5 % apart and the budget went
/// four gates without saying which. MiB is the looser reading and it is also the
/// one a Windows user reproduces, because Task Manager labels MiB as MB.
pub(crate) const IDLE_RSS_BUDGET_BYTES: u64 = 35 * 1024 * 1024;

/// `docs/perf-budgets.md`: "Soak (24 h) RSS growth < 5 MB", same reading.
pub(crate) const RSS_GROWTH_BUDGET_BYTES: u64 = 5 * 1024 * 1024;

/// How much GDI/USER handle drift counts as "flat".
///
/// **Not zero, and not measured either.** [D-005](https://github.com/itabajah/duja/blob/main/docs/debt.md#d-005)
/// is this project's standing example of a harness that gates on absolute zero
/// and reports FAIL on a healthy run, so zero is the wrong default. But the
/// opposite failure is a threshold so loose it never fires, and the honest
/// position is that nobody has run this for 24 hours yet, so there is no
/// measured drift to set it from. Eight is a starting point chosen to be small
/// enough that a per-wake leak trips it within an hour: at one handle leaked per
/// display-change event this fires long before the 10,000-object limit Windows
/// enforces. **The first long run should replace it with a measured number**,
/// and that is a task rather than a hope - it is in the soak's own report.
pub(crate) const HANDLE_GROWTH_TOLERANCE: u32 = 8;

/// The longest warm-up window: allocations settle, the engine finishes its
/// initial probes, and the allocator reaches steady state.
const MAX_WARMUP: Duration = Duration::from_mins(1);

/// One sample of this process's usage at a point in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sample {
    /// Time since the run started.
    pub(crate) elapsed: Duration,
    /// What the OS reported, or `None` on a platform that cannot say.
    pub(crate) metrics: Option<ProcessMetrics>,
}

/// How long to ignore before taking the growth baseline.
///
/// A fixed 60 seconds would make every short run report its own warm-up as a
/// leak; a fixed fraction would give a 24-hour run a 2.4-hour warm-up and hide
/// a slow leak in it. So: a tenth of the run, capped at a minute.
pub(crate) fn warmup(total: Duration) -> Duration {
    // `checked_div` rather than `/`: the divisor is a literal 10 and cannot be
    // zero, but `arithmetic_side_effects` does not know that and a lint suppressed
    // here would have to be re-justified every time this function is edited.
    total.checked_div(10).unwrap_or(MAX_WARMUP).min(MAX_WARMUP)
}

/// A finished soak, ready to render and to decide the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SoakReport {
    /// Requested duration.
    pub(crate) duration: Duration,
    /// Every sample taken, in order.
    pub(crate) samples: Vec<Sample>,
    /// How many displays the pipeline saw at the start.
    pub(crate) displays: usize,
}

/// What a finished soak concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every budget held.
    Pass,
    /// At least one budget was exceeded.
    Fail,
    /// This platform cannot read its own metrics, so nothing was measured.
    /// **Not a pass**: a harness that reports success when it measured nothing
    /// is the exact failure this module exists to remove.
    Unmeasurable,
}

impl SoakReport {
    /// The first sample taken at or after the warm-up window, which is the
    /// baseline growth is measured from. `None` if the run ended before the
    /// warm-up did.
    pub(crate) fn baseline(&self) -> Option<&Sample> {
        let after = warmup(self.duration);
        self.samples.iter().find(|s| s.elapsed >= after)
    }

    /// The last sample with real metrics.
    pub(crate) fn last_measured(&self) -> Option<&Sample> {
        self.samples.iter().rev().find(|s| s.metrics.is_some())
    }

    /// Peak resident bytes across the whole run.
    pub(crate) fn peak_rss(&self) -> Option<u64> {
        self.samples
            .iter()
            .filter_map(|s| s.metrics.map(|m| m.rss_bytes))
            .max()
    }

    /// RSS growth from the baseline to the end, in bytes. Saturating: a run that
    /// *shrank* reports zero growth rather than an underflow.
    pub(crate) fn rss_growth(&self) -> Option<u64> {
        let base = self.baseline()?.metrics?.rss_bytes;
        let last = self.last_measured()?.metrics?.rss_bytes;
        Some(last.saturating_sub(base))
    }

    /// GDI and USER handle growth from the baseline, where the platform counts
    /// them at all.
    pub(crate) fn handle_growth(&self) -> (Option<u32>, Option<u32>) {
        let Some(base) = self.baseline().and_then(|s| s.metrics) else {
            return (None, None);
        };
        let Some(last) = self.last_measured().and_then(|s| s.metrics) else {
            return (None, None);
        };
        (
            base.gdi_objects
                .zip(last.gdi_objects)
                .map(|(b, l)| l.saturating_sub(b)),
            base.user_objects
                .zip(last.user_objects)
                .map(|(b, l)| l.saturating_sub(b)),
        )
    }

    /// The verdict, and every reason it is not [`Verdict::Pass`].
    ///
    /// Returns the reasons rather than only the verdict so the report can print
    /// all of them: a run that breaks two budgets and prints one teaches half of
    /// what it cost to learn.
    pub(crate) fn verdict(&self) -> (Verdict, Vec<String>) {
        let mut reasons = Vec::new();
        if self.last_measured().is_none() {
            return (
                Verdict::Unmeasurable,
                vec![
                    "this platform cannot read its own resource usage, so nothing was measured \
                     (see duja_platform::process)"
                        .to_owned(),
                ],
            );
        }
        if self.baseline().is_none() {
            return (
                Verdict::Unmeasurable,
                vec![format!(
                    "the run ended before its {:?} warm-up did, so there is no baseline to \
                     measure growth from",
                    warmup(self.duration)
                )],
            );
        }

        if let Some(peak) = self.peak_rss()
            && peak > IDLE_RSS_BUDGET_BYTES
        {
            reasons.push(format!(
                "peak RSS {peak} bytes exceeds the {IDLE_RSS_BUDGET_BYTES}-byte idle budget"
            ));
        }
        if let Some(growth) = self.rss_growth()
            && growth >= RSS_GROWTH_BUDGET_BYTES
        {
            reasons.push(format!(
                "RSS grew {growth} bytes from the baseline; the budget is under \
                 {RSS_GROWTH_BUDGET_BYTES}"
            ));
        }
        let (gdi, user) = self.handle_growth();
        for (label, growth) in [("GDI", gdi), ("USER", user)] {
            if let Some(growth) = growth
                && growth > HANDLE_GROWTH_TOLERANCE
            {
                reasons.push(format!(
                    "{label} objects grew by {growth}; the tolerance is \
                     {HANDLE_GROWTH_TOLERANCE}"
                ));
            }
        }

        if reasons.is_empty() {
            (Verdict::Pass, reasons)
        } else {
            (Verdict::Fail, reasons)
        }
    }
}

/// Run the soak for `secs`, sampling every `interval_secs`, and report.
///
/// # Errors
/// Bubbles a failure to start the platform event pump. A soak that cannot start
/// the pump is not a soak of Duja.
pub(crate) fn run(secs: u64, interval_secs: u64) -> anyhow::Result<ExitCode> {
    let duration = Duration::from_secs(secs);
    let interval = Duration::from_secs(interval_secs.max(1));
    let displays = backend::discover().len();

    // The same assembly `--headless` uses, and then nothing. See the module
    // header on why doing nothing is the test.
    let (tick_rx, mut forwarder) = run::start_platform()?;
    let (engine, notifications) = duja_app::Engine::spawn(
        duja_app::EngineConfig::default(),
        run::enumerator(),
        run::controller_factory(),
        tick_rx,
    );
    // Drain notifications rather than printing them: hours of output is not a
    // report, and an undrained channel is itself a leak this harness would then
    // be measuring instead of Duja.
    let drain = std::thread::spawn(move || while notifications.recv().is_ok() {});

    eprintln!(
        "duja soak: {displays} display(s), running {secs}s, sampling every {interval_secs}s. \
         Warm-up is {:?}; growth is measured from the first sample after it.",
        warmup(duration)
    );

    let started = Instant::now();
    let mut samples = Vec::new();
    loop {
        let elapsed = started.elapsed();
        let sample = Sample {
            elapsed,
            metrics: process::self_metrics(),
        };
        eprintln!("{}", render_sample(&sample));
        samples.push(sample);
        if elapsed >= duration {
            break;
        }
        // `elapsed()` again rather than a fixed sleep: sampling and printing
        // take time, and over 24 hours a fixed interval drifts far enough that
        // "every 60s" stops being true.
        let remaining = duration.saturating_sub(started.elapsed());
        std::thread::sleep(interval.min(remaining).max(Duration::from_millis(1)));
    }

    engine.shutdown();
    forwarder.shutdown();
    let _ = drain.join();

    let report = SoakReport {
        duration,
        samples,
        displays,
    };
    print!("{report}");
    match report.verdict().0 {
        Verdict::Pass => Ok(ExitCode::SUCCESS),
        // Both non-pass arms are non-zero, and `Unmeasurable` is deliberately
        // NOT success: a harness that exits 0 having measured nothing is the
        // false assurance this module was built to remove.
        Verdict::Fail | Verdict::Unmeasurable => Ok(ExitCode::from(1)),
    }
}

/// One sample as a log line.
fn render_sample(sample: &Sample) -> String {
    let secs = sample.elapsed.as_secs();
    match sample.metrics {
        None => format!("  t+{secs:>6}s  (this platform cannot read its own usage)"),
        Some(m) => {
            let gdi = m
                .gdi_objects
                .map_or_else(|| "-".to_owned(), |v| v.to_string());
            let user = m
                .user_objects
                .map_or_else(|| "-".to_owned(), |v| v.to_string());
            format!(
                "  t+{secs:>6}s  rss {:>12} bytes  gdi {gdi:>5}  user {user:>5}",
                m.rss_bytes
            )
        }
    }
}

impl fmt::Display for SoakReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (verdict, reasons) = self.verdict();
        writeln!(f, "\n--- duja soak report ---")?;
        writeln!(f, "displays         {}", self.displays)?;
        writeln!(f, "duration         {:?}", self.duration)?;
        writeln!(f, "samples          {}", self.samples.len())?;
        writeln!(f, "warm-up          {:?}", warmup(self.duration))?;
        match self.peak_rss() {
            Some(peak) => writeln!(
                f,
                "peak RSS         {peak} bytes (budget {IDLE_RSS_BUDGET_BYTES})"
            )?,
            None => writeln!(f, "peak RSS         unavailable on this platform")?,
        }
        match self.rss_growth() {
            Some(growth) => writeln!(
                f,
                "RSS growth       {growth} bytes (budget under {RSS_GROWTH_BUDGET_BYTES})"
            )?,
            None => writeln!(f, "RSS growth       not measured")?,
        }
        let (gdi, user) = self.handle_growth();
        for (label, growth) in [("GDI growth", gdi), ("USER growth", user)] {
            match growth {
                Some(g) => writeln!(f, "{label:<16} {g} (tolerance {HANDLE_GROWTH_TOLERANCE})")?,
                None => writeln!(f, "{label:<16} not counted on this platform")?,
            }
        }
        writeln!(
            f,
            "verdict          {}",
            match verdict {
                Verdict::Pass => "PASS",
                Verdict::Fail => "FAIL",
                Verdict::Unmeasurable => "UNMEASURABLE",
            }
        )?;
        for reason in &reasons {
            writeln!(f, "  - {reason}")?;
        }
        if verdict == Verdict::Pass && self.duration >= Duration::from_hours(1) {
            writeln!(
                f,
                "\nThis run has a number for handle drift. `HANDLE_GROWTH_TOLERANCE` is a\n\
                 guess (see its docs); replace it with what this measured."
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(rss: u64, gdi: u32, user: u32) -> ProcessMetrics {
        ProcessMetrics {
            rss_bytes: rss,
            gdi_objects: Some(gdi),
            user_objects: Some(user),
        }
    }

    /// A run of `count` samples spread evenly across `duration`, whose metrics
    /// come from `f`.
    fn report(
        duration: Duration,
        count: u32,
        f: impl Fn(u32) -> Option<ProcessMetrics>,
    ) -> SoakReport {
        let samples = (0..count)
            .map(|i| Sample {
                elapsed: duration
                    .checked_div(count.saturating_sub(1).max(1))
                    .unwrap_or_default()
                    .saturating_mul(i),
                metrics: f(i),
            })
            .collect();
        SoakReport {
            duration,
            samples,
            displays: 1,
        }
    }

    #[test]
    fn warmup_is_a_tenth_of_the_run_capped_at_a_minute() {
        assert_eq!(warmup(Duration::from_mins(1)), Duration::from_secs(6));
        assert_eq!(warmup(Duration::from_mins(10)), Duration::from_mins(1));
        // The cap is what stops a 24-hour run spending 2.4 hours warming up and
        // hiding a slow leak inside it.
        assert_eq!(warmup(Duration::from_hours(24)), Duration::from_mins(1));
        assert_eq!(warmup(Duration::ZERO), Duration::ZERO);
    }

    /// Growth is measured from **after** the warm-up, so the allocator settling
    /// is not reported as a leak. Without the baseline this run reads as
    /// +8 MB and fails; from the baseline it is flat.
    #[test]
    fn warm_up_growth_is_not_counted_as_a_leak() {
        let run = report(Duration::from_secs(100), 11, |i| {
            let rss = if i == 0 { 20_000_000 } else { 28_000_000 };
            Some(metrics(rss, 10, 10))
        });
        assert_eq!(run.rss_growth(), Some(0));
        assert_eq!(run.verdict().0, Verdict::Pass);
    }

    #[test]
    fn a_leak_after_the_warm_up_fails() {
        let run = report(Duration::from_secs(100), 11, |i| {
            Some(metrics(20_000_000 + u64::from(i) * 1_000_000, 10, 10))
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail);
        assert!(
            reasons.iter().any(|r| r.contains("RSS grew")),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_run_that_shrank_reports_no_growth_rather_than_underflowing() {
        let run = report(Duration::from_secs(100), 11, |i| {
            Some(metrics(
                30_000_000_u64.saturating_sub(u64::from(i) * 100_000),
                10,
                10,
            ))
        });
        assert_eq!(run.rss_growth(), Some(0));
    }

    #[test]
    fn a_handle_leak_fails_even_when_memory_is_flat() {
        let run = report(Duration::from_secs(100), 11, |i| {
            Some(metrics(20_000_000, 10 + i, 10))
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail);
        assert!(reasons.iter().any(|r| r.contains("GDI")), "{reasons:?}");
    }

    /// The `#[D-005]` lesson: a harness that gates on absolute zero reports FAIL
    /// on a healthy run. Drift inside the tolerance is a pass.
    #[test]
    fn handle_drift_inside_the_tolerance_is_not_a_failure() {
        let run = report(Duration::from_secs(100), 11, |i| {
            Some(metrics(20_000_000, 10 + u32::from(i % 2 == 0), 10))
        });
        assert_eq!(run.verdict().0, Verdict::Pass);
    }

    /// Both budgets broken must both be reported. A run that costs hours and
    /// teaches half of what it found is a run half wasted.
    #[test]
    fn every_broken_budget_is_reported_not_just_the_first() {
        let run = report(Duration::from_secs(100), 11, |i| {
            Some(metrics(
                40_000_000 + u64::from(i) * 1_000_000,
                10 + i * 2,
                10 + i * 2,
            ))
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail);
        assert_eq!(reasons.len(), 4, "{reasons:?}");
    }

    /// The one that matters most: a platform that cannot measure must not
    /// report success. `--soak` exits non-zero here.
    #[test]
    fn a_platform_that_cannot_measure_is_not_a_pass() {
        let run = report(Duration::from_secs(100), 11, |_| None);
        assert_eq!(run.verdict().0, Verdict::Unmeasurable);
        assert_ne!(run.verdict().0, Verdict::Pass);
    }

    /// And a run too short to clear its own warm-up has no baseline, so it has
    /// measured nothing either.
    #[test]
    fn a_run_shorter_than_its_warm_up_is_unmeasurable() {
        let run = SoakReport {
            duration: Duration::from_mins(10),
            samples: vec![Sample {
                elapsed: Duration::ZERO,
                metrics: Some(metrics(20_000_000, 10, 10)),
            }],
            displays: 1,
        };
        assert_eq!(run.verdict().0, Verdict::Unmeasurable);
    }

    /// A platform that reports RSS but counts no GUI objects (Linux) is
    /// measurable, and its handle rows say "not counted" rather than zero.
    #[test]
    fn missing_handle_counts_do_not_make_a_run_unmeasurable() {
        let run = report(Duration::from_secs(100), 11, |_| {
            Some(ProcessMetrics {
                rss_bytes: 20_000_000,
                gdi_objects: None,
                user_objects: None,
            })
        });
        assert_eq!(run.verdict().0, Verdict::Pass);
        assert_eq!(run.handle_growth(), (None, None));
        assert!(run.to_string().contains("not counted on this platform"));
    }

    #[test]
    fn peak_rss_over_the_idle_budget_fails_even_with_no_growth() {
        let run = report(Duration::from_secs(100), 11, |i| {
            // A spike in the middle that returns: growth is zero, peak is not.
            let rss = if i == 5 { 40_000_000 } else { 20_000_000 };
            Some(metrics(rss, 10, 10))
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail);
        assert!(
            reasons.iter().any(|r| r.contains("peak RSS")),
            "{reasons:?}"
        );
    }
}

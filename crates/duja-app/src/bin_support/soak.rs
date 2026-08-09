//! The `--soak` harness: the instrument two perf budgets have named since P4.
//!
//! [`docs/perf-budgets.md`] cites `--soak` twice — for the idle-RSS row and for
//! "Soak (24 h) RSS growth < 5 MB; flat GDI/USER handle counts" — and until P8
//! wave 3 there was no such flag. Two budgets were unmeasurable by the method
//! their own row named, which is the false-assurance shape this project has a
//! rule against: a maintainer reads the row, believes the budget is checked, and
//! it never was.
//!
//! # Where the report goes, which on Windows is not the console
//!
//! **A release `duja.exe` is a GUI-subsystem binary** (`main.rs`'s
//! `windows_subsystem = "windows"`), so it has no console: `eprintln!` lands on
//! an invalid handle, std maps that to `Ok`, and every line this harness prints
//! is silently discarded. The shell does not wait for it either, so the exit
//! code — the whole of the "UNMEASURABLE is not a pass" guarantee — is
//! unobservable too. `main.rs` has said this since P4 and P8 wave 3 wrote past
//! it, which made the instrument useless on the one build worth soaking: the
//! release one, since that is what the 35 MB budget is written against.
//!
//! So the report is also **written to a file** — `soak-report.txt` beside the
//! rotating log — and the path is printed. A run whose output went nowhere still
//! leaves the numbers on disk. `docs/qa-checklist.md` carries the invocation
//! that also gets you the exit code.
//!
//! # What it runs
//!
//! `backend::discover()` for a display count, then the three pieces
//! `run::headless` assembles: `start_platform`, the engine, the IPC server. So
//! it is headless **plus** that one enumeration, which matters for the peak
//! below.
//!
//! Then it goes **idle**. That is the budget's own definition rather than a
//! shortcut: ADR-0005 parks every thread on `recv` and the design rule is "no
//! polling loops anywhere", so an idle soak tests exactly that.
//!
//! # Four things it does not measure, said plainly
//!
//! - **A busy Duja.** A soak that drives level changes and hot-plug for hours is
//!   a different harness and does not exist. `--stress` floods for seconds,
//!   which is a throughput test.
//! - **The tray process.** The idle-RSS budget says "flyout closed", which is a
//!   *tray-mode* state; this assembles no Slint shell, no tray icon and no
//!   window, so what it reports is the **headless** process. That is a lower
//!   bound on the tray build, useful for growth (a leak in the engine or the
//!   pump shows up in both) and **not** a substitute for the absolute number the
//!   budget row asks for. The row says so too.
//! - **Private memory.** See [`duja_platform::process`]: what the OS hands back
//!   is the *whole* resident set, private plus resident shareable pages. Against
//!   a budget written as "private" that over-counts, which is the safe direction
//!   to be wrong in, but it is not the same number.
//! - **GUI objects, in any meaningful sense.** This is the one an earlier version
//!   of this header got backwards. It justified including the IPC server as "the
//!   single most plausible source of the handle leak the GDI/USER counters exist
//!   to catch" — but a named-pipe instance is a *kernel* handle, which
//!   `GetGuiResources` does not count at all. What moves those counters is
//!   `CreateWindowExW`, `CreateSolidBrush` and the per-ramp device contexts, all
//!   of which live in the overlay dimmer and the gamma sink, and this harness
//!   builds neither. `duja_platform`'s own note that a headless Duja "reports
//!   exactly 0 GDI objects" is the same fact from the other side, and a run on
//!   this box confirms it: **GDI 0 and USER 5**, unchanged across ninety
//!   seconds. So the GUI half of the budget is structurally near-zero here and
//!   passing it is weak evidence rather than strong.
//!
//!   **Kernel handles are counted now, which is the half that was missing.**
//!   `GetProcessHandleCount` on Windows and `/proc/self/fd` on Linux, and runs
//!   on this box report **around 250** of them — the pipe server, the log file,
//!   the threads, the things this harness actually builds. That is the counter a
//!   leaked pipe instance moves, and before P9 wave 3 nothing watched it. It is
//!   also the first family here that is not perfectly flat: it falls a handful
//!   over ninety seconds, which is why the report names a fall instead of
//!   saturating it to zero.
//!   `docs/debt.md` D-112 carries what is still open: the overlay and gamma
//!   objects, which need a harness that dims a real screen for the duration.
//!
//! # The split
//!
//! Everything that decides — the warm-up window, the growth arithmetic, the
//! verdict — is pure and unit-tested on every lane. Only the sampling touches
//! the OS, and the report is honest about the platform that cannot sample:
//! macOS reports `UNMEASURABLE`, with a non-zero exit, rather than zero.
//!
//! [`docs/perf-budgets.md`]: https://github.com/itabajah/duja/blob/main/docs/perf-budgets.md

use std::fmt;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use duja_platform::process::{self, ProcessMetrics};

use crate::bin_support::{backend, run};

/// `docs/perf-budgets.md`: "Idle RSS (flyout closed) <= 35 MB private".
///
/// Read as **decimal MB** — 35,000,000 — which is the *tighter* of the two
/// readings. The binary budget in ADR-0012 took the looser one because the
/// measured value cleared both and disambiguating must not smuggle in a
/// tightening; the same reasoning applies in reverse here. The headless process
/// measures around 17.6 MB, so the strict reading also clears with room, and
/// choosing it means this wave loosened nothing it was not asked to loosen.
///
/// Measured again in P9 wave 3: **16.07 to 16.25 MB** peak across more than a
/// dozen runs on this box, so the headroom is still large.
///
/// **One further run reported 31.3 MB and nobody can say why.** It was
/// `duja --soak 90 --every 10`; its samples, verbatim: `t+0s 16084992`, then
/// `t+10s 31322112` and flat at that figure to the end. It was the first
/// execution of a freshly linked binary,
/// and a first draft of this note wrote that condition down as the finding - but
/// a reviewer then ran exactly that condition twice more and got 16.24 MB and
/// 16.11 MB, so the condition is **withdrawn** rather than repeated. What is
/// left is one anomalous run, its numbers, and no mechanism: `WorkingSetSize`
/// counts resident *shareable* pages so a cold image is conceivable, and a step
/// appearing at the second sample and holding is not what page-in looks like.
/// Recorded because an absolute RSS figure from a single run evidently can be
/// twice the settled one, and this row is where the next person who sees it
/// should add their sample.
pub(crate) const IDLE_RSS_BUDGET_BYTES: u64 = 35_000_000;

/// `docs/perf-budgets.md`: "Soak (24 h) RSS growth < 5 MB", same reading.
pub(crate) const RSS_GROWTH_BUDGET_BYTES: u64 = 5_000_000;

/// How much handle drift this harness *fails* on, in any of the three families.
///
/// **The budget row says "flat", and this is not that.** It is an operational
/// threshold, it is a guess rather than a measurement, and the two facts are
/// related: nobody has run this for 24 hours, so there is no measured idle drift
/// to set a real one from. [D-005](https://github.com/itabajah/duja/blob/main/docs/debt.md#d-005)
/// is this project's standing example of the opposite mistake — a harness gating
/// on absolute zero and reporting FAIL on a healthy run — so zero is the wrong
/// default too.
///
/// Eight is chosen so that a per-wake leak trips it within an hour, long before
/// the 10,000-object ceiling Windows enforces. Because it is looser than the
/// budget, [`SoakRun`]'s report **names any non-zero drift even when it passes,
/// in either direction**: the run must not be able to report "flat" for
/// something that moved. The first long run should replace this with what it
/// measured.
///
/// **The reasoning above is the GUI families', and the kernel family inherited
/// it without earning it.** The 10,000-object quota is the GDI/USER per-process
/// limit; kernel handles have a ceiling three orders of magnitude higher, so
/// "long before the ceiling" is not the argument there.
///
/// And where GDI and USER measure exactly flat on every headless run recorded,
/// the kernel count moves. Every drift measured on this box has been **negative
/// and no larger than five**, with the within-run spread reaching nine once. A
/// fall of any size passes, because the comparison is one-sided rather than
/// because five is inside eight - a first version of this paragraph said "within
/// the tolerance, but only just", which is wrong arithmetic (nine is not inside
/// eight) about the wrong mechanism.
///
/// **The risk worth naming is the other direction, and it is unmeasured.** If
/// the count can wander by five downward it can plausibly wander upward too, and
/// a rise of nine would FAIL a healthy run - which is precisely the
/// [D-005](https://github.com/itabajah/duja/blob/main/docs/debt.md#d-005) shape
/// this constant's own docs cite as the mistake to avoid. Nobody has run long
/// enough to know the upward spread. One threshold for three families with
/// different and mostly unmeasured noise floors is a placeholder, and this is
/// the paragraph that says so.
pub(crate) const HANDLE_GROWTH_TOLERANCE: u32 = 8;

/// The longest warm-up window: allocations settle, the engine finishes its
/// initial probes, and the allocator reaches steady state.
const MAX_WARMUP: Duration = Duration::from_mins(1);

/// One sample of this process's usage at a point in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sample {
    /// Time since the run started.
    pub(crate) elapsed: Duration,
    /// What the OS reported, or `None` on a platform (or a call) that could not
    /// say.
    pub(crate) metrics: Option<ProcessMetrics>,
}

/// How long to ignore before taking the growth baseline.
///
/// A fixed 60 seconds would make every short run report its own warm-up as a
/// leak; a fixed fraction would give a 24-hour run a 2.4-hour warm-up and hide a
/// slow leak inside it. So: a tenth of the run, capped at a minute.
pub(crate) fn warmup(total: Duration) -> Duration {
    // `checked_div` because `arithmetic_side_effects` is a workspace lint that CI
    // promotes to an error (`-D warnings`), and it does not know the divisor is a
    // literal. The `None` arm is
    // unreachable (`Duration::checked_div` fails only on a zero divisor); `ZERO`
    // is the conservative fallback anyway, since it measures more rather than
    // less.
    total
        .checked_div(10)
        .unwrap_or(Duration::ZERO)
        .min(MAX_WARMUP)
}

/// A soak, accumulated as it runs.
///
/// Deliberately **O(1) in the run length**: keeping every sample would mean a
/// `Vec` growing to several megabytes over a 24-hour run at a one-second
/// interval, inside the very process whose memory growth is being budgeted at
/// five. The harness must not be the thing it measures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SoakRun {
    /// Requested duration.
    duration: Duration,
    /// Displays the pipeline saw at the start.
    displays: usize,
    /// Whether the IPC server actually started.
    ///
    /// `ipc::start` returns `None` rather than an error when the endpoint is
    /// taken, and **any already-running Duja holds it** - which is exactly the
    /// situation the QA checklist describes ("on an idle desktop, from a real
    /// tray build's box"). Without recording this, a run could report PASS for an
    /// assembly smaller than the one this module argues for, leaving only a WARN
    /// line that on a release Windows build goes nowhere at all.
    ipc_started: bool,
    /// Samples taken, readable or not.
    samples: usize,
    /// Samples the OS could not answer.
    unreadable: usize,
    /// Highest resident bytes seen at or after the warm-up. This is the budgeted
    /// one.
    peak_rss: Option<u64>,
    /// Highest resident bytes seen *before* the warm-up. Reported, never gated:
    /// it is the startup residue the warm-up exists to exclude.
    startup_peak_rss: Option<u64>,
    /// The first **readable** sample at or after the warm-up window.
    baseline: Option<Sample>,
    /// The last **readable** sample.
    last: Option<Sample>,
}

/// Handle drift from the baseline, one field per family, **signed**.
///
/// **A struct rather than a tuple**, because there were two families and now
/// there are three, and this project already has a row about a positional pair
/// of same-typed values getting transposed silently.
///
/// A `None` **field** means the platform counts no such family, and only that:
/// a *failed* read makes the whole sample unreadable rather than a half-filled
/// one, on every platform (see [`duja_platform::process`]). A
/// [`HandleGrowth::default()`] - every field `None` - additionally comes back
/// from [`SoakRun::handle_growth`] when there is no baseline to subtract from,
/// so a run that measured nothing prints "not counted on this platform" about a
/// platform that counts all three. That is cosmetic rather than misleading,
/// because such a run is `UNMEASURABLE` and exits non-zero, but it is the one
/// case where the sentence above is not the whole story.
///
/// # Why signed, when the budget is about growth
///
/// It was `Option<u32>` through a `saturating_sub`, so a count that *fell*
/// reported `0` - and both this module and `docs/perf-budgets.md` promise the
/// report "names any non-zero drift even when it passes: the run must not be
/// able to report flat for something that moved". That promise was safe only
/// because the two GUI families measure exactly flat on a headless run. The
/// kernel family, added in P9 wave 3, is the first one that moves: a
/// ninety-second run on this box went 256 to 247, and the report printed
/// `kernel handles growth 0`. The first thing the new instrument did on a real
/// box was print "flat" for something that moved by nine.
///
/// So the drift is signed and the *budget* clamps rather than the measurement.
/// A decrease is not a leak and must not fail a run; it is also not nothing,
/// and hiding it is what this project calls a false assurance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HandleGrowth {
    /// GDI objects. Windows only.
    pub(crate) gdi: Option<i64>,
    /// USER objects. Windows only.
    pub(crate) user: Option<i64>,
    /// Open kernel handles (Windows) or file descriptors (Linux).
    pub(crate) kernel: Option<i64>,
}

impl HandleGrowth {
    /// The three families with their report labels, in report order.
    ///
    /// One place decides the labels and the order, so a caller cannot pair
    /// "USER" with the kernel count by writing the array out again.
    pub(crate) const fn labelled(self) -> [(&'static str, Option<i64>); 3] {
        [
            ("GDI", self.gdi),
            ("USER", self.user),
            ("kernel handles", self.kernel),
        ]
    }
}

/// What a finished soak concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every budget held.
    Pass,
    /// At least one budget was exceeded.
    Fail,
    /// Nothing was measured, so nothing is claimed. **Not a pass**: a harness
    /// that reports success having measured nothing is the exact failure this
    /// module exists to remove, and it exits non-zero.
    Unmeasurable,
}

impl SoakRun {
    /// Start a run.
    pub(crate) fn new(duration: Duration, displays: usize) -> Self {
        SoakRun {
            duration,
            displays,
            ipc_started: false,
            samples: 0,
            unreadable: 0,
            peak_rss: None,
            startup_peak_rss: None,
            baseline: None,
            last: None,
        }
    }

    /// Fold one sample in.
    ///
    /// The baseline is the first sample at or after the warm-up **that the OS
    /// could actually answer**. Selecting on time alone was a real defect: one
    /// failed read landing on the baseline slot made every growth check
    /// `None`, every `if let Some` skip, and the verdict `Pass` on a run whose
    /// handle count had climbed by five hundred.
    pub(crate) fn observe(&mut self, sample: Sample) {
        self.samples = self.samples.saturating_add(1);
        let Some(metrics) = sample.metrics else {
            self.unreadable = self.unreadable.saturating_add(1);
            return;
        };
        // Peak is measured over the same window as growth, deliberately. The
        // budgeted peak used to include t=0 - taken right after discovery, the
        // pump, the engine and the IPC server were all assembled - so the highest
        // point of a perfectly healthy run was the startup residue, and the
        // budget most likely to trip was tripped by the very thing the warm-up
        // exists to exclude. That is the D-005 shape this module cites,
        // reintroduced on the other budget. Startup is still measured; it is
        // reported rather than gated.
        let after_warmup = sample.elapsed >= warmup(self.duration);
        let slot = if after_warmup {
            &mut self.peak_rss
        } else {
            &mut self.startup_peak_rss
        };
        *slot = Some(slot.map_or(metrics.rss_bytes, |peak| peak.max(metrics.rss_bytes)));
        if after_warmup && self.baseline.is_none() {
            self.baseline = Some(sample);
        }
        self.last = Some(sample);
    }

    /// RSS growth from the baseline to the end, in bytes. Saturating: a run that
    /// *shrank* reports zero growth rather than underflowing.
    pub(crate) fn rss_growth(&self) -> Option<u64> {
        let base = self.baseline?.metrics?.rss_bytes;
        let last = self.last?.metrics?.rss_bytes;
        Some(last.saturating_sub(base))
    }

    /// GDI and USER handle growth from the baseline.
    ///
    /// `None` means the platform does not count them, and **only** that. It used
    /// to be able to mean "the Windows query failed" as well, because
    /// `gui_objects` answers `None` on failure and `self_metrics` passed that
    /// through - so a single failed `GetGuiResources` made the report say "not
    /// counted on this platform" about Windows, skip the handle budget, and pass.
    /// `duja_platform::process` now refuses to build a half-read sample, so the
    /// two cases are distinguishable again: a failed query is an *unreadable*
    /// sample and is counted as one.
    pub(crate) fn handle_growth(&self) -> HandleGrowth {
        let (Some(base), Some(last)) = (
            self.baseline.and_then(|s| s.metrics),
            self.last.and_then(|s| s.metrics),
        ) else {
            return HandleGrowth::default();
        };
        // Signed, and via `i64` so the subtraction of two `u32`s cannot wrap:
        // every `u32` difference fits, so `checked_sub` cannot fail here and the
        // `unwrap_or(0)` is unreachable rather than a fallback anyone relies on.
        let delta = |b: Option<u32>, l: Option<u32>| {
            b.zip(l)
                .map(|(b, l)| i64::from(l).checked_sub(i64::from(b)).unwrap_or(0))
        };
        HandleGrowth {
            gdi: delta(base.gdi_objects, last.gdi_objects),
            user: delta(base.user_objects, last.user_objects),
            kernel: delta(base.kernel_handles, last.kernel_handles),
        }
    }

    /// Whether the baseline and the final sample are the same one, which means
    /// growth was "measured" across zero elapsed time.
    ///
    /// Reachable with the documented defaults, and it was: `--every` defaults to
    /// 60, so any `duja --soak N` with `N <= 60` took exactly two samples, and
    /// the t=0 one is before the warm-up. The baseline was therefore the final
    /// sample, growth was it minus itself, and a run whose RSS had risen a
    /// quarter of a megabyte printed `0 bytes` and `PASS`.
    fn baseline_is_the_end(&self) -> bool {
        match (self.baseline, self.last) {
            (Some(base), Some(last)) => base.elapsed == last.elapsed,
            _ => false,
        }
    }

    /// The verdict, and every reason it is not [`Verdict::Pass`].
    ///
    /// Returns all the reasons rather than the first: a run that costs hours and
    /// reports one of the two budgets it broke is a run half wasted.
    pub(crate) fn verdict(&self) -> (Verdict, Vec<String>) {
        if self.last.is_none() {
            return (
                Verdict::Unmeasurable,
                vec![format!(
                    "none of the {} samples could be read - this platform cannot report its \
                     own resource usage (see duja_platform::process)",
                    self.samples
                )],
            );
        }
        if self.baseline.is_none() {
            return (
                Verdict::Unmeasurable,
                vec![format!(
                    "no readable sample landed at or after the {:?} warm-up, so there is no \
                     baseline to measure growth from",
                    warmup(self.duration)
                )],
            );
        }
        if self.baseline_is_the_end() {
            return (
                Verdict::Unmeasurable,
                vec![format!(
                    "the baseline and the final sample are the same one, so growth would be \
                     measured across zero elapsed time. Run for longer than the {:?} warm-up, \
                     or sample more often than `--every {}`",
                    warmup(self.duration),
                    self.duration.as_secs()
                )],
            );
        }

        let mut reasons = Vec::new();
        if let Some(peak) = self.peak_rss
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
        for (label, drift) in self.handle_growth().labelled() {
            // Only an *increase* is a leak, so the budget clamps here rather
            // than in the measurement: `HandleGrowth` keeps the sign so the
            // report can name a fall, and this arm ignores one.
            if let Some(drift) = drift
                && drift > i64::from(HANDLE_GROWTH_TOLERANCE)
            {
                reasons.push(format!(
                    "{label} grew by {drift}; this harness fails above \
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

    // The same three pieces `run::headless` assembles. See the module header on
    // why the IPC server is on this list rather than skipped as scaffolding.
    let (tick_rx, mut forwarder) = run::start_platform()?;
    let (engine, notifications) = duja_app::Engine::spawn(
        duja_app::EngineConfig::default(),
        run::enumerator(),
        run::controller_factory(),
        tick_rx,
    );
    let ipc_server = crate::bin_support::ipc::start(std::sync::Arc::new(
        crate::bin_support::ipc::HeadlessBridge::new(engine.sender()),
    ));
    let ipc_started = ipc_server.is_some();
    // Drain notifications rather than printing them: hours of output is not a
    // report, and an undrained channel is itself a leak this harness would then
    // be measuring instead of Duja.
    let drain = std::thread::spawn(move || while notifications.recv().is_ok() {});

    eprintln!(
        "duja soak: {displays} display(s), running {secs}s, sampling every {interval_secs}s. \
         Warm-up is {:?}; growth is measured from the first readable sample after it.",
        warmup(duration)
    );

    let started = Instant::now();
    let mut soak = SoakRun::new(duration, displays);
    soak.ipc_started = ipc_started;
    loop {
        let elapsed = started.elapsed();
        let sample = Sample {
            elapsed,
            metrics: process::self_metrics(),
        };
        eprintln!("{}", render_sample(&sample));
        soak.observe(sample);
        if elapsed >= duration {
            break;
        }
        // The interval is not drift-corrected: each iteration sleeps a full
        // interval *after* its own sampling and printing, so the per-iteration
        // cost accumulates exactly as a fixed sleep would. What the recomputed
        // `elapsed()` does is clamp the LAST sleep so the run does not overshoot
        // `duration`. Correcting the drift would mean sleeping until
        // `started + n * interval`, which is worth doing only if a sample ever
        // becomes expensive; two syscalls and a println are not.
        let remaining = duration.saturating_sub(started.elapsed());
        std::thread::sleep(interval.min(remaining).max(Duration::from_millis(1)));
    }

    if let Some(server) = ipc_server {
        server.shutdown();
    }
    engine.shutdown();
    forwarder.shutdown();
    let _ = drain.join();

    let report = soak.to_string();
    print!("{report}");
    // Also to a file, because on a release Windows build the line above went
    // nowhere: `windows_subsystem = "windows"` means no console, and std maps the
    // invalid handle to `Ok`. See the module header.
    match write_report(&report) {
        Ok(path) => eprintln!("report written to {}", path.display()),
        Err(e) => eprintln!("could not write the report file: {e}"),
    }
    match soak.verdict().0 {
        Verdict::Pass => Ok(ExitCode::SUCCESS),
        Verdict::Fail | Verdict::Unmeasurable => Ok(ExitCode::from(1)),
    }
}

/// Write `report` beside the rotating log, returning where it landed.
///
/// The console is not a reliable sink for this binary (see the module header),
/// and a 24-hour run whose output vanished is 24 hours wasted. Best-effort: a
/// failure here is reported and does not change the verdict.
///
/// # Errors
/// Any failure to create the directory or write the file.
fn write_report(report: &str) -> std::io::Result<std::path::PathBuf> {
    let paths = crate::bin_support::paths::DujaPaths::resolve_or_fallback();
    std::fs::create_dir_all(&paths.log_dir)?;
    let path = paths.log_dir.join("soak-report.txt");
    std::fs::write(&path, report)?;
    Ok(path)
}

/// One sample as a log line.
fn render_sample(sample: &Sample) -> String {
    let secs = sample.elapsed.as_secs();
    match sample.metrics {
        None => format!("  t+{secs:>6}s  (could not read this process's usage)"),
        Some(m) => {
            let gdi = m
                .gdi_objects
                .map_or_else(|| "-".to_owned(), |v| v.to_string());
            let user = m
                .user_objects
                .map_or_else(|| "-".to_owned(), |v| v.to_string());
            let kernel = m
                .kernel_handles
                .map_or_else(|| "-".to_owned(), |v| v.to_string());
            format!(
                "  t+{secs:>6}s  rss {:>12} bytes  gdi {gdi:>5}  user {user:>5}  handles {kernel:>5}",
                m.rss_bytes
            )
        }
    }
}

impl fmt::Display for SoakRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (verdict, reasons) = self.verdict();
        writeln!(f, "\n--- duja soak report ---")?;
        writeln!(f, "displays         {}", self.displays)?;
        writeln!(
            f,
            "ipc server       {}",
            if self.ipc_started {
                "started"
            } else {
                "NOT started - another Duja holds the endpoint, so this run measured less"
            }
        )?;
        writeln!(f, "duration         {:?}", self.duration)?;
        writeln!(
            f,
            "samples          {} ({} unreadable)",
            self.samples, self.unreadable
        )?;
        writeln!(f, "warm-up          {:?}", warmup(self.duration))?;
        match self.peak_rss {
            Some(peak) => writeln!(
                f,
                "peak RSS         {peak} bytes (budget {IDLE_RSS_BUDGET_BYTES}, headless \
                 process - see the module docs)"
            )?,
            None => writeln!(f, "peak RSS         never read")?,
        }
        match self.startup_peak_rss {
            Some(peak) => writeln!(
                f,
                "startup peak     {peak} bytes (before the warm-up; reported, not budgeted)"
            )?,
            None => writeln!(f, "startup peak     not sampled")?,
        }
        match self.rss_growth() {
            Some(growth) => writeln!(
                f,
                "RSS growth       {growth} bytes (budget under {RSS_GROWTH_BUDGET_BYTES})"
            )?,
            None => writeln!(f, "RSS growth       not measured")?,
        }
        for (family, drift) in self.handle_growth().labelled() {
            let label = format!("{family} drift");
            match drift {
                // Any non-zero drift is named even on a pass, in either
                // direction: the budget row says "flat" and this harness's
                // tolerance is looser than that, so a silent pass would be
                // reporting "flat" for something that moved. A *fall* used to
                // print as `0`, because the arithmetic saturated - which made
                // this comment false the moment a family that moves was added.
                Some(0) => writeln!(f, "{label:<21} 0")?,
                Some(d) if d < 0 => writeln!(
                    f,
                    "{label:<21} {d} - NOT FLAT (fell; not a leak, so it does not fail \
                     the run, but the budget says flat)"
                )?,
                Some(d) => writeln!(
                    f,
                    "{label:<21} +{d} - NOT FLAT (the budget says flat; this harness fails \
                     above {HANDLE_GROWTH_TOLERANCE})"
                )?,
                None => writeln!(f, "{label:<21} not counted on this platform")?,
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
        // The kernel count tracks `user` here so a fixture that moves `user`
        // moves this too - which is what `every_broken_budget_is_reported...`
        // needs. It is *not* true that every fixture built through this helper
        // exercises all three families: those that hold `user` constant hold the
        // kernel count constant with it. Tests that are about the kernel family
        // build their own metrics with three distinct values, so the shared
        // value here cannot hide a transposed field.
        ProcessMetrics {
            rss_bytes: rss,
            gdi_objects: Some(gdi),
            user_objects: Some(user),
            kernel_handles: Some(user),
        }
    }

    /// Metrics with only the kernel family readable, which is the Linux shape:
    /// `/proc/self/fd` answers, and GDI/USER do not exist there at all.
    fn kernel_only_metrics(rss: u64, kernel: u32) -> ProcessMetrics {
        ProcessMetrics {
            rss_bytes: rss,
            gdi_objects: None,
            user_objects: None,
            kernel_handles: Some(kernel),
        }
    }

    /// A run of `count` samples spread evenly across `duration`, whose metrics
    /// come from `f`.
    fn run_of(
        duration: Duration,
        count: u32,
        f: impl Fn(u32) -> Option<ProcessMetrics>,
    ) -> SoakRun {
        let mut soak = SoakRun::new(duration, 1);
        for i in 0..count {
            soak.observe(Sample {
                elapsed: duration
                    .checked_div(count.saturating_sub(1).max(1))
                    .unwrap_or_default()
                    .saturating_mul(i),
                metrics: f(i),
            });
        }
        soak
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
    /// is not reported as a leak. Without the baseline this run reads as +8 MB
    /// and fails; from the baseline it is flat.
    #[test]
    fn warm_up_growth_is_not_counted_as_a_leak() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            let rss = if i == 0 { 20_000_000 } else { 28_000_000 };
            Some(metrics(rss, 10, 10))
        });
        assert_eq!(run.rss_growth(), Some(0));
        assert_eq!(run.verdict().0, Verdict::Pass);
    }

    #[test]
    fn a_leak_after_the_warm_up_fails() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
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
        let run = run_of(Duration::from_secs(100), 11, |i| {
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
        let run = run_of(Duration::from_secs(100), 11, |i| {
            Some(metrics(20_000_000, 10 + i, 10))
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail);
        assert!(reasons.iter().any(|r| r.contains("GDI")), "{reasons:?}");
    }

    /// The D-005 lesson: a harness gating on absolute zero reports FAIL on a
    /// healthy run. Drift inside the tolerance passes - but the report must
    /// still say it was not flat, because the budget row says flat and this
    /// tolerance is looser than the budget.
    #[test]
    fn drift_inside_the_tolerance_passes_but_is_never_reported_as_flat() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            Some(metrics(20_000_000, 10 + u32::from(i % 2 == 0), 10))
        });
        assert_eq!(run.verdict().0, Verdict::Pass);
        assert!(run.to_string().contains("NOT FLAT"), "{run}");
    }

    /// Every broken budget must be reported. A run that costs hours and teaches
    /// half of what it found is a run half wasted.
    ///
    /// **Asserted by naming each budget rather than counting them**, because the
    /// count was `4` until a fifth family was added and a tally in a test is one
    /// more thing the next edit falsifies. Naming them is stronger in one
    /// direction - a count of five is satisfied by five copies of one reason -
    /// and weaker in the other, since a count also pinned that nothing extra was
    /// reported. The de-duplication below buys that half back without a tally.
    #[test]
    fn every_broken_budget_is_reported_not_just_the_first() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            Some(metrics(
                40_000_000 + u64::from(i) * 1_000_000,
                10 + i * 2,
                10 + i * 2,
            ))
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail);
        for budget in ["peak RSS", "RSS grew", "GDI", "USER", "kernel handles"] {
            assert!(
                reasons.iter().any(|r| r.contains(budget)),
                "nothing in the report mentions `{budget}`: {reasons:?}"
            );
        }
        // Naming them is stronger than counting them in one direction only: a
        // count also pinned the *absence* of extras, and without this a loop
        // that pushed every reason twice would still pass.
        let mut unique = reasons.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            reasons.len(),
            "a reason was reported twice: {reasons:?}"
        );
    }

    /// The one that matters most: a platform that cannot measure must not report
    /// success. `--soak` exits non-zero here.
    #[test]
    fn a_platform_that_cannot_measure_is_not_a_pass() {
        let run = run_of(Duration::from_secs(100), 11, |_| None);
        assert_eq!(run.verdict().0, Verdict::Unmeasurable);
    }

    /// A run too short to clear its own warm-up has no baseline, so it has
    /// measured nothing either.
    #[test]
    fn a_run_shorter_than_its_warm_up_is_unmeasurable() {
        let mut run = SoakRun::new(Duration::from_mins(10), 1);
        run.observe(Sample {
            elapsed: Duration::ZERO,
            metrics: Some(metrics(20_000_000, 10, 10)),
        });
        assert_eq!(run.verdict().0, Verdict::Unmeasurable);
    }

    /// The defect a review found in the first version of this harness, as a
    /// fixture. With `--every` defaulting to 60, every `duja --soak N` with
    /// N <= 60 took two samples: t=0 (before the warm-up) and t=N. The baseline
    /// was therefore the FINAL sample, growth was it minus itself, and a run
    /// whose RSS had risen a quarter of a megabyte printed `0 bytes` and `PASS`.
    #[test]
    fn a_baseline_that_is_also_the_final_sample_measures_nothing() {
        let mut run = SoakRun::new(Duration::from_secs(3), 1);
        run.observe(Sample {
            elapsed: Duration::ZERO,
            metrics: Some(metrics(17_342_464, 0, 4)),
        });
        run.observe(Sample {
            elapsed: Duration::from_secs(3),
            metrics: Some(metrics(17_629_184, 0, 5)),
        });
        // Growth "is" zero, and that is precisely why the verdict must not be a
        // pass: the two numbers came from one sample.
        assert_eq!(run.rss_growth(), Some(0));
        assert_eq!(run.verdict().0, Verdict::Unmeasurable);
    }

    /// The second defect the same review found: one failed read landing on the
    /// baseline slot made every growth check `None`, every `if let Some` skip,
    /// and the verdict `Pass` on a run whose handle count climbed by 500.
    #[test]
    fn one_unreadable_sample_does_not_hide_a_leak() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            // Sample 1 is the first at/after the 10s warm-up, and it fails.
            if i == 1 {
                None
            } else {
                Some(metrics(20_000_000, 10 + i * 50, 10))
            }
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail, "{reasons:?}");
        assert!(reasons.iter().any(|r| r.contains("GDI")), "{reasons:?}");
        assert_eq!(run.samples, 11);
        assert_eq!(run.unreadable, 1);
    }

    /// A platform that reports RSS but counts nothing else is measurable, and
    /// its handle rows say "not counted" rather than zero.
    #[test]
    fn missing_handle_counts_do_not_make_a_run_unmeasurable() {
        let run = run_of(Duration::from_secs(100), 11, |_| {
            Some(ProcessMetrics {
                rss_bytes: 20_000_000,
                gdi_objects: None,
                user_objects: None,
                kernel_handles: None,
            })
        });
        assert_eq!(run.verdict().0, Verdict::Pass);
        assert_eq!(run.handle_growth(), HandleGrowth::default());
        assert!(run.to_string().contains("not counted on this platform"));
    }

    /// **The case D-112 exists for.** A leak in a kernel handle - a named-pipe
    /// instance per connection, a descriptor never closed - moves no GUI object
    /// at all, so before this counter existed the harness would have reported a
    /// clean PASS while the process climbed towards its handle ceiling. Windows
    /// shape: GDI and USER both flat, kernel climbing.
    #[test]
    fn a_kernel_handle_leak_fails_a_run_whose_gui_objects_are_flat() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            Some(ProcessMetrics {
                rss_bytes: 20_000_000,
                gdi_objects: Some(0),
                user_objects: Some(0),
                kernel_handles: Some(40 + i * 30),
            })
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail, "{reasons:?}");
        assert!(
            reasons.iter().any(|r| r.contains("kernel handles")),
            "the failure must name the family that moved, not a neighbour: {reasons:?}"
        );
        assert!(
            !reasons
                .iter()
                .any(|r| r.contains("GDI") || r.contains("USER")),
            "nothing moved in the GUI families: {reasons:?}"
        );
    }

    /// The Linux shape: no GUI counters at all, and the descriptor count is the
    /// only handle signal there is. Before this, a Linux soak had none.
    #[test]
    fn a_descriptor_leak_is_caught_where_no_gui_counter_exists() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            Some(kernel_only_metrics(20_000_000, 12 + i * 5))
        });
        let (verdict, reasons) = run.verdict();
        assert_eq!(verdict, Verdict::Fail, "{reasons:?}");
        assert!(
            reasons.iter().any(|r| r.contains("kernel handles")),
            "{reasons:?}"
        );
        assert_eq!(run.handle_growth().gdi, None);
        assert_eq!(run.handle_growth().user, None);
    }

    /// The labels and the order are decided in one place so a caller cannot
    /// pair a family's name with a neighbour's count - the failure this project
    /// has already had once with a positional pair of same-typed values.
    #[test]
    fn each_growth_family_keeps_its_own_label() {
        let growth = HandleGrowth {
            gdi: Some(1),
            user: Some(2),
            kernel: Some(3),
        };
        assert_eq!(
            growth.labelled(),
            [
                ("GDI", Some(1)),
                ("USER", Some(2)),
                ("kernel handles", Some(3))
            ]
        );
    }

    /// **A count that falls is reported, and does not fail the run.**
    ///
    /// The drift arithmetic saturated at zero until P9 wave 3, so a family that
    /// fell printed `0` - and this module and `docs/perf-budgets.md` both
    /// promise the report "names any non-zero drift even when it passes". The
    /// promise held only because the two GUI families measure exactly flat on a
    /// headless run. The first real run of the kernel family went 256 to 247 and
    /// the report said `0`, which is the false-assurance shape this project
    /// rates below an admitted gap.
    #[test]
    fn a_handle_count_that_falls_is_named_rather_than_reported_as_flat() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            Some(kernel_only_metrics(20_000_000, 256 - i))
        });
        // A fall is not a leak.
        assert_eq!(run.verdict().0, Verdict::Pass);
        // But it is not flat either, and the report has to say so.
        let drift = run.handle_growth().kernel.expect("the family was counted");
        assert!(
            drift < 0,
            "the count fell, so the drift is negative: {drift}"
        );
        let report = run.to_string();
        assert!(
            report.contains("NOT FLAT"),
            "a fall must be named rather than printed as 0: {report}"
        );
        // The magnitude has to appear too. Asserting only "NOT FLAT" left the
        // rendered number unpinned, so a report that said the right words about
        // the wrong quantity would have passed.
        assert!(
            report.contains("-9"),
            "the report must carry the drift itself: {report}"
        );
    }

    /// The other direction of the same rule: a rise inside the tolerance still
    /// passes and is still named.
    #[test]
    fn a_small_rise_passes_and_is_still_named() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
            Some(kernel_only_metrics(20_000_000, 100 + u32::from(i > 5)))
        });
        assert_eq!(run.verdict().0, Verdict::Pass);
        assert_eq!(run.handle_growth().kernel, Some(1));
        let report = run.to_string();
        assert!(report.contains("NOT FLAT"), "{report}");
        // A rise is signed too, so it cannot be confused with a fall at a
        // glance. Dropping the `+` from the positive arm survived every other
        // assertion in this file.
        assert!(
            report.contains("+1"),
            "a rise must render with its sign: {report}"
        );
    }

    #[test]
    fn peak_rss_over_the_idle_budget_fails_even_with_no_growth() {
        let run = run_of(Duration::from_secs(100), 11, |i| {
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

    /// The peak must survive an unreadable sample after it, and must not be
    /// reset by one.
    #[test]
    fn the_peak_is_kept_across_an_unreadable_sample() {
        let run = run_of(Duration::from_secs(100), 11, |i| match i {
            5 => Some(metrics(30_000_000, 10, 10)),
            6 => None,
            _ => Some(metrics(20_000_000, 10, 10)),
        });
        assert_eq!(run.peak_rss, Some(30_000_000));
    }
}

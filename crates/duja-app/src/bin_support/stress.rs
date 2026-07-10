//! The `--stress` harness: the P3 exit-criteria driver.
//!
//! It assembles the real pipeline like `--headless`, but wraps every
//! factory-produced controller in a [`CountingController`] so it can compare the
//! number of `SetUserLevel` *inputs* it floods in against the number of hardware
//! *writes* the engine actually performs (coalescing should make writes ≪
//! inputs). It fails (non-zero exit) if any display went unresponsive or any
//! backend error was observed.

use std::fmt;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::bounded;

use duja_app::{Engine, EngineCommand, EngineConfig, EngineNotification};
use duja_core::id::StableDisplayId;
use duja_core::model::DisplaySnapshot;

use crate::bin_support::counting::{Counters, CountingController};
use crate::bin_support::rng::XorShift64;
use crate::bin_support::{backend, run};

/// How long to let the engine's initial `Get` probes settle before reading the
/// baseline levels.
const SETTLE: Duration = Duration::from_millis(300);

/// How long to let the final restore + any pending writes drain before joining.
const DRAIN: Duration = Duration::from_millis(300);

/// A finished stress run's tallies, ready to render and to decide the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StressReport {
    /// Displays present at the start of the run.
    pub(crate) displays: usize,
    /// Requested flood duration in seconds.
    pub(crate) duration_secs: u64,
    /// Requested flood rate (ticks/sec/display).
    pub(crate) hz: u32,
    /// `SetUserLevel` commands sent to the engine.
    pub(crate) inputs_sent: u64,
    /// Hardware `set` calls the engine actually performed.
    pub(crate) hardware_writes: u64,
    /// Hardware `get` calls (initial level probes, mostly).
    pub(crate) hardware_reads: u64,
    /// Backend errors observed across all controllers.
    pub(crate) errors: u64,
    /// `DisplayUnresponsive` notifications observed.
    pub(crate) unresponsive: u64,
}

impl StressReport {
    /// Hardware writes per 100 inputs (integer; 0 when nothing was sent). A low
    /// number is the goal: it shows coalescing shielded the hardware.
    pub(crate) fn writes_per_100_inputs(&self) -> u64 {
        self.hardware_writes
            .saturating_mul(100)
            .checked_div(self.inputs_sent)
            .unwrap_or(0)
    }

    /// Whether the run met the exit criteria (no errors, no unresponsive).
    pub(crate) fn passed(&self) -> bool {
        self.errors == 0 && self.unresponsive == 0
    }

    /// The process exit code for this report.
    pub(crate) fn exit_code(&self) -> ExitCode {
        if self.passed() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}

impl fmt::Display for StressReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "stress report")?;
        writeln!(f, "  displays:          {}", self.displays)?;
        writeln!(
            f,
            "  duration/rate:     {}s @ {} Hz/display",
            self.duration_secs, self.hz
        )?;
        writeln!(f, "  inputs sent:       {}", self.inputs_sent)?;
        writeln!(f, "  hardware writes:   {}", self.hardware_writes)?;
        writeln!(
            f,
            "  writes/100 inputs: {} (lower = better coalescing)",
            self.writes_per_100_inputs()
        )?;
        writeln!(f, "  hardware reads:    {}", self.hardware_reads)?;
        writeln!(f, "  errors:            {}", self.errors)?;
        writeln!(f, "  unresponsive:      {}", self.unresponsive)?;
        write!(
            f,
            "  result:            {}",
            if self.passed() { "PASS" } else { "FAIL" }
        )
    }
}

/// Run the stress harness. With zero displays it prints `no displays` and exits
/// 0 (so a disconnected session does not fail the harness).
///
/// # Errors
/// Propagates a failure to start the platform event pump.
pub(crate) fn run(secs: u64, hz: u32) -> anyhow::Result<ExitCode> {
    let initial_displays = backend::discover();
    if initial_displays.is_empty() {
        println!("no displays");
        return Ok(ExitCode::SUCCESS);
    }

    // Shared tallies: one counter set per opened controller, plus a live count
    // of unresponsive notifications.
    let registry: Arc<Mutex<Vec<Arc<Counters>>>> = Arc::new(Mutex::new(Vec::new()));
    let unresponsive = Arc::new(AtomicU64::new(0));

    let (tick_rx, mut forwarder) = run::start_platform()?;
    let factory = counting_factory(registry.clone());

    let (engine, notifications) =
        Engine::spawn(EngineConfig::default(), run::enumerator(), factory, tick_rx);

    // Count unresponsive notifications on a side thread for the duration.
    let notif_unresponsive = unresponsive.clone();
    let notif_join = std::thread::spawn(move || {
        while let Ok(notification) = notifications.recv() {
            if matches!(notification, EngineNotification::DisplayUnresponsive(_)) {
                notif_unresponsive.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    let sender = engine.sender();

    // Let initial Get probes settle, then read the baseline levels to restore.
    std::thread::sleep(SETTLE);
    let baseline = snapshot(&sender);

    let ids: Vec<StableDisplayId> = baseline.iter().map(|s| s.id.clone()).collect();
    let inputs_sent = flood(&sender, &ids, secs, hz);

    // Restore the baseline levels, then let the last writes drain.
    for snap in &baseline {
        let _ = sender.send(EngineCommand::SetUserLevel {
            id: snap.id.clone(),
            pct: snap.user_level_pct,
        });
    }
    std::thread::sleep(DRAIN);

    engine.shutdown();
    forwarder.shutdown();
    let _ = notif_join.join();

    let report = build_report(
        &registry,
        &unresponsive,
        initial_displays.len(),
        secs,
        hz,
        inputs_sent,
    );
    println!("{report}");
    Ok(report.exit_code())
}

/// Build the counting controller factory over a shared registry of counters.
///
/// Each call returns a deferred opener that opens (and wraps) the controller on
/// the worker thread; a counter set is registered only once the open succeeds.
fn counting_factory(registry: Arc<Mutex<Vec<Arc<Counters>>>>) -> duja_app::ControllerFactory {
    Box::new(move |id: &StableDisplayId| {
        let id = id.clone();
        let registry = registry.clone();
        Box::new(move || {
            let inner = backend::open_controller(&id)?;
            let counters = Counters::new_shared();
            if let Ok(mut guard) = registry.lock() {
                guard.push(counters.clone());
            }
            let wrapped: Box<dyn duja_core::controller::BrightnessController> =
                Box::new(CountingController::new(inner, counters));
            Some(wrapped)
        }) as duja_app::ControllerOpener
    })
}

/// Ask the engine for its current snapshots (empty on timeout).
fn snapshot(sender: &crossbeam_channel::Sender<EngineCommand>) -> Vec<DisplaySnapshot> {
    let (reply_tx, reply_rx) = bounded(1);
    if sender
        .send(EngineCommand::Snapshot { reply: reply_tx })
        .is_err()
    {
        return Vec::new();
    }
    reply_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default()
}

/// Flood `SetUserLevel` with random values across `ids` at `hz` ticks/sec for
/// `secs` seconds. Returns the number of commands sent.
fn flood(
    sender: &crossbeam_channel::Sender<EngineCommand>,
    ids: &[StableDisplayId],
    secs: u64,
    hz: u32,
) -> u64 {
    let interval = tick_interval(hz);
    let duration = Duration::from_secs(secs);
    let mut rng = XorShift64::new(seed());
    let start = Instant::now();
    let mut inputs_sent: u64 = 0;

    while start.elapsed() < duration {
        for id in ids {
            let pct = rng.next_pct();
            if sender
                .send(EngineCommand::SetUserLevel {
                    id: id.clone(),
                    pct,
                })
                .is_ok()
            {
                inputs_sent = inputs_sent.saturating_add(1);
            }
        }
        std::thread::sleep(interval);
    }
    inputs_sent
}

/// The sleep between flood ticks for a given rate (at least 1 ms).
fn tick_interval(hz: u32) -> Duration {
    let ms = 1000u64.checked_div(u64::from(hz)).unwrap_or(1).max(1);
    Duration::from_millis(ms)
}

/// A time-derived, non-cryptographic seed for the flood PRNG.
fn seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0x1234_5678, |d| d.as_secs() ^ u64::from(d.subsec_nanos()))
}

/// Aggregate the shared counters into a [`StressReport`].
fn build_report(
    registry: &Arc<Mutex<Vec<Arc<Counters>>>>,
    unresponsive: &Arc<AtomicU64>,
    displays: usize,
    duration_secs: u64,
    hz: u32,
    inputs_sent: u64,
) -> StressReport {
    let (mut writes, mut reads, mut errors) = (0u64, 0u64, 0u64);
    if let Ok(guard) = registry.lock() {
        for counters in guard.iter() {
            writes = writes.saturating_add(counters.sets());
            reads = reads.saturating_add(counters.gets());
            errors = errors.saturating_add(counters.errors());
        }
    }
    StressReport {
        displays,
        duration_secs,
        hz,
        inputs_sent,
        hardware_writes: writes,
        hardware_reads: reads,
        errors,
        unresponsive: unresponsive.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::{StressReport, tick_interval};
    use std::time::Duration;

    fn report(inputs: u64, writes: u64, errors: u64, unresponsive: u64) -> StressReport {
        StressReport {
            displays: 2,
            duration_secs: 5,
            hz: 20,
            inputs_sent: inputs,
            hardware_writes: writes,
            hardware_reads: 4,
            errors,
            unresponsive,
        }
    }

    #[test]
    fn writes_per_100_inputs_is_integer_ratio() {
        assert_eq!(report(1000, 50, 0, 0).writes_per_100_inputs(), 5);
        // Guards divide-by-zero.
        assert_eq!(report(0, 0, 0, 0).writes_per_100_inputs(), 0);
    }

    #[test]
    fn passed_requires_no_errors_and_no_unresponsive() {
        assert!(report(100, 10, 0, 0).passed());
        assert!(!report(100, 10, 1, 0).passed());
        assert!(!report(100, 10, 0, 1).passed());
    }

    #[test]
    fn render_mentions_key_metrics() {
        let text = report(1000, 50, 0, 0).to_string();
        assert!(text.contains("inputs sent:       1000"));
        assert!(text.contains("hardware writes:   50"));
        assert!(text.contains("PASS"));
        assert!(report(1, 1, 1, 0).to_string().contains("FAIL"));
    }

    #[test]
    fn tick_interval_is_at_least_one_ms() {
        assert_eq!(tick_interval(20), Duration::from_millis(50));
        assert_eq!(tick_interval(1), Duration::from_secs(1));
        // A very high rate floors at 1 ms rather than a busy 0 ms.
        assert_eq!(tick_interval(100_000), Duration::from_millis(1));
    }
}

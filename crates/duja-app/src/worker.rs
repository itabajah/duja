//! Per-monitor worker threads.
//!
//! Each worker exclusively owns its
//! [`BrightnessController`](duja_core::controller::BrightnessController)
//! (**opened on this thread** via the injected [`ControllerOpener`] as the first
//! thing the worker does — the trait's `&mut self` makes serialization a
//! compile-time property, so no locking is needed). The loop:
//!
//! 1. parks on its command channel when idle (**zero wakeups**);
//! 2. drains every immediately-available command, keeping the newest value per
//!    feature (latest-wins coalescing; distinct features never merge);
//! 3. performs each feature whose engine-level min-gap has elapsed, waking via
//!    `recv_timeout` only while a gap is outstanding;
//! 4. acks every performed op back to the engine.
//!
//! Every controller call runs under [`catch_unwind`]; a panic becomes an
//! [`AckOutcome::Panicked`] and the worker exits (the engine then marks the
//! display unresponsive). A stuck (never-returning) controller call simply
//! never acks — the engine's watchdog handles that by leaking this thread.
//!
//! A worker also observes a shared `retired` flag the engine flips on detach: it
//! checks the flag after every controller call and before performing any buffered
//! write, and exits immediately when set. This guarantees a detached worker whose
//! wedged call finally returns never drains its backlog into another hardware
//! write — so it can never become a second writer racing its replacement
//! (ADR-0017).

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use duja_core::controller::{BrightnessController, ControlError};
use duja_core::id::StableDisplayId;
use duja_core::model::Feature;

use crate::ControllerOpener;
use crate::protocol::{AckOutcome, InflightKey, WorkerAck, WorkerCommand};

/// The engine's handle to one worker: its command sender, join handle, and the
/// lifecycle signals that make detach safe (ADR-0017).
///
/// Dropping the handle drops the sender, so an idle worker observes the
/// disconnect and exits on its own; leaked (stuck) workers are simply dropped
/// without joining.
#[derive(Debug)]
pub(crate) struct WorkerHandle {
    /// Channel to send [`WorkerCommand`]s to this worker.
    pub(crate) cmd_tx: Sender<WorkerCommand>,
    /// The worker thread's join handle (never joined for leaked workers).
    pub(crate) join: JoinHandle<()>,
    /// Disconnects when the worker thread exits by **any** path. The engine waits
    /// on this with a shared deadline for a **bounded** shutdown, detaching a
    /// worker still wedged in a driver call instead of joining it forever.
    pub(crate) done: Receiver<()>,
    /// This worker's monotonic identity, carried on its
    /// [`AckOutcome::OpenFailed`] so the engine can ignore a stale failure from an
    /// already-replaced worker (generation match).
    pub(crate) generation: u64,
    /// Flipped by the engine when it detaches this worker. The worker observes it
    /// after every controller call (and before performing buffered writes) and
    /// exits immediately, so a detached worker never performs another hardware
    /// write — never a second writer to the same panel.
    pub(crate) retired: Arc<AtomicBool>,
}

/// Spawn a worker for `id` that opens its controller via `opener` **on its own
/// thread**, then runs the control loop.
///
/// Running the open on the worker thread (rather than the engine thread) keeps
/// every controller and its thread-affine resources — a COM apartment, a
/// physical-monitor handle — constructed, used, and dropped on one thread. If
/// the open returns `None`, the worker reports [`AckOutcome::OpenFailed`] and
/// exits without ever entering the loop.
pub(crate) fn spawn_worker(
    id: StableDisplayId,
    opener: ControllerOpener,
    min_gap: Duration,
    generation: u64,
    ack_tx: Sender<WorkerAck>,
) -> WorkerHandle {
    let (cmd_tx, cmd_rx) = unbounded::<WorkerCommand>();
    let (done_tx, done_rx) = unbounded::<()>();
    let retired = Arc::new(AtomicBool::new(false));
    let worker_retired = Arc::clone(&retired);
    let join = thread::spawn(move || {
        // Dropped when this thread exits by ANY path (open failure, clean stop, or
        // a leaked-then-returning wedged call), disconnecting `done_rx` so the
        // engine's bounded shutdown can observe the exit without a `join()`.
        let _done = done_tx;
        let Some(controller) = opener() else {
            let _ = ack_tx.send(WorkerAck {
                id,
                outcome: AckOutcome::OpenFailed { generation },
            });
            return;
        };
        worker_loop(
            &id,
            controller,
            min_gap,
            generation,
            &cmd_rx,
            &ack_tx,
            &worker_retired,
        );
    });
    WorkerHandle {
        cmd_tx,
        join,
        done: done_rx,
        generation,
        retired,
    }
}

/// What a receive attempt produced.
enum Wake {
    /// A command arrived.
    Cmd(WorkerCommand),
    /// The pending min-gap elapsed with no new command.
    Timeout,
    /// The channel disconnected (engine gone / worker retired): exit.
    Stop,
}

/// A queued write awaiting its min-gap: the latest value and its sequence.
type Pending = (u16, u64);

/// Read-back tolerance (raw units) for the first-write no-op check. On Duja's
/// default DDC path a write is *not* hardware-verified, so this worker-side
/// read-back (now retried, below) is the first and only confirmation that the
/// write took; the ±2 units absorb a monitor rounding to its own step size.
const BRIGHTNESS_VERIFY_TOLERANCE: u16 = 2;

/// Read-back attempts for the first-write no-op check. A slow-but-working panel
/// (behind a USB-C dock / MST hub / KVM) can lag a write by more than one pacing
/// gap, so a single un-retried read would misread "slow" as "dead" and downgrade
/// it permanently. Retrying lets a live panel reveal movement before we commit to
/// the one-way downgrade; a truly dead panel never moves. Mirrors the DDC
/// controller's verify-retry count.
const BRIGHTNESS_VERIFY_ATTEMPTS: u32 = 3;

/// Base back-off between read-back attempts, doubled each attempt up to
/// [`BRIGHTNESS_VERIFY_BACKOFF_MAX`] — mirrors the DDC controller's retry back-off.
/// On real hardware the DDC controller additionally paces every wire op, so a slow
/// panel gets even more wall-clock to reflect than this alone provides.
const BRIGHTNESS_VERIFY_BACKOFF_BASE: Duration = Duration::from_millis(20);

/// Ceiling on a single read-back back-off.
const BRIGHTNESS_VERIFY_BACKOFF_MAX: Duration = Duration::from_millis(320);

/// One-shot state for detecting a display with **no working hardware brightness**
/// (BUG 3). A worker downgrades such a display to software-only exactly once, via
/// the tightest available signal, then stops watching.
#[derive(Debug, Default)]
struct Verify {
    /// Set once the worker has made its one-shot hardware determination — a probe
    /// with no hardware range, a verified first write, or a rejected first write.
    /// While set, no further downgrade is emitted (idempotent per worker).
    settled: bool,
    /// The most recent brightness value read back, used as the pre-write baseline
    /// for the first-write verification.
    last_known_brightness: Option<u16>,
}

/// Detection (a): probe the controller on open. Emits a single generation-tagged
/// [`AckOutcome::SoftwareFallback`] and returns `true` (settled) when the probe
/// reports **no hardware brightness range**. A probe that reports a hardware
/// range, fails transiently, or panics leaves the worker watching writes as before
/// (returns `false`). The worker keeps running either way — a downgraded display
/// still exists and its overlay dims it.
fn probe_on_open(
    controller: &mut Box<dyn BrightnessController>,
    id: &StableDisplayId,
    generation: u64,
    ack_tx: &Sender<WorkerAck>,
) -> bool {
    // RATIONALE(AssertUnwindSafe): as elsewhere in this module, a caught panic
    // drops the controller with the worker; the first real op's panic path handles
    // a genuinely broken controller.
    match catch_unwind(AssertUnwindSafe(|| controller.probe())) {
        Ok(Ok(caps)) if !caps.hardware_range => {
            let _ = ack_tx.send(WorkerAck {
                id: id.clone(),
                outcome: AckOutcome::SoftwareFallback { generation },
            });
            true
        }
        _ => false,
    }
}

/// Whether a rejected write's error is a positive no-hardware signal, as opposed
/// to a transient / gone condition that must **not** trigger a downgrade. A lone
/// [`ControlError::Timeout`] or [`ControlError::Disconnected`] is transient (the
/// panel may be momentarily busy or unplugging); [`ControlError::Unsupported`] and
/// an opaque [`ControlError::Backend`] (e.g. a DDC write that never verified) mean
/// the panel genuinely cannot do brightness.
fn is_no_hardware_error(err: &ControlError) -> bool {
    matches!(err, ControlError::Unsupported | ControlError::Backend(_))
}

/// Whether a first effective write was a **silent no-op**: the read-back neither
/// reached the target nor moved from the pre-write value. Requiring BOTH (not
/// merely "did not reach target") keeps the downgrade tight — a panel that moved
/// but landed off-target, or whose read-back is merely noisy, stays hardware-backed.
fn is_silent_noop(before: u16, target: u16, after: u16, tol: u16) -> bool {
    after.abs_diff(target) > tol && after.abs_diff(before) <= tol
}

/// The exponential read-back back-off for a zero-based `attempt`, capped.
fn verify_backoff(attempt: u32) -> Duration {
    let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
    BRIGHTNESS_VERIFY_BACKOFF_BASE
        .checked_mul(factor)
        .unwrap_or(BRIGHTNESS_VERIFY_BACKOFF_MAX)
        .min(BRIGHTNESS_VERIFY_BACKOFF_MAX)
}

/// One-shot no-hardware check on the first *effective* brightness write, run
/// after the write's ack. Returns `true` when the write proves the display has no
/// working hardware brightness (the caller then emits a single
/// [`AckOutcome::SoftwareFallback`]). Only a write that commanded a meaningful
/// change from the last known value is diagnostic; the read-back is retried (see
/// [`verify_first_write`]).
fn check_first_write(
    controller: &mut Box<dyn BrightnessController>,
    verify: &mut Verify,
    feature: Feature,
    raw: u16,
    result: &Result<(), ControlError>,
    retired: &Arc<AtomicBool>,
) -> bool {
    if verify.settled || feature != Feature::Brightness {
        return false;
    }
    match result {
        Ok(()) => {
            // A trivial (no-delta) write proves nothing, so keep watching for the
            // first real one (do not consume the one-shot).
            let Some(before) = verify.last_known_brightness else {
                return false;
            };
            if before.abs_diff(raw) <= BRIGHTNESS_VERIFY_TOLERANCE {
                return false;
            }
            verify_first_write(controller, verify, feature, before, raw, retired)
        }
        // A rejected first brightness write with a positive no-hardware error is
        // itself the signal (debt #48: no longer swallowed as a clean Set).
        Err(err) if is_no_hardware_error(err) => {
            verify.settled = true;
            true
        }
        // A transient error on the first write must not downgrade.
        Err(_) => false,
    }
}

/// Read back the first effective brightness write with retries + back-off, to
/// tell a **slow-but-working** panel (which moves within the retries) from a
/// **dead** one (which never moves). Returns `true` only when *every* successful
/// read-back is a silent no-op — a definitive downgrade signal.
///
/// The one-shot latch (`verify.settled`) is consumed **only on a definitive
/// verdict**: movement seen (live) or a confirmed no-op (dead). An all-transient
/// (or detached) verification leaves the latch open so a later write re-attempts,
/// so a lone read glitch can never permanently disable detection.
fn verify_first_write(
    controller: &mut Box<dyn BrightnessController>,
    verify: &mut Verify,
    feature: Feature,
    before: u16,
    target: u16,
    retired: &Arc<AtomicBool>,
) -> bool {
    let mut saw_definitive_noop = false;
    for attempt in 0..BRIGHTNESS_VERIFY_ATTEMPTS {
        if retired.load(Ordering::Acquire) {
            // Detached mid-verify: the verdict is moot (a stale fallback would be
            // dropped by the engine's generation match anyway). Do not latch.
            return false;
        }
        // RATIONALE(AssertUnwindSafe): mirrors the loop's other controller calls —
        // a caught panic drops the controller with the worker, so no torn state is
        // observed; a panicking read is treated as inconclusive for this attempt.
        if let Ok(Ok(range)) = catch_unwind(AssertUnwindSafe(|| controller.get(feature))) {
            verify.last_known_brightness = Some(range.current);
            if is_silent_noop(before, target, range.current, BRIGHTNESS_VERIFY_TOLERANCE) {
                // A dead panel never moves; a slow one may still catch up, so keep
                // retrying before committing.
                saw_definitive_noop = true;
            } else {
                // Moved toward / to the target: the panel is live.
                verify.settled = true;
                return false;
            }
        }
        if attempt.saturating_add(1) < BRIGHTNESS_VERIFY_ATTEMPTS {
            thread::sleep(verify_backoff(attempt));
        }
    }
    if saw_definitive_noop {
        // Every successful read-back showed the panel unmoved: definitively dead.
        verify.settled = true;
        true
    } else {
        // No successful read-back (all transient): inconclusive — leave the latch
        // open for a later write to re-attempt.
        false
    }
}

/// Perform the queued reads (step 3a): each `get` acks its result and updates the
/// brightness baseline. Returns `true` if the worker must stop — a read panicked,
/// or the engine retired the worker mid-read (a detached worker must not linger).
fn perform_reads(
    gets: &mut Vec<(Feature, u64)>,
    controller: &mut Box<dyn BrightnessController>,
    verify: &mut Verify,
    id: &StableDisplayId,
    ack_tx: &Sender<WorkerAck>,
    retired: &Arc<AtomicBool>,
) -> bool {
    for (feature, seq) in gets.drain(..) {
        let outcome = match catch_unwind(AssertUnwindSafe(|| controller.get(feature))) {
            Ok(result) => {
                // Track the last brightness reading as the pre-write baseline for
                // the first-write no-op check.
                if feature == Feature::Brightness
                    && !verify.settled
                    && let Ok(range) = &result
                {
                    verify.last_known_brightness = Some(range.current);
                }
                AckOutcome::Get {
                    feature,
                    seq,
                    result,
                }
            }
            // RATIONALE(AssertUnwindSafe): the controller is dropped with this
            // thread right after a caught panic (we `return` below), so no later
            // reader can observe a torn state.
            Err(_) => AckOutcome::Panicked {
                key: InflightKey::Get(feature),
                seq,
            },
        };
        let is_panic = matches!(outcome, AckOutcome::Panicked { .. });
        let _ = ack_tx.send(WorkerAck {
            id: id.clone(),
            outcome,
        });
        if is_panic {
            return true;
        }
        // If the engine detached us during this read, exit before doing any more
        // work (a detached worker must not linger).
        if retired.load(Ordering::Acquire) {
            return true;
        }
    }
    false
}

fn worker_loop(
    id: &StableDisplayId,
    mut controller: Box<dyn BrightnessController>,
    min_gap: Duration,
    generation: u64,
    cmd_rx: &Receiver<WorkerCommand>,
    ack_tx: &Sender<WorkerAck>,
    retired: &Arc<AtomicBool>,
) {
    // Newest queued write per feature, and the earliest instant each feature
    // may next be written (its min-gap deadline).
    let mut latest: BTreeMap<Feature, Pending> = BTreeMap::new();
    let mut next_ok: BTreeMap<Feature, Instant> = BTreeMap::new();
    // One-shot no-hardware detection (BUG 3). Detection (a) runs on open here.
    let mut verify = Verify {
        settled: probe_on_open(&mut controller, id, generation, ack_tx),
        last_known_brightness: None,
    };

    loop {
        // Reads are performed immediately (rare, on add); collect any that
        // arrive this round here.
        let mut gets: Vec<(Feature, u64)> = Vec::new();

        // 1. Wait: block forever when nothing is queued (zero idle wakeups),
        //    otherwise wake when the earliest min-gap elapses.
        let wake = match earliest_wait(&latest, &next_ok) {
            None => match cmd_rx.recv() {
                Ok(cmd) => Wake::Cmd(cmd),
                Err(_) => Wake::Stop,
            },
            Some(timeout) => match cmd_rx.recv_timeout(timeout) {
                Ok(cmd) => Wake::Cmd(cmd),
                Err(RecvTimeoutError::Timeout) => Wake::Timeout,
                Err(RecvTimeoutError::Disconnected) => Wake::Stop,
            },
        };

        match wake {
            Wake::Stop => return,
            Wake::Timeout => {}
            Wake::Cmd(cmd) => {
                if absorb(&cmd, &mut latest, &mut next_ok, &mut gets) {
                    return;
                }
                // 2. Drain everything else immediately available.
                while let Ok(cmd) = cmd_rx.try_recv() {
                    if absorb(&cmd, &mut latest, &mut next_ok, &mut gets) {
                        return;
                    }
                }
            }
        }

        // 3a. Perform reads (not rate-limited).
        if perform_reads(&mut gets, &mut controller, &mut verify, id, ack_tx, retired) {
            return;
        }

        // 3b. Perform writes whose min-gap has elapsed.
        //
        // If the engine detached us while we were parked / draining, exit before
        // performing ANY buffered write: a detached worker whose replacement may
        // already exist must never issue a second hardware write to the panel.
        if retired.load(Ordering::Acquire) {
            return;
        }
        let now = Instant::now();
        let ready: Vec<Feature> = latest
            .iter()
            .filter(|(feature, _)| now >= next_ok.get(feature).copied().unwrap_or(now))
            .map(|(feature, _)| *feature)
            .collect();
        for feature in ready {
            let Some((raw, seq)) = latest.remove(&feature) else {
                continue;
            };
            // RATIONALE(AssertUnwindSafe): see 3a — the controller does not
            // outlive a caught panic.
            let Ok(inner) = catch_unwind(AssertUnwindSafe(|| controller.set(feature, raw))) else {
                let _ = ack_tx.send(WorkerAck {
                    id: id.clone(),
                    outcome: AckOutcome::Panicked {
                        key: InflightKey::Set(feature),
                        seq,
                    },
                });
                return;
            };
            // Ack the Set regardless of the backend result: the worker completed the
            // op and is not wedged, so the watchdog slot must clear. Debt #48 is
            // addressed by ACTING on a rejected inner result below (a downgrade
            // signal), never by dropping this ack — which would strand the watchdog
            // armed and false-fire the display unresponsive.
            let _ = ack_tx.send(WorkerAck {
                id: id.clone(),
                outcome: AckOutcome::Set { feature, seq },
            });
            // The write we just performed may have been a wedged call the engine
            // gave up on and detached us for (watchdog / shutdown). If so, exit NOW
            // — do not loop back to drain and coalesce the queued backlog into
            // another write, which would race the freshly-spawned replacement
            // worker as a second writer on the same monitor (E-A / ADR-0017).
            if retired.load(Ordering::Acquire) {
                return;
            }
            // Detection (b)/(c): one-shot no-hardware check on the first effective
            // brightness write (a silent no-op read-back, or a rejected write).
            if check_first_write(&mut controller, &mut verify, feature, raw, &inner, retired) {
                let _ = ack_tx.send(WorkerAck {
                    id: id.clone(),
                    outcome: AckOutcome::SoftwareFallback { generation },
                });
            }
            next_ok.insert(feature, now.checked_add(min_gap).unwrap_or(now));
        }
    }
}

/// Fold one command into the pending state. Returns `true` if the worker
/// should stop (a `Shutdown` was received).
fn absorb(
    cmd: &WorkerCommand,
    latest: &mut BTreeMap<Feature, Pending>,
    next_ok: &mut BTreeMap<Feature, Instant>,
    gets: &mut Vec<(Feature, u64)>,
) -> bool {
    match cmd {
        WorkerCommand::Shutdown => return true,
        WorkerCommand::Set { feature, raw, seq } => {
            latest.insert(*feature, (*raw, *seq));
            // A never-seen feature becomes writable immediately; an
            // already-emitted feature keeps its outstanding gap deadline.
            next_ok.entry(*feature).or_insert_with(Instant::now);
        }
        WorkerCommand::Get { feature, seq } => gets.push((*feature, *seq)),
    }
    false
}

/// The shortest wait until some queued feature is writable, or `None` when
/// nothing is queued (park indefinitely).
fn earliest_wait(
    latest: &BTreeMap<Feature, Pending>,
    next_ok: &BTreeMap<Feature, Instant>,
) -> Option<Duration> {
    let now = Instant::now();
    latest
        .keys()
        .map(|feature| {
            next_ok
                .get(feature)
                .map_or(Duration::ZERO, |ready| ready.saturating_duration_since(now))
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Sender, unbounded};
    use duja_core::controller::{BrightnessController, ControlError};
    use duja_core::model::{Capabilities, Feature, FeatureRange};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A controller that reports every successful write on a channel, so tests
    /// can observe writes after the controller has moved onto a worker thread.
    #[derive(Debug)]
    struct Recording {
        writes: Sender<(Feature, u16)>,
        caps: Capabilities,
        values: BTreeMap<Feature, FeatureRange>,
    }

    impl Recording {
        fn new(writes: Sender<(Feature, u16)>) -> Self {
            let caps = Capabilities {
                features: [Feature::Brightness, Feature::Contrast]
                    .into_iter()
                    .collect(),
                hardware_range: true,
                raw_capabilities: None,
                allowed_inputs: Vec::new(),
            };
            let values = caps
                .features
                .iter()
                .map(|&f| {
                    (
                        f,
                        FeatureRange {
                            current: 50,
                            max: 100,
                        },
                    )
                })
                .collect();
            Recording {
                writes,
                caps,
                values,
            }
        }
    }

    impl BrightnessController for Recording {
        fn probe(&mut self) -> Result<Capabilities, ControlError> {
            Ok(self.caps.clone())
        }
        fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError> {
            self.values
                .get(&feature)
                .copied()
                .ok_or(ControlError::Unsupported)
        }
        fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
            self.values.insert(
                feature,
                FeatureRange {
                    current: value,
                    max: 100,
                },
            );
            let _ = self.writes.send((feature, value));
            Ok(())
        }
    }

    fn worker_id() -> StableDisplayId {
        let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        e.push(0x04);
        e.push(0x21);
        e.resize(127, 0x00);
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg());
        StableDisplayId::from_edid(&e).unwrap()
    }

    /// Wait for a `(feature, value)` matching `pred`, counting how many writes
    /// were seen before it. Times out to keep the test from hanging.
    fn drain_until(
        writes: &Receiver<(Feature, u16)>,
        pred: impl Fn(Feature, u16) -> bool,
    ) -> (usize, Vec<(Feature, u16)>) {
        let mut seen = Vec::new();
        loop {
            match writes.recv_timeout(Duration::from_secs(2)) {
                Ok((f, v)) => {
                    seen.push((f, v));
                    if pred(f, v) {
                        return (seen.len(), seen);
                    }
                }
                Err(_) => return (seen.len(), seen),
            }
        }
    }

    #[test]
    fn distinct_features_not_cross_coalesced() {
        // Interleave Brightness and Contrast writes; both final values must
        // land — features never collapse into one another.
        let (writes_tx, writes_rx) = unbounded();
        let (ack_tx, _ack_rx) = unbounded();
        let opener: crate::ControllerOpener = Box::new(move || {
            Some(Box::new(Recording::new(writes_tx)) as Box<dyn BrightnessController>)
        });
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(5), 1, ack_tx);

        for (i, feature) in [
            Feature::Brightness,
            Feature::Contrast,
            Feature::Brightness,
            Feature::Contrast,
        ]
        .into_iter()
        .enumerate()
        {
            let seq = u64::try_from(i).unwrap();
            let raw = if feature == Feature::Brightness {
                11
            } else {
                88
            };
            handle
                .cmd_tx
                .send(WorkerCommand::Set { feature, raw, seq })
                .unwrap();
        }

        // Collect writes until we have seen the final value of both features.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut got_bright = false;
        let mut got_contrast = false;
        while !(got_bright && got_contrast) {
            let (f, v) = writes_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            seen.lock().unwrap().push((f, v));
            if f == Feature::Brightness && v == 11 {
                got_bright = true;
            }
            if f == Feature::Contrast && v == 88 {
                got_contrast = true;
            }
        }
        assert!(got_bright, "brightness final value never landed");
        assert!(got_contrast, "contrast final value never landed");

        handle.cmd_tx.send(WorkerCommand::Shutdown).unwrap();
        handle.join.join().unwrap();
    }

    #[test]
    fn burst_yields_single_hw_write_at_worker() {
        // Flood one feature with 100 writes faster than the min-gap; the worker
        // must coalesce to far fewer, and the LAST value must win.
        let (writes_tx, writes_rx) = unbounded();
        let (ack_tx, _ack_rx) = unbounded();
        let opener: crate::ControllerOpener = Box::new(move || {
            Some(Box::new(Recording::new(writes_tx)) as Box<dyn BrightnessController>)
        });
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(80), 1, ack_tx);

        for _ in 0..100u32 {
            handle
                .cmd_tx
                .send(WorkerCommand::Set {
                    feature: Feature::Brightness,
                    raw: 10,
                    seq: 0,
                })
                .unwrap();
        }
        handle
            .cmd_tx
            .send(WorkerCommand::Set {
                feature: Feature::Brightness,
                raw: 77,
                seq: 1,
            })
            .unwrap();

        let (count, seen) = drain_until(&writes_rx, |f, v| f == Feature::Brightness && v == 77);
        assert!(
            count < 100,
            "expected far fewer than 100 writes, got {count}"
        );
        assert_eq!(
            seen.last().copied(),
            Some((Feature::Brightness, 77)),
            "last value must win"
        );

        handle.cmd_tx.send(WorkerCommand::Shutdown).unwrap();
        handle.join.join().unwrap();
    }

    /// A controller that probes with no hardware brightness range (its `set`/`get`
    /// still succeed, so only the probe distinguishes it).
    #[derive(Debug)]
    struct NoHardware;

    impl BrightnessController for NoHardware {
        fn probe(&mut self) -> Result<Capabilities, ControlError> {
            Ok(Capabilities {
                features: std::collections::BTreeSet::new(),
                hardware_range: false,
                raw_capabilities: None,
                allowed_inputs: Vec::new(),
            })
        }
        fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
            Ok(FeatureRange {
                current: 50,
                max: 100,
            })
        }
        fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
            Ok(())
        }
    }

    #[test]
    fn probe_without_hardware_range_acks_software_fallback() {
        // Detection (a): the probe-on-open must emit a generation-tagged
        // SoftwareFallback for a controller reporting no hardware range.
        let (ack_tx, ack_rx) = unbounded();
        let opener: crate::ControllerOpener =
            Box::new(|| Some(Box::new(NoHardware) as Box<dyn BrightnessController>));
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(5), 9, ack_tx);

        let mut saw_fallback = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(ack) = ack_rx.recv_timeout(Duration::from_millis(200))
                && matches!(ack.outcome, AckOutcome::SoftwareFallback { generation: 9 })
            {
                saw_fallback = true;
                break;
            }
        }
        assert!(
            saw_fallback,
            "a probe with no hardware range must ack SoftwareFallback with the worker's generation"
        );

        handle.cmd_tx.send(WorkerCommand::Shutdown).unwrap();
        handle.join.join().unwrap();
    }

    #[test]
    fn healthy_controller_never_acks_software_fallback() {
        // False-downgrade guard at the worker level: a controller that probes
        // hardware-backed and whose read-back reflects writes must never emit a
        // SoftwareFallback, even after a meaningful first write.
        let (writes_tx, _writes_rx) = unbounded();
        let (ack_tx, ack_rx) = unbounded();
        let opener: crate::ControllerOpener = Box::new(move || {
            Some(Box::new(Recording::new(writes_tx)) as Box<dyn BrightnessController>)
        });
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(5), 1, ack_tx);

        // Learn the level (50), then perform a meaningful write (10); Recording
        // reflects writes, so the read-back proves the panel moved.
        handle
            .cmd_tx
            .send(WorkerCommand::Get {
                feature: Feature::Brightness,
                seq: 1,
            })
            .unwrap();
        handle
            .cmd_tx
            .send(WorkerCommand::Set {
                feature: Feature::Brightness,
                raw: 10,
                seq: 2,
            })
            .unwrap();

        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            if let Ok(ack) = ack_rx.recv_timeout(Duration::from_millis(100)) {
                assert!(
                    !matches!(ack.outcome, AckOutcome::SoftwareFallback { .. }),
                    "a healthy, reflecting controller must never emit SoftwareFallback"
                );
            }
        }

        handle.cmd_tx.send(WorkerCommand::Shutdown).unwrap();
        handle.join.join().unwrap();
    }

    /// A controller whose probe fails transiently (an asleep/busy monitor) and
    /// whose reads/writes also time out — it reports NO capabilities and NO
    /// movement, yet none of that is a *definitive* no-hardware signal.
    #[derive(Debug)]
    struct AsleepController;

    impl BrightnessController for AsleepController {
        fn probe(&mut self) -> Result<Capabilities, ControlError> {
            Err(ControlError::Timeout)
        }
        fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
            Err(ControlError::Timeout)
        }
        fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
            Err(ControlError::Timeout)
        }
    }

    #[test]
    fn probe_error_never_acks_software_fallback() {
        // The false-software-only fix at the worker seam: a probe that fails
        // transiently (a live monitor that was asleep/busy at open time) must NOT
        // be treated as "no hardware". `probe_on_open` only downgrades on a
        // *definitive* `Ok(caps)` with `!hardware_range`; an `Err` is inconclusive,
        // so no SoftwareFallback is emitted and the display stays hardware-backed.
        let (ack_tx, ack_rx) = unbounded();
        let opener: crate::ControllerOpener =
            Box::new(|| Some(Box::new(AsleepController) as Box<dyn BrightnessController>));
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(5), 4, ack_tx);

        // Drive a write too, so both detection paths (probe + first write) get a
        // chance; a transient write failure must likewise never downgrade.
        handle
            .cmd_tx
            .send(WorkerCommand::Set {
                feature: Feature::Brightness,
                raw: 10,
                seq: 1,
            })
            .unwrap();

        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            if let Ok(ack) = ack_rx.recv_timeout(Duration::from_millis(100)) {
                assert!(
                    !matches!(ack.outcome, AckOutcome::SoftwareFallback { .. }),
                    "a transient probe/read/write failure must never emit SoftwareFallback"
                );
            }
        }

        handle.cmd_tx.send(WorkerCommand::Shutdown).unwrap();
        handle.join.join().unwrap();
    }

    #[test]
    fn failed_open_acks_open_failed_and_exits() {
        // A deferred open that returns None must report OpenFailed and the
        // worker thread must exit without entering its loop.
        let (ack_tx, ack_rx) = unbounded();
        let opener: crate::ControllerOpener = Box::new(|| None);
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(5), 7, ack_tx);

        let ack = ack_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker never acked the failed open");
        assert!(
            matches!(ack.outcome, AckOutcome::OpenFailed { generation: 7 }),
            "expected OpenFailed with the worker's generation, got {:?}",
            ack.outcome
        );
        // The thread exited on its own; joining must not hang.
        handle.join.join().unwrap();
    }
}

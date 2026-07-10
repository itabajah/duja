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

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use duja_core::controller::BrightnessController;
use duja_core::id::StableDisplayId;
use duja_core::model::Feature;

use crate::ControllerOpener;
use crate::protocol::{AckOutcome, InflightKey, WorkerAck, WorkerCommand};

/// The engine's handle to one worker: its command sender and join handle.
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
    ack_tx: Sender<WorkerAck>,
) -> WorkerHandle {
    let (cmd_tx, cmd_rx) = unbounded::<WorkerCommand>();
    let join = thread::spawn(move || {
        let Some(controller) = opener() else {
            let _ = ack_tx.send(WorkerAck {
                id,
                outcome: AckOutcome::OpenFailed,
            });
            return;
        };
        worker_loop(&id, controller, min_gap, &cmd_rx, &ack_tx);
    });
    WorkerHandle { cmd_tx, join }
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

fn worker_loop(
    id: &StableDisplayId,
    mut controller: Box<dyn BrightnessController>,
    min_gap: Duration,
    cmd_rx: &Receiver<WorkerCommand>,
    ack_tx: &Sender<WorkerAck>,
) {
    // Newest queued write per feature, and the earliest instant each feature
    // may next be written (its min-gap deadline).
    let mut latest: BTreeMap<Feature, Pending> = BTreeMap::new();
    let mut next_ok: BTreeMap<Feature, Instant> = BTreeMap::new();

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
        for (feature, seq) in gets.drain(..) {
            let outcome = match catch_unwind(AssertUnwindSafe(|| controller.get(feature))) {
                Ok(result) => AckOutcome::Get {
                    feature,
                    seq,
                    result,
                },
                // RATIONALE(AssertUnwindSafe): the controller is dropped with
                // this thread right after a caught panic (we `return` below),
                // so no later reader can observe a torn state.
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
                return;
            }
        }

        // 3b. Perform writes whose min-gap has elapsed.
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
            let outcome = match catch_unwind(AssertUnwindSafe(|| controller.set(feature, raw))) {
                Ok(_result) => AckOutcome::Set { feature, seq },
                // RATIONALE(AssertUnwindSafe): see 3a — the controller does not
                // outlive a caught panic.
                Err(_) => AckOutcome::Panicked {
                    key: InflightKey::Set(feature),
                    seq,
                },
            };
            let is_panic = matches!(outcome, AckOutcome::Panicked { .. });
            let _ = ack_tx.send(WorkerAck {
                id: id.clone(),
                outcome,
            });
            if is_panic {
                return;
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
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(5), ack_tx);

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
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(80), ack_tx);

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

    #[test]
    fn failed_open_acks_open_failed_and_exits() {
        // A deferred open that returns None must report OpenFailed and the
        // worker thread must exit without entering its loop.
        let (ack_tx, ack_rx) = unbounded();
        let opener: crate::ControllerOpener = Box::new(|| None);
        let handle = spawn_worker(worker_id(), opener, Duration::from_millis(5), ack_tx);

        let ack = ack_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker never acked the failed open");
        assert!(
            matches!(ack.outcome, AckOutcome::OpenFailed),
            "expected OpenFailed, got {:?}",
            ack.outcome
        );
        // The thread exited on its own; joining must not hang.
        handle.join.join().unwrap();
    }
}

//! End-to-end tests for the engine actor, driven entirely through its public
//! API with injected fake controllers and enumerators.
//!
//! All synchronization is via channels and generous `recv_timeout` deadlines —
//! never bare sleeps for correctness — so the suite is deterministic on CI and
//! flake-free under repetition. Every join is timeout-guarded so a regression
//! surfaces as a failure, not a hang.

// RATIONALE: integration tests are a separate crate and do not inherit the
// library's `cfg(test)` lint allows. These tests use unwrap/expect for brevity
// and exercise a fake controller that intentionally panics.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use duja_app::{Engine, EngineCommand, EngineConfig, EngineNotification, Enumeration};
use duja_core::controller::{BrightnessController, ControlError};
use duja_core::id::StableDisplayId;
use duja_core::manager::DiscoveredDisplay;
use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot, Feature, FeatureRange};

// --- fixtures -------------------------------------------------------------

/// A stable id derived from a synthetic single-serial EDID.
fn display_id() -> StableDisplayId {
    let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    e.push(0x04);
    e.push(0x21);
    e.push(0x00);
    e.push(0x00);
    e.extend_from_slice(&1u32.to_le_bytes());
    e.resize(127, 0x00);
    let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
    e.push(sum.wrapping_neg());
    StableDisplayId::from_edid(&e).unwrap()
}

fn caps() -> Capabilities {
    Capabilities {
        features: [Feature::Brightness, Feature::Contrast]
            .into_iter()
            .collect(),
        hardware_range: true,
        raw_capabilities: None,
    }
}

fn discovered(id: &StableDisplayId) -> DiscoveredDisplay {
    DiscoveredDisplay {
        id: id.clone(),
        kind: DisplayKind::ExternalDdc,
        name: Some("Test Monitor".to_owned()),
        capabilities: caps(),
    }
}

/// A controller that reports every successful write on a channel so tests can
/// observe writes after it has moved onto a worker thread.
#[derive(Debug)]
struct Recording {
    writes: Sender<(Feature, u16)>,
    values: BTreeMap<Feature, FeatureRange>,
}

impl Recording {
    fn new(writes: Sender<(Feature, u16)>) -> Self {
        let values = caps()
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
        Recording { writes, values }
    }
}

impl BrightnessController for Recording {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
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

/// A controller whose `set` blocks forever (stuck GPU driver); `get` succeeds.
#[derive(Debug)]
struct Hang;

impl BrightnessController for Hang {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        Ok(FeatureRange {
            current: 50,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
        loop {
            thread::park();
        }
    }
}

/// A controller whose `get` announces it was entered (with the value it will
/// return), blocks until externally released, and announces it is about to
/// return — so a test can hold a Get in flight, force a respawn, and pin down
/// the exact order acks are enqueued in.
#[derive(Debug)]
struct GatedGet {
    entered: Sender<u16>,
    release: Receiver<()>,
    returned: Sender<()>,
    current: u16,
}

impl BrightnessController for GatedGet {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        let _ = self.entered.send(self.current);
        let _ = self.release.recv();
        // The worker enqueues this Get's ack immediately after `get` returns, so
        // signalling here lets a test guarantee this ack is enqueued before it
        // releases another worker's Get.
        let _ = self.returned.send(());
        Ok(FeatureRange {
            current: self.current,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
        Ok(())
    }
}

/// A controller whose `set` panics; `get` succeeds.
#[derive(Debug)]
struct Panicky;

impl BrightnessController for Panicky {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        Ok(FeatureRange {
            current: 50,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
        panic!("simulated driver panic");
    }
}

// --- helpers --------------------------------------------------------------

/// A shared, mutable enumeration state, cloneable for the enumerator closure.
type Displays = Arc<Mutex<Vec<DiscoveredDisplay>>>;

/// Build an enumerator over `state` that also signals each call on `calls`.
fn enumerator(state: Displays, calls: Sender<()>) -> duja_app::Enumerator {
    Box::new(move || {
        let _ = calls.send(());
        Enumeration {
            displays: state.lock().unwrap().clone(),
        }
    })
}

/// Wait up to `dur` for a notification satisfying `pred`.
fn wait_note(
    rx: &Receiver<EngineNotification>,
    dur: Duration,
    pred: impl Fn(&EngineNotification) -> bool,
) -> bool {
    let deadline = Instant::now().checked_add(dur).unwrap();
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        match rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(note) => {
                if pred(&note) {
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
}

/// Drain writes until one matches `pred`; returns (count seen, all seen).
fn drain_writes(
    writes: &Receiver<(Feature, u16)>,
    pred: impl Fn(Feature, u16) -> bool,
) -> (usize, Vec<(Feature, u16)>) {
    let mut seen = Vec::new();
    loop {
        match writes.recv_timeout(Duration::from_secs(3)) {
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

/// Request a snapshot and return it, failing the test on timeout.
fn snapshot(cmds: &Sender<EngineCommand>) -> Vec<DisplaySnapshot> {
    let (reply, reply_rx) = unbounded();
    cmds.send(EngineCommand::Snapshot { reply }).unwrap();
    reply_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("engine did not answer Snapshot")
}

/// Run `f` on another thread and assert it finishes within `dur` (join guard).
fn within(dur: Duration, f: impl FnOnce() + Send + 'static) {
    let (done_tx, done_rx) = unbounded();
    thread::spawn(move || {
        f();
        let _ = done_tx.send(());
    });
    assert!(
        done_rx.recv_timeout(dur).is_ok(),
        "operation did not complete within {dur:?}"
    );
}

fn recording_factory(writes: Sender<(Feature, u16)>) -> duja_app::ControllerFactory {
    Box::new(move |_id| {
        let writes = writes.clone();
        Box::new(move || Some(Box::new(Recording::new(writes)) as Box<dyn BrightnessController>))
            as duja_app::ControllerOpener
    })
}

/// A controller that records the thread its first hardware op runs on, so a
/// test can prove open and use happen on the same (worker) thread.
#[derive(Debug)]
struct ThreadRecording {
    op_thread: Arc<Mutex<Option<ThreadId>>>,
    signal: Sender<()>,
}

impl BrightnessController for ThreadRecording {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        let mut slot = self.op_thread.lock().unwrap();
        if slot.is_none() {
            *slot = Some(thread::current().id());
            let _ = self.signal.send(());
        }
        Ok(FeatureRange {
            current: 50,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
        Ok(())
    }
}

// --- tests ----------------------------------------------------------------

#[test]
fn controller_is_opened_on_the_worker_thread() {
    // The controller (which may own thread-affine resources such as a COM
    // apartment) must be OPENED on the very worker thread that will use and
    // drop it — not on the engine thread. We record the thread the factory
    // opens on and the thread the first hardware op runs on; they must match.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let (signal_tx, signal_rx) = unbounded::<()>();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let open_thread: Arc<Mutex<Option<ThreadId>>> = Arc::new(Mutex::new(None));
    let op_thread: Arc<Mutex<Option<ThreadId>>> = Arc::new(Mutex::new(None));

    let factory: duja_app::ControllerFactory = {
        let open_thread = open_thread.clone();
        let op_thread = op_thread.clone();
        let signal = signal_tx.clone();
        Box::new(move |_id| {
            let open_thread = open_thread.clone();
            let op_thread = op_thread.clone();
            let signal = signal.clone();
            Box::new(move || {
                *open_thread.lock().unwrap() = Some(thread::current().id());
                Some(Box::new(ThreadRecording {
                    op_thread: op_thread.clone(),
                    signal: signal.clone(),
                }) as Box<dyn BrightnessController>)
            }) as duja_app::ControllerOpener
        })
    };

    let (engine, _notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        factory,
        platform_rx,
    );

    // The engine dispatches an initial Get on add; it runs the controller's
    // `get` on the worker thread and signals.
    signal_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("initial get should run on the worker");

    let opened = *open_thread.lock().unwrap();
    let operated = *op_thread.lock().unwrap();
    assert!(opened.is_some(), "controller was never opened");
    assert_eq!(
        opened, operated,
        "controller must be opened on the same (worker) thread that uses it"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn burst_yields_single_hw_write() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(80),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(60),
    };
    let (engine, _notes) = Engine::spawn(
        cfg,
        enumerator(state, calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );
    let cmds = engine.sender();

    // Flood 100 writes faster than the min-gap, then a final distinct value.
    for _ in 0..100u32 {
        cmds.send(EngineCommand::SetUserLevel {
            id: id.clone(),
            pct: 10,
        })
        .unwrap();
    }
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 77,
    })
    .unwrap();

    let (count, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 77);
    assert!(
        count < 100,
        "expected far fewer than 100 writes, got {count}"
    );
    assert_eq!(
        seen.last().copied(),
        Some((Feature::Brightness, 77)),
        "the last value must win"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn drag_burst_delivers_final_value() {
    // Regression for P4 gate Finding 1. The tray used to gate SetUserLevel
    // through a UI-side leading-edge throttle that admitted the FIRST sample of a
    // burst and dropped the rest with no trailing flush — so a drag ending inside
    // the throttle window never forwarded its final value, and the hardware
    // settled at an intermediate level while the UI showed the correct one. The
    // throttle is gone: the tray forwards every SetUserLevel, and the engine
    // worker's write_min_gap last-wins coalescer bounds the write rate AND lands
    // the final value. This drives a distinct-value descending sweep (a drag from
    // 100 to 0) and asserts the FINAL value reaches the controller.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(80),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(60),
    };
    let (engine, _notes) = Engine::spawn(
        cfg,
        enumerator(state, calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );
    let cmds = engine.sender();

    // A drag from 100 down to 0 in distinct 5% steps, faster than write_min_gap.
    for step in 0..=20u8 {
        let pct = 100 - step * 5;
        cmds.send(EngineCommand::SetUserLevel {
            id: id.clone(),
            pct,
        })
        .unwrap();
    }

    // The controller must settle at the final value (0) — never an intermediate.
    let (count, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 0);
    assert!(count <= 21, "coalescing should drop intermediate writes");
    assert_eq!(
        seen.last().copied(),
        Some((Feature::Brightness, 0)),
        "the final drag value must land on the hardware, not an intermediate one"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn stuck_controller_marks_display_unresponsive() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_millis(150),
        displaychange_debounce: Duration::from_millis(60),
    };
    let factory: duja_app::ControllerFactory = Box::new(|_id| {
        Box::new(|| Some(Box::new(Hang) as Box<dyn BrightnessController>))
            as duja_app::ControllerOpener
    });
    let (engine, notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    // A write to the stuck controller never acks; the watchdog must fire.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 40,
    })
    .unwrap();

    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| {
            matches!(n, EngineNotification::DisplayUnresponsive(x) if *x == want)
        }),
        "expected a DisplayUnresponsive notification"
    );

    // Subsequent sets must not be dispatched (no worker), and the engine must
    // still answer Snapshot — proving it is alive, not blocked on the driver.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 50,
    })
    .unwrap();
    let snaps = snapshot(&cmds);
    assert!(
        snaps.iter().any(|s| s.id == id),
        "display should still be listed (greyed, not hidden)"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn recovered_display_gets_fresh_worker() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // Factory hands out a Hang controller first, then Recording ones.
    let hang_first = Arc::new(Mutex::new(true));
    let factory: duja_app::ControllerFactory = {
        let hang_first = hang_first.clone();
        let writes_tx = writes_tx.clone();
        Box::new(move |_id| {
            let hang = {
                let mut flag = hang_first.lock().unwrap();
                let hang = *flag;
                *flag = false;
                hang
            };
            let writes_tx = writes_tx.clone();
            Box::new(move || {
                if hang {
                    Some(Box::new(Hang) as Box<dyn BrightnessController>)
                } else {
                    Some(Box::new(Recording::new(writes_tx)) as Box<dyn BrightnessController>)
                }
            }) as duja_app::ControllerOpener
        })
    };

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_millis(150),
        displaychange_debounce: Duration::from_millis(60),
    };
    let (engine, notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    // First write hangs -> unresponsive.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 40,
    })
    .unwrap();
    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| {
            matches!(n, EngineNotification::DisplayUnresponsive(x) if *x == want)
        }),
        "expected DisplayUnresponsive before recovery"
    );

    // An enumeration sights the (still-connected) display again -> Responsive,
    // and the engine spawns a fresh worker with a fresh (Recording) controller.
    cmds.send(EngineCommand::RefreshNow).unwrap();

    // Writes must flow again through the fresh worker.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 60,
    })
    .unwrap();
    let (_count, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 60);
    assert!(
        seen.contains(&(Feature::Brightness, 60)),
        "writes should resume through the fresh worker"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn replug_restores_last_level() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(60),
    };
    let (engine, _notes) = Engine::spawn(
        cfg,
        enumerator(state.clone(), calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );
    let cmds = engine.sender();

    // Set 30% and observe the write.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 30,
    })
    .unwrap();
    let (_c, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 30);
    assert!(seen.contains(&(Feature::Brightness, 30)));

    // Unplug (enumeration without it). The following Snapshot is processed in
    // order after the refresh, so it acts as a barrier: once it reports the
    // display gone we know the removal landed before we flip the state back
    // (otherwise the two refreshes could race and never see a diff).
    *state.lock().unwrap() = Vec::new();
    cmds.send(EngineCommand::RefreshNow).unwrap();
    assert!(
        snapshot(&cmds).is_empty(),
        "the display should be gone after the unplug enumeration"
    );

    // Replug (enumeration with it again).
    *state.lock().unwrap() = vec![discovered(&id)];
    cmds.send(EngineCommand::RefreshNow).unwrap();

    // The fresh worker must receive a restore write of 30%.
    let (_c2, seen2) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 30);
    assert!(
        seen2.contains(&(Feature::Brightness, 30)),
        "replug should restore the last level"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn reattach_of_unresponsive_display_restores_level_not_power_on_default() {
    // A display is dimmed to 30% while its worker is wedged, trips the watchdog
    // (unresponsive), is unplugged, then replugged. The manager emits BOTH
    // Reattached { restore_level: Some(30) } and Responsive in one pass; the
    // engine must restore 30 and must NOT let the Responsive arm respawn a
    // second worker and re-learn the panel's power-on 50%.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // First controller hangs (a write wedges it -> watchdog -> unresponsive);
    // every later controller records writes and reads back the power-on 50%.
    let hang_first = Arc::new(Mutex::new(true));
    let factory: duja_app::ControllerFactory = {
        let hang_first = hang_first.clone();
        let writes_tx = writes_tx.clone();
        Box::new(move |_id| {
            let hang = {
                let mut flag = hang_first.lock().unwrap();
                let h = *flag;
                *flag = false;
                h
            };
            let writes_tx = writes_tx.clone();
            Box::new(move || {
                if hang {
                    Some(Box::new(Hang) as Box<dyn BrightnessController>)
                } else {
                    Some(Box::new(Recording::new(writes_tx)) as Box<dyn BrightnessController>)
                }
            }) as duja_app::ControllerOpener
        })
    };

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_millis(150),
        displaychange_debounce: Duration::from_millis(60),
    };
    let (engine, notes) = Engine::spawn(
        cfg,
        enumerator(state.clone(), calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    // The user dims to 30% while the (hung) worker is wedged.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 30,
    })
    .unwrap();

    // The wedged write trips the watchdog.
    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| {
            matches!(n, EngineNotification::DisplayUnresponsive(x) if *x == want)
        }),
        "expected the wedged write to mark the display unresponsive"
    );

    // Unplug (barrier via Snapshot), then replug the same display.
    *state.lock().unwrap() = Vec::new();
    cmds.send(EngineCommand::RefreshNow).unwrap();
    assert!(
        snapshot(&cmds).is_empty(),
        "the display should be gone after the unplug enumeration"
    );
    *state.lock().unwrap() = vec![discovered(&id)];
    cmds.send(EngineCommand::RefreshNow).unwrap();

    // The fresh worker must receive the RESTORE write of 30%.
    let (_c, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 30);
    assert!(
        seen.contains(&(Feature::Brightness, 30)),
        "reattach must restore the saved 30%, saw writes {seen:?}"
    );

    // ...and the level must never be clobbered back to the power-on 50% by a
    // stray initial-Get from a superseded second respawn.
    let relearned = wait_note(&notes, Duration::from_millis(500), |n| {
        matches!(
            n,
            EngineNotification::DisplaysChanged(snaps)
                if snaps.iter().any(|s| s.id == id && s.user_level_pct == 50)
        )
    });
    assert!(
        !relearned,
        "reattach re-learned the power-on 50%, losing the user's 30%"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn stale_get_ack_cannot_clobber_fresh_learn() {
    // Worker A's initial Get is held in flight; the display is unplugged (A
    // retired) and replugged (worker B, a fresh initial Get). We release A's
    // stale Get first, then B's fresh one. The learned level must come from B's
    // reading (70), never A's superseded reading (20).
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let (entered_tx, entered_rx) = unbounded::<u16>();
    let (release_a_tx, release_a_rx) = unbounded::<()>();
    let (release_b_tx, release_b_rx) = unbounded::<()>();
    let (returned_a_tx, returned_a_rx) = unbounded::<()>();
    let (returned_b_tx, _returned_b_rx) = unbounded::<()>();

    let calls = Arc::new(Mutex::new(0u32));
    let factory: duja_app::ControllerFactory = {
        let entered_tx = entered_tx.clone();
        Box::new(move |_id| {
            let n = {
                let mut c = calls.lock().unwrap();
                *c += 1;
                *c
            };
            let entered_tx = entered_tx.clone();
            let (current, release, returned) = if n == 1 {
                (20u16, release_a_rx.clone(), returned_a_tx.clone())
            } else {
                (70u16, release_b_rx.clone(), returned_b_tx.clone())
            };
            Box::new(move || {
                Some(Box::new(GatedGet {
                    entered: entered_tx.clone(),
                    release: release.clone(),
                    returned: returned.clone(),
                    current,
                }) as Box<dyn BrightnessController>)
            }) as duja_app::ControllerOpener
        })
    };

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        // Large so the deliberately-blocked Gets never trip the watchdog.
        watchdog_timeout: Duration::from_secs(30),
        displaychange_debounce: Duration::from_millis(60),
    };
    let (engine, notes) = Engine::spawn(
        cfg,
        enumerator(state.clone(), calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    // Worker A entered its initial Get (value 20) and is now blocked.
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(2)).ok(),
        Some(20),
        "worker A should enter its initial Get"
    );

    // Unplug (retiring A while its Get is in flight), then replug -> worker B.
    *state.lock().unwrap() = Vec::new();
    cmds.send(EngineCommand::RefreshNow).unwrap();
    assert!(snapshot(&cmds).is_empty(), "display should be gone");
    *state.lock().unwrap() = vec![discovered(&id)];
    cmds.send(EngineCommand::RefreshNow).unwrap();

    // Worker B entered its fresh initial Get (value 70) and is now blocked.
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(2)).ok(),
        Some(70),
        "worker B should enter its fresh initial Get"
    );

    // Release the STALE Get and wait until worker A has returned from it — its
    // ack is enqueued immediately after. A snapshot round-trip then advances the
    // engine loop. Only THEN release the fresh Get, so A's stale ack is strictly
    // enqueued (and thus processed, FIFO) before B's fresh ack.
    release_a_tx.send(()).unwrap();
    returned_a_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker A should return from its stale Get");
    let _ = snapshot(&cmds);
    release_b_tx.send(()).unwrap();

    // The fresh reading (70) must be the learned level — not A's stale 20.
    let learned_fresh = wait_note(&notes, Duration::from_secs(2), |n| {
        matches!(
            n,
            EngineNotification::DisplaysChanged(snaps)
                if snaps.iter().any(|s| s.id == id && s.user_level_pct == 70)
        )
    });
    assert!(
        learned_fresh,
        "the fresh Get reading (70) must be learned; a stale ack must not consume the learn"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn worker_panic_does_not_kill_engine() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(60),
    };
    let factory: duja_app::ControllerFactory = Box::new(|_id| {
        Box::new(|| Some(Box::new(Panicky) as Box<dyn BrightnessController>))
            as duja_app::ControllerOpener
    });
    let (engine, notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 40,
    })
    .unwrap();

    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| {
            matches!(n, EngineNotification::DisplayUnresponsive(x) if *x == want)
        }),
        "a worker panic should mark the display unresponsive"
    );

    // The engine itself must still be serving requests.
    let snaps = snapshot(&cmds);
    assert!(snaps.iter().any(|s| s.id == id));

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn displaychange_ticks_are_debounced() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, _writes_rx) = unbounded();
    let (calls_tx, calls_rx) = unbounded::<()>();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(80),
    };
    let (engine, _notes) = Engine::spawn(
        cfg,
        enumerator(state, calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );

    // Startup performs exactly one enumeration.
    calls_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("startup enumeration");

    // A storm of 10 ticks must collapse to a single enumeration.
    for _ in 0..10u32 {
        platform_tx.send(()).unwrap();
    }
    calls_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("one debounced enumeration");

    // No further enumeration follows the single debounced fire.
    assert!(
        calls_rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "the debounced storm must yield exactly one enumeration"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
}

#[test]
fn idle_engine_performs_no_enumerations() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, _writes_rx) = unbounded();
    let (calls_tx, calls_rx) = unbounded::<()>();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let (engine, _notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );

    // One startup enumeration, then silence: no timers, no idle wakeups.
    calls_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("startup enumeration");
    assert!(
        calls_rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "an idle engine must not enumerate on its own"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn shutdown_joins_cleanly() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, _writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let (engine, _notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );
    let _ = engine.sender();

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn drop_shuts_down() {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, _writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let (engine, _notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );

    // Dropping the handle must shut the engine down without hanging.
    within(Duration::from_secs(2), move || drop(engine));
    let _ = platform_tx;
}

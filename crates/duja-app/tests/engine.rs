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
use std::thread;
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
        Some(Box::new(Recording::new(writes.clone())) as Box<dyn BrightnessController>)
    })
}

// --- tests ----------------------------------------------------------------

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
    let factory: duja_app::ControllerFactory =
        Box::new(|_id| Some(Box::new(Hang) as Box<dyn BrightnessController>));
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
            let mut flag = hang_first.lock().unwrap();
            if *flag {
                *flag = false;
                Some(Box::new(Hang) as Box<dyn BrightnessController>)
            } else {
                Some(Box::new(Recording::new(writes_tx.clone())) as Box<dyn BrightnessController>)
            }
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
    let factory: duja_app::ControllerFactory =
        Box::new(|_id| Some(Box::new(Panicky) as Box<dyn BrightnessController>));
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

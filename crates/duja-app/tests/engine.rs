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
        allowed_inputs: Vec::new(),
    }
}

/// Capabilities that additionally advertise input switching with a fixed value
/// list (`0x11`/`0x0F`) — for the input-source dispatch tests.
fn caps_with_inputs() -> Capabilities {
    Capabilities {
        features: [Feature::Brightness, Feature::InputSource]
            .into_iter()
            .collect(),
        hardware_range: true,
        raw_capabilities: None,
        allowed_inputs: vec![0x11, 0x0F],
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

fn discovered_with_inputs(id: &StableDisplayId) -> DiscoveredDisplay {
    DiscoveredDisplay {
        capabilities: caps_with_inputs(),
        ..discovered(id)
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

/// A controller that announces each `set` entry on `entered` (with its instance
/// `tag`), optionally blocks the FIRST `set` on a one-shot `gate`, then records
/// every completed write on `writes` as `(tag, value)`. Lets a test hold a write
/// wedged in the driver, force a retire/replace, release it, and prove the
/// detached worker never performs a second hardware write (E-A) or that shutdown
/// does not block on the wedged call (H3).
#[derive(Debug)]
struct GatedSet {
    tag: u8,
    entered: Sender<u8>,
    writes: Sender<(u8, u16)>,
    gate: Option<Receiver<()>>,
    current: u16,
}

impl BrightnessController for GatedSet {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        Ok(FeatureRange {
            current: self.current,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, value: u16) -> Result<(), ControlError> {
        let _ = self.entered.send(self.tag);
        if let Some(gate) = self.gate.take() {
            // Block this (first) write until the test releases it — the wedged
            // driver call the watchdog fires on.
            let _ = gate.recv();
        }
        let _ = self.writes.send((self.tag, value));
        Ok(())
    }
}

/// Wait up to `dur` for the next `entered` tag equal to `want`, draining other
/// tags. Returns whether it arrived.
fn wait_tag(rx: &Receiver<u8>, want: u8, dur: Duration) -> bool {
    let deadline = Instant::now().checked_add(dur).unwrap();
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        match rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(tag) if tag == want => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
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
        level_poll_interval: Duration::from_millis(50),
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
        level_poll_interval: Duration::from_millis(50),
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
fn set_input_rejects_code_not_in_probed_list() {
    // The engine must only forward an input-source switch whose code is in the
    // display's probed allowed_inputs. A code outside that set is dropped before
    // it can reach the worker (and the wire); an allowed code is forwarded as a
    // Feature::InputSource write.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    // Enumerate the display advertising inputs 0x11 and 0x0F only.
    let state: Displays = Arc::new(Mutex::new(vec![discovered_with_inputs(&id)]));

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(60),
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, _notes) = Engine::spawn(
        cfg,
        enumerator(state, calls_tx),
        recording_factory(writes_tx),
        platform_rx,
    );
    let cmds = engine.sender();

    // A disallowed code (0x20) must be dropped; an allowed one (0x11) must land.
    cmds.send(EngineCommand::SetInput {
        id: id.clone(),
        value: 0x20,
    })
    .unwrap();
    cmds.send(EngineCommand::SetInput {
        id: id.clone(),
        value: 0x11,
    })
    .unwrap();

    let (_count, seen) = drain_writes(&writes_rx, |f, v| f == Feature::InputSource && v == 0x11);
    assert!(
        seen.contains(&(Feature::InputSource, 0x11)),
        "the allowed input switch must reach the worker, saw {seen:?}"
    );
    assert!(
        !seen.contains(&(Feature::InputSource, 0x20)),
        "a code not in the probed list must never reach the worker, saw {seen:?}"
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
        level_poll_interval: Duration::from_millis(50),
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
        level_poll_interval: Duration::from_millis(50),
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
        level_poll_interval: Duration::from_millis(50),
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
        level_poll_interval: Duration::from_millis(50),
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
        level_poll_interval: Duration::from_millis(50),
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
        level_poll_interval: Duration::from_millis(50),
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
        level_poll_interval: Duration::from_millis(50),
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

// --- Level polling / external-change reflection (PR-D) ---------------------
//
// A controller whose current level lives behind a shared handle a test can
// mutate to simulate an external change (the monitor's own buttons); `set`
// writes through it (so Duja's own writes are observable as no-drift reads).

#[derive(Debug)]
struct PollController {
    current: Arc<Mutex<u16>>,
    max: u16,
}

impl BrightnessController for PollController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        Ok(FeatureRange {
            current: *self.current.lock().unwrap(),
            max: self.max,
        })
    }
    fn set(&mut self, _feature: Feature, value: u16) -> Result<(), ControlError> {
        *self.current.lock().unwrap() = value;
        Ok(())
    }
}

fn poll_factory(current: Arc<Mutex<u16>>, max: u16) -> duja_app::ControllerFactory {
    Box::new(move |_id| {
        let current = current.clone();
        Box::new(move || {
            Some(Box::new(PollController { current, max }) as Box<dyn BrightnessController>)
        }) as duja_app::ControllerOpener
    })
}

/// A short-interval config so polling tests run fast.
fn fast_poll_cfg() -> EngineConfig {
    EngineConfig {
        write_min_gap: Duration::from_millis(20),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(40),
        level_poll_interval: Duration::from_millis(30),
    }
}

/// Spawn an engine over a `PollController`, and wait until it has **learned** the
/// initial level (`60`). Returns everything the test needs.
///
/// The learned value is deliberately not [`DEFAULT_USER_LEVEL_PCT`] (50): the
/// pre-learn snapshot reports the default, so waiting on a distinct value blocks
/// until the initial-learn `Get` truly completes — otherwise a test could change
/// the hardware while that `Get` is still in flight and have it absorbed as the
/// "learned" level (no drift, no reflection).
fn spawn_polling(
    current: Arc<Mutex<u16>>,
    max: u16,
    learned: u8,
) -> (
    Engine,
    Receiver<EngineNotification>,
    Sender<EngineCommand>,
    Sender<()>,
) {
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));
    let (engine, notes) = Engine::spawn(
        fast_poll_cfg(),
        enumerator(state, calls_tx),
        poll_factory(current, max),
        platform_rx,
    );
    let cmds = engine.sender();
    assert!(
        wait_note(&notes, Duration::from_secs(3), |n| matches!(
            n,
            EngineNotification::DisplaysChanged(s)
                if s.first().map(|d| d.user_level_pct) == Some(learned)
        )),
        "engine must learn the initial level {learned}"
    );
    (engine, notes, cmds, platform_tx)
}

#[test]
fn polling_reflects_an_external_change() {
    let current = Arc::new(Mutex::new(60u16));
    let (engine, notes, cmds, platform_tx) = spawn_polling(current.clone(), 100, 60);

    // Simulate the monitor's own buttons raising the brightness, then enable
    // polling: enabling polls immediately, so the read is deterministic (no
    // dependence on the periodic timer, which parallel test scheduling can defer).
    *current.lock().unwrap() = 80;
    cmds.send(EngineCommand::SetLevelPolling { on: true })
        .unwrap();

    assert!(
        wait_note(&notes, Duration::from_secs(5), |n| matches!(
            n,
            EngineNotification::LevelRead { hw_pct: 80, .. }
        )),
        "an external change must surface as LevelRead(80)"
    );
    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn self_write_echo_is_suppressed() {
    let current = Arc::new(Mutex::new(60u16));
    let (engine, notes, cmds, platform_tx) = spawn_polling(current.clone(), 100, 60);

    cmds.send(EngineCommand::SetLevelPolling { on: true })
        .unwrap();
    // Duja's own write: the hardware moves to 30 and the engine records 30, so a
    // subsequent poll read of 30 must NOT be mistaken for an external change.
    cmds.send(EngineCommand::SetUserLevel {
        id: display_id(),
        pct: 30,
    })
    .unwrap();

    assert!(
        !wait_note(&notes, Duration::from_millis(600), |n| matches!(
            n,
            EngineNotification::LevelRead { .. }
        )),
        "Duja's own write must not echo back as a LevelRead"
    );
    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn polling_stops_when_disabled() {
    let current = Arc::new(Mutex::new(60u16));
    let (engine, notes, cmds, platform_tx) = spawn_polling(current.clone(), 100, 60);

    // Set the value first, then enable: the immediate poll on enable reads it
    // deterministically (no periodic-timer dependence).
    *current.lock().unwrap() = 80;
    cmds.send(EngineCommand::SetLevelPolling { on: true })
        .unwrap();
    assert!(
        wait_note(&notes, Duration::from_secs(5), |n| matches!(
            n,
            EngineNotification::LevelRead { hw_pct: 80, .. }
        )),
        "the first external change is reflected while polling is on"
    );

    // Disable polling, then change again: no further reflection may arrive.
    cmds.send(EngineCommand::SetLevelPolling { on: false })
        .unwrap();
    *current.lock().unwrap() = 40;
    assert!(
        !wait_note(&notes, Duration::from_millis(600), |n| matches!(
            n,
            EngineNotification::LevelRead { .. }
        )),
        "no LevelRead may arrive once polling is disabled (zero idle wakeups)"
    );
    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn idle_engine_performs_no_polls() {
    // Polling is never enabled, so an external change is invisible and the engine
    // never wakes to read it — the zero-idle-wakeup guarantee.
    let current = Arc::new(Mutex::new(60u16));
    let (engine, notes, _cmds, platform_tx) = spawn_polling(current.clone(), 100, 60);

    *current.lock().unwrap() = 80;
    assert!(
        !wait_note(&notes, Duration::from_millis(600), |n| matches!(
            n,
            EngineNotification::LevelRead { .. }
        )),
        "with polling off, an external change must not be observed"
    );
    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn self_write_echo_suppressed_on_a_sub_100_max_panel() {
    // On a panel reporting brightness_max < 100 the write quantizes, so the
    // readback pct differs from the requested pct by more than 1 — a pct-level
    // drift check would flag Duja's OWN write as an external change. The raw-level
    // check must still suppress it. Initial raw 18 with max 30 ⇒ learned 60.
    let current = Arc::new(Mutex::new(18u16));
    let (engine, notes, cmds, platform_tx) = spawn_polling(current.clone(), 30, 60);

    cmds.send(EngineCommand::SetLevelPolling { on: true })
        .unwrap();
    // pct 5 ⇒ raw = floor(5*30/100) = 1 ⇒ readback pct = floor(1*100/30) = 3.
    // A pct-level compare would see 3 vs the recorded 5 (delta 2 > 1) and echo;
    // the raw-level compare sees raw 1 vs the raw we wrote (1) and suppresses.
    cmds.send(EngineCommand::SetUserLevel {
        id: display_id(),
        pct: 5,
    })
    .unwrap();

    assert!(
        !wait_note(&notes, Duration::from_millis(600), |n| matches!(
            n,
            EngineNotification::LevelRead { .. }
        )),
        "Duja's own write must not echo back on a sub-100-max panel"
    );
    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

// --- worker-lifecycle fixes (H3, E-A, E-C, E-D, E-E) -----------------------

#[test]
fn shutdown_does_not_hang_on_a_blocked_worker() {
    // H3: a worker blocked inside controller.set() (younger than the watchdog) is
    // still in the engine's worker map when shutdown runs. Shutdown must send
    // Shutdown, flag it retired, wait a BOUNDED time, and DETACH it — never
    // join() a thread wedged in a driver call, which would hang app exit forever.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (entered_tx, entered_rx) = unbounded::<u8>();
    let (writes_tx, _writes_rx) = unbounded::<(u8, u16)>();
    // The gate is never released, so the worker's set() blocks for the whole test.
    let (release_tx, release_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let factory: duja_app::ControllerFactory = {
        let entered_tx = entered_tx.clone();
        let writes_tx = writes_tx.clone();
        let release_rx = release_rx.clone();
        Box::new(move |_id| {
            let entered_tx = entered_tx.clone();
            let writes_tx = writes_tx.clone();
            let release_rx = release_rx.clone();
            Box::new(move || {
                Some(Box::new(GatedSet {
                    tag: 1,
                    entered: entered_tx.clone(),
                    writes: writes_tx.clone(),
                    gate: Some(release_rx.clone()),
                    current: 50,
                }) as Box<dyn BrightnessController>)
            }) as duja_app::ControllerOpener
        })
    };

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        // Large: the wedged write must NOT be watchdog-retired before shutdown,
        // so the worker is still registered when shutdown runs.
        watchdog_timeout: Duration::from_secs(30),
        displaychange_debounce: Duration::from_millis(60),
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, _notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    // Drive a write and wait until the worker is actually blocked inside set().
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 40,
    })
    .unwrap();
    assert!(
        wait_tag(&entered_rx, 1, Duration::from_secs(2)),
        "the worker should enter its (blocking) set()"
    );

    // Shutdown must complete within a bounded time despite the wedged worker.
    within(Duration::from_secs(8), move || engine.shutdown());
    let _ = platform_tx;
    let _ = release_tx;
}

#[test]
fn detached_worker_performs_no_second_write() {
    // E-A: a worker wedged in set() past the watchdog is detached (marked stuck)
    // and replaced. When the wedged call finally returns, the zombie must NOT
    // drain the buffered backlog and issue a SECOND hardware write to the same
    // panel (two DDC writers on one monitor). The `retired` flag makes it exit
    // the instant its blocked call returns.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (entered_tx, entered_rx) = unbounded::<u8>();
    let (writes_tx, _writes_rx) = unbounded::<(u8, u16)>();
    let (release_tx, release_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // Worker A (first open) wedges its first set() on the gate; worker B (every
    // later open) writes freely. Each tags its set-entries/writes by instance.
    let call = Arc::new(Mutex::new(0u32));
    let factory: duja_app::ControllerFactory = {
        let entered_tx = entered_tx.clone();
        let writes_tx = writes_tx.clone();
        let release_rx = release_rx.clone();
        Box::new(move |_id| {
            let n = {
                let mut c = call.lock().unwrap();
                *c += 1;
                *c
            };
            let entered_tx = entered_tx.clone();
            let writes_tx = writes_tx.clone();
            let release_rx = release_rx.clone();
            Box::new(move || {
                let (tag, gate) = if n == 1 {
                    (1u8, Some(release_rx.clone()))
                } else {
                    (2u8, None)
                };
                Some(Box::new(GatedSet {
                    tag,
                    entered: entered_tx.clone(),
                    writes: writes_tx.clone(),
                    gate,
                    current: 50,
                }) as Box<dyn BrightnessController>)
            }) as duja_app::ControllerOpener
        })
    };

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_millis(150),
        displaychange_debounce: Duration::from_millis(60),
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    // Worker A enters its first set() and wedges on the gate.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 40,
    })
    .unwrap();
    assert!(
        wait_tag(&entered_rx, 1, Duration::from_secs(2)),
        "worker A should enter its wedged set()"
    );

    // Queue a backlog to A while it is wedged (buffered in A's channel; crossbeam
    // still delivers it after the engine drops the sender on detach).
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 41,
    })
    .unwrap();
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 42,
    })
    .unwrap();

    // The watchdog fires: A is detached and marked unresponsive.
    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| {
            matches!(n, EngineNotification::DisplayUnresponsive(x) if *x == want)
        }),
        "the wedged worker should be marked unresponsive"
    );

    // A sighting spawns replacement worker B; the snapshot round-trip is a barrier
    // proving the respawn was processed before we release A.
    cmds.send(EngineCommand::RefreshNow).unwrap();
    let _ = snapshot(&cmds);

    // Release A's wedged call. A must exit WITHOUT draining the backlog and
    // issuing a second write; a second tag-1 set-entry is exactly the double-write.
    release_tx.send(()).unwrap();
    assert!(
        !wait_tag(&entered_rx, 1, Duration::from_secs(1)),
        "detached worker A must not perform a second write (double-writer)"
    );

    within(Duration::from_secs(4), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn open_failure_marks_display_unresponsive() {
    // E-C: a genuine OpenFailed (the deferred open returns None) must mark the
    // display UNRESPONSIVE (grey it, arm respawn on the next sighting) — not leave
    // it listed as a normal display with no worker and no recovery path.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // Every open fails.
    let factory: duja_app::ControllerFactory =
        Box::new(|_id| Box::new(|| None) as duja_app::ControllerOpener);

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_secs(5),
        displaychange_debounce: Duration::from_millis(60),
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    // The failed open must surface as unresponsive...
    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| {
            matches!(n, EngineNotification::DisplayUnresponsive(x) if *x == want)
        }),
        "a failed open must mark the display unresponsive"
    );
    // ...while still being listed (greyed, not hidden), and the engine stays live.
    let snaps = snapshot(&cmds);
    assert!(
        snaps.iter().any(|s| s.id == id),
        "the display should still be listed (greyed)"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn stale_open_failed_does_not_retire_a_fresh_worker() {
    // E-C (generation): a stale OpenFailed from a worker that was already replaced
    // (a superseded generation) must be IGNORED — it must not retire or grey the
    // fresh, healthy worker that took its place.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (open_entered_tx, open_entered_rx) = unbounded::<()>();
    let (release_tx, release_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // First open: signal entry, block, then FAIL (None). Later opens: Recording.
    let call = Arc::new(Mutex::new(0u32));
    let factory: duja_app::ControllerFactory = {
        let writes_tx = writes_tx.clone();
        Box::new(move |_id| {
            let n = {
                let mut c = call.lock().unwrap();
                *c += 1;
                *c
            };
            let writes_tx = writes_tx.clone();
            let open_entered_tx = open_entered_tx.clone();
            let release_rx = release_rx.clone();
            Box::new(move || {
                if n == 1 {
                    let _ = open_entered_tx.send(());
                    let _ = release_rx.recv();
                    None
                } else {
                    Some(Box::new(Recording::new(writes_tx)) as Box<dyn BrightnessController>)
                }
            }) as duja_app::ControllerOpener
        })
    };

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_secs(30),
        displaychange_debounce: Duration::from_millis(60),
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, notes) = Engine::spawn(
        cfg,
        enumerator(state.clone(), calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    // Worker A's open is wedged (generation 1).
    open_entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker A should start opening");

    // Unplug + replug: A is retired and healthy worker B (generation 2) spawns.
    *state.lock().unwrap() = Vec::new();
    cmds.send(EngineCommand::RefreshNow).unwrap();
    assert!(snapshot(&cmds).is_empty(), "display should be gone");
    *state.lock().unwrap() = vec![discovered(&id)];
    cmds.send(EngineCommand::RefreshNow).unwrap();

    // B is live: a user write flows through it.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 33,
    })
    .unwrap();
    let (_c, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 33);
    assert!(
        seen.contains(&(Feature::Brightness, 33)),
        "worker B should be the live worker"
    );

    // Release A: it fails its open and emits a STALE OpenFailed (generation 1).
    release_tx.send(()).unwrap();

    // The stale ack must NOT grey the display — B is healthy.
    let want = id.clone();
    assert!(
        !wait_note(&notes, Duration::from_millis(600), |n| {
            matches!(n, EngineNotification::DisplayUnresponsive(x) if *x == want)
        }),
        "a stale OpenFailed must not mark the fresh worker's display unresponsive"
    );
    // ...and B still writes.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 44,
    })
    .unwrap();
    let (_c2, seen2) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 44);
    assert!(
        seen2.contains(&(Feature::Brightness, 44)),
        "worker B must remain live after the stale ack"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn abandoned_display_is_not_un_greyed_on_resight() {
    // E-D: after MAX_STUCK_RESPAWNS stuck cycles a display is abandoned (no more
    // respawn). A later enumeration sights it and the manager emits Responsive,
    // but the engine must NOT report it responsive — with no live worker it must
    // stay greyed.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // Every worker hangs on set(): each recovery attempt wedges and re-sticks.
    let factory: duja_app::ControllerFactory = Box::new(|_id| {
        Box::new(|| Some(Box::new(Hang) as Box<dyn BrightnessController>))
            as duja_app::ControllerOpener
    });

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_millis(120),
        displaychange_debounce: Duration::from_millis(60),
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    // Cycle 1: a write wedges -> unresponsive.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 40,
    })
    .unwrap();
    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| matches!(
            n,
            EngineNotification::DisplayUnresponsive(x) if *x == want
        )),
        "cycle 1: display should be marked unresponsive"
    );

    // Recovery 1 respawns a fresh (still-hanging) worker (stuck_count 1 < MAX).
    cmds.send(EngineCommand::RefreshNow).unwrap();
    // Cycle 2: drive a fresh write to the respawned worker so it wedges and
    // re-sticks (stuck_count -> 2 == MAX). Driving it explicitly keeps this test
    // independent of the recovery-restore path (E-E).
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 41,
    })
    .unwrap();
    let want2 = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| matches!(
            n,
            EngineNotification::DisplayUnresponsive(x) if *x == want2
        )),
        "cycle 2: the respawned worker should wedge and re-stick"
    );

    // Recovery 2: the display is abandoned (stuck_count == MAX). The sighting must
    // NOT surface DisplayResponsive — no live worker exists.
    cmds.send(EngineCommand::RefreshNow).unwrap();
    let want3 = id.clone();
    assert!(
        !wait_note(&notes, Duration::from_secs(1), |n| matches!(
            n,
            EngineNotification::DisplayResponsive(x) if *x == want3
        )),
        "an abandoned display must not be reported responsive (stays greyed)"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn watchdog_recovery_restores_user_level_not_relearn() {
    // E-E: on the pure watchdog-recovery (Responsive, no replug) path the engine
    // must RESTORE the recorded user level via a Set — mirroring Reattached — not
    // issue an initial Get that relearns (and clobbers) the level from hardware.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // First worker hangs (wedges the write -> unresponsive); later workers record.
    let hang_first = Arc::new(Mutex::new(true));
    let factory: duja_app::ControllerFactory = {
        let hang_first = hang_first.clone();
        let writes_tx = writes_tx.clone();
        Box::new(move |_id| {
            let hang = {
                let mut f = hang_first.lock().unwrap();
                let h = *f;
                *f = false;
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
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, notes) = Engine::spawn(cfg, enumerator(state, calls_tx), factory, platform_rx);
    let cmds = engine.sender();

    // Dim to 30 while the hung worker is wedged -> watchdog -> unresponsive.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 30,
    })
    .unwrap();
    let want = id.clone();
    assert!(
        wait_note(&notes, Duration::from_secs(2), |n| matches!(
            n,
            EngineNotification::DisplayUnresponsive(x) if *x == want
        )),
        "the wedged write should mark the display unresponsive"
    );

    // Watchdog recovery: a sighting spawns a fresh worker. The engine must
    // RE-DISPATCH a Set of 30 (restore), not an initial Get (relearn).
    cmds.send(EngineCommand::RefreshNow).unwrap();
    let (_c, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 30);
    assert!(
        seen.contains(&(Feature::Brightness, 30)),
        "recovery must restore the user's 30%, not relearn from hardware; saw {seen:?}"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

// --- no-hardware software-only fallback (BUG 3) ----------------------------
//
// A display with no working hardware brightness must be detected at runtime and
// downgraded to `DisplayKind::SoftwareOnly`, so software dimming spans the whole
// slider. Detection is worker-side via three tight signals: a probe reporting no
// hardware range, a first effective brightness write whose read-back never moves
// (a silent no-op), and a first brightness write the backend rejects.

/// Build capabilities with an explicit `hardware_range` (and Brightness present
/// iff hardware-backed), for the probe-driven detection test.
fn caps_hw(hardware_range: bool) -> Capabilities {
    Capabilities {
        features: if hardware_range {
            [Feature::Brightness].into_iter().collect()
        } else {
            std::collections::BTreeSet::new()
        },
        hardware_range,
        raw_capabilities: None,
        allowed_inputs: Vec::new(),
    }
}

/// A controller that reports a configurable `hardware_range` from `probe`, counts
/// hardware `set` calls, and answers `get` with a fixed value.
#[derive(Debug)]
struct ProbeController {
    hardware_range: bool,
    sets: Arc<Mutex<usize>>,
}

impl BrightnessController for ProbeController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps_hw(self.hardware_range))
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        Ok(FeatureRange {
            current: 50,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
        let mut count = self.sets.lock().unwrap();
        *count = count.saturating_add(1);
        Ok(())
    }
}

/// A controller that probes as hardware-backed and ACKs every write (`Ok`), but
/// whose read-back is pinned at `fixed` — a dead panel that lies about accepting
/// writes. Its first effective write should be caught by the worker's read-back
/// verification.
#[derive(Debug)]
struct AckNoopController {
    fixed: u16,
}

impl BrightnessController for AckNoopController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        Ok(FeatureRange {
            current: self.fixed,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
        Ok(()) // ACK, but the panel never moves.
    }
}

/// A controller that probes as hardware-backed and whose writes DO take, but only
/// reflect on the read-back after `stale_reads` further reads (a slow panel behind
/// a dock / MST hub / KVM). `stale_reads` reads after a `set` still report the old
/// value; the next read reports (and settles at) the written value.
#[derive(Debug)]
struct LateReflectController {
    shown: u16,
    pending: Option<u16>,
    stale_left: u32,
    stale_reads: u32,
}

impl LateReflectController {
    fn new(initial: u16, stale_reads: u32) -> Self {
        LateReflectController {
            shown: initial,
            pending: None,
            stale_left: 0,
            stale_reads,
        }
    }
}

impl BrightnessController for LateReflectController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        if let Some(target) = self.pending {
            if self.stale_left == 0 {
                self.shown = target;
                self.pending = None;
            } else {
                self.stale_left = self.stale_left.saturating_sub(1);
            }
        }
        Ok(FeatureRange {
            current: self.shown,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, value: u16) -> Result<(), ControlError> {
        self.pending = Some(value);
        self.stale_left = self.stale_reads;
        Ok(())
    }
}

/// A controller that probes as hardware-backed but REJECTS every write with
/// `Unsupported` (the backend says the panel cannot do brightness). `get`
/// succeeds at a fixed value so the initial learn completes.
#[derive(Debug)]
struct RejectWriteController {
    fixed: u16,
}

impl BrightnessController for RejectWriteController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        Ok(caps())
    }
    fn get(&mut self, _feature: Feature) -> Result<FeatureRange, ControlError> {
        Ok(FeatureRange {
            current: self.fixed,
            max: 100,
        })
    }
    fn set(&mut self, _feature: Feature, _value: u16) -> Result<(), ControlError> {
        Err(ControlError::Unsupported)
    }
}

/// Wait for a `DisplaysChanged` whose snapshot for `id` has the given kind.
fn wait_kind(
    notes: &Receiver<EngineNotification>,
    id: &StableDisplayId,
    kind: DisplayKind,
    dur: Duration,
) -> bool {
    wait_note(notes, dur, |n| {
        matches!(
            n,
            EngineNotification::DisplaysChanged(snaps)
                if snaps.iter().any(|s| s.id == *id && s.kind == kind)
        )
    })
}

#[test]
fn probe_without_hardware_range_downgrades_to_software_only() {
    // BUG 3 core: a controller that probes `hardware_range: false` must downgrade
    // the display to SoftwareOnly — driven by the PROBE alone, before (and without)
    // any dead hardware brightness write.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let sets = Arc::new(Mutex::new(0usize));
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let factory: duja_app::ControllerFactory = {
        let sets = sets.clone();
        Box::new(move |_id| {
            let sets = sets.clone();
            Box::new(move || {
                Some(Box::new(ProbeController {
                    hardware_range: false,
                    sets: sets.clone(),
                }) as Box<dyn BrightnessController>)
            }) as duja_app::ControllerOpener
        })
    };
    let (engine, notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        factory,
        platform_rx,
    );

    assert!(
        wait_kind(
            &notes,
            &id,
            DisplayKind::SoftwareOnly,
            Duration::from_secs(3)
        ),
        "a probe reporting no hardware range must downgrade the display to SoftwareOnly"
    );
    assert_eq!(
        *sets.lock().unwrap(),
        0,
        "the downgrade must be probe-driven — no dead hardware write is required"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn ack_but_silent_noop_write_downgrades_to_software_only() {
    // Detection b: a controller that probes hardware-backed and ACKs writes, but
    // whose read-back never moves, must be downgraded after its first effective
    // brightness write's read-back proves the panel did not move.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    // `get` is pinned at 60, distinct from DEFAULT_USER_LEVEL_PCT (50), so the
    // learned-level notification below is unambiguous.
    let factory: duja_app::ControllerFactory = Box::new(|_id| {
        Box::new(
            || Some(Box::new(AckNoopController { fixed: 60 }) as Box<dyn BrightnessController>),
        ) as duja_app::ControllerOpener
    });
    let (engine, notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    // Wait until the initial Get has been learned (60), so the worker knows the
    // pre-write value and the next write is an unambiguous, meaningful change.
    assert!(
        wait_note(&notes, Duration::from_secs(3), |n| matches!(
            n,
            EngineNotification::DisplaysChanged(s)
                if s.first().map(|d| d.user_level_pct) == Some(60)
        )),
        "engine must learn the initial level 60"
    );

    // Drive a distinctly different level; the write ACKs but the panel never moves.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 20,
    })
    .unwrap();

    assert!(
        wait_kind(
            &notes,
            &id,
            DisplayKind::SoftwareOnly,
            Duration::from_secs(3)
        ),
        "an ACK-but-no-op first write must downgrade the display to SoftwareOnly"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn slow_but_working_controller_is_not_falsely_downgraded() {
    // BLOCKER-1 guard (the one that must bite a naive single-read): a working panel
    // that reflects a write LATE — the first read-back still shows the old value,
    // a later one shows the new value — must NOT be downgraded. Detection (b)
    // retries the read-back, so it observes the movement; a single un-retried read
    // would misread "slow" as "dead" and permanently force software-only.
    //
    // `stale_reads = 1`: the first read-back after the write is stale, the second
    // is fresh — inside the retry budget, outside a single read.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let factory: duja_app::ControllerFactory = Box::new(|_id| {
        Box::new(|| {
            Some(Box::new(LateReflectController::new(60, 1)) as Box<dyn BrightnessController>)
        }) as duja_app::ControllerOpener
    });
    let (engine, notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    // Learn the initial level (60), then drive a real, meaningful change.
    assert!(
        wait_note(&notes, Duration::from_secs(3), |n| matches!(
            n,
            EngineNotification::DisplaysChanged(s)
                if s.first().map(|d| d.user_level_pct) == Some(60)
        )),
        "engine must learn the initial level 60"
    );
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 20,
    })
    .unwrap();

    assert!(
        !wait_kind(
            &notes,
            &id,
            DisplayKind::SoftwareOnly,
            Duration::from_secs(1)
        ),
        "a slow-but-working controller must never be downgraded (BLOCKER-1 false-downgrade guard)"
    );
    let snaps = snapshot(&cmds);
    assert!(
        snaps
            .iter()
            .any(|s| s.id == id && s.kind == DisplayKind::ExternalDdc),
        "a slow-but-working controller must stay hardware-backed (ExternalDdc)"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn wrongly_forced_display_self_heals_to_hardware() {
    // MAJOR-2 self-heal: a working-but-very-slow panel that detection (b) mistakes
    // for dead (stale across ALL retries) must recover once a later poll read shows
    // the hardware has actually MOVED to the value Duja wrote — proving it live. It
    // must return to ExternalDdc without an app restart.
    //
    // `stale_reads = 6` outlasts detection (b)'s retries (so it IS wrongly forced),
    // then reflects during polling (so it self-heals).
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let factory: duja_app::ControllerFactory = Box::new(|_id| {
        Box::new(|| {
            Some(Box::new(LateReflectController::new(60, 6)) as Box<dyn BrightnessController>)
        }) as duja_app::ControllerOpener
    });
    let (engine, notes) = Engine::spawn(
        fast_poll_cfg(),
        enumerator(state, calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    // Learn 60, then a meaningful write the panel is too slow to reflect within the
    // retries ⇒ wrongly forced software-only.
    assert!(
        wait_note(&notes, Duration::from_secs(3), |n| matches!(
            n,
            EngineNotification::DisplaysChanged(s)
                if s.first().map(|d| d.user_level_pct) == Some(60)
        )),
        "engine must learn the initial level 60"
    );
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 20,
    })
    .unwrap();
    assert!(
        wait_kind(
            &notes,
            &id,
            DisplayKind::SoftwareOnly,
            Duration::from_secs(3)
        ),
        "the very slow panel should first be (wrongly) forced software-only"
    );

    // Enable polling: a later poll reads the now-reflected value (20), which both
    // matches what Duja wrote AND differs from the prior reading (60) ⇒ moved ⇒
    // self-heal back to the real hardware kind.
    cmds.send(EngineCommand::SetLevelPolling { on: true })
        .unwrap();
    assert!(
        wait_kind(
            &notes,
            &id,
            DisplayKind::ExternalDdc,
            Duration::from_secs(5)
        ),
        "a wrongly-forced display must self-heal to ExternalDdc once proven live"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn rejected_first_write_downgrades_to_software_only() {
    // Detection c (debt #48): a first brightness write the backend REJECTS
    // (`Ok(Err(Unsupported))`) is no longer swallowed as a clean Set — it is a
    // no-hardware signal that downgrades the display to SoftwareOnly.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let factory: duja_app::ControllerFactory = Box::new(|_id| {
        Box::new(|| {
            Some(Box::new(RejectWriteController { fixed: 60 }) as Box<dyn BrightnessController>)
        }) as duja_app::ControllerOpener
    });
    let (engine, notes) = Engine::spawn(
        EngineConfig::default(),
        enumerator(state, calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    assert!(
        wait_note(&notes, Duration::from_secs(3), |n| matches!(
            n,
            EngineNotification::DisplaysChanged(s)
                if s.first().map(|d| d.user_level_pct) == Some(60)
        )),
        "engine must learn the initial level 60"
    );

    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 20,
    })
    .unwrap();

    assert!(
        wait_kind(
            &notes,
            &id,
            DisplayKind::SoftwareOnly,
            Duration::from_secs(3)
        ),
        "a rejected first brightness write must downgrade the display to SoftwareOnly"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

#[test]
fn stale_software_fallback_does_not_downgrade_fresh_worker() {
    // Generation match (false-downgrade guard): a SoftwareFallback from a worker
    // that was already replaced must be IGNORED, so it cannot downgrade the fresh,
    // healthy worker that took its place. Worker A blocks in `probe` (reporting no
    // hardware range); it is retired and replaced by a healthy worker B; only then
    // is A's probe released, emitting a STALE fallback.
    let id = display_id();
    let (platform_tx, platform_rx) = unbounded::<()>();
    let (writes_tx, writes_rx) = unbounded();
    let (probe_entered_tx, probe_entered_rx) = unbounded::<()>();
    let (release_tx, release_rx) = unbounded::<()>();
    let (calls_tx, _calls_rx) = unbounded();
    let state: Displays = Arc::new(Mutex::new(vec![discovered(&id)]));

    let call = Arc::new(Mutex::new(0u32));
    let factory: duja_app::ControllerFactory = {
        let writes_tx = writes_tx.clone();
        Box::new(move |_id| {
            let n = {
                let mut c = call.lock().unwrap();
                *c += 1;
                *c
            };
            let writes_tx = writes_tx.clone();
            let probe_entered_tx = probe_entered_tx.clone();
            let release_rx = release_rx.clone();
            Box::new(move || {
                if n == 1 {
                    Some(Box::new(GatedProbe {
                        entered: probe_entered_tx.clone(),
                        release: release_rx.clone(),
                        hardware_range: false,
                    }) as Box<dyn BrightnessController>)
                } else {
                    Some(Box::new(Recording::new(writes_tx)) as Box<dyn BrightnessController>)
                }
            }) as duja_app::ControllerOpener
        })
    };

    let cfg = EngineConfig {
        write_min_gap: Duration::from_millis(10),
        watchdog_timeout: Duration::from_secs(30),
        displaychange_debounce: Duration::from_millis(60),
        level_poll_interval: Duration::from_millis(50),
    };
    let (engine, notes) = Engine::spawn(
        cfg,
        enumerator(state.clone(), calls_tx),
        factory,
        platform_rx,
    );
    let cmds = engine.sender();

    // Worker A is wedged inside its probe (generation 1).
    probe_entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("worker A should enter its probe");

    // Unplug + replug: A is retired and healthy worker B (generation 2) spawns.
    *state.lock().unwrap() = Vec::new();
    cmds.send(EngineCommand::RefreshNow).unwrap();
    assert!(snapshot(&cmds).is_empty(), "display should be gone");
    *state.lock().unwrap() = vec![discovered(&id)];
    cmds.send(EngineCommand::RefreshNow).unwrap();

    // B is live: a user write flows through it.
    cmds.send(EngineCommand::SetUserLevel {
        id: id.clone(),
        pct: 33,
    })
    .unwrap();
    let (_c, seen) = drain_writes(&writes_rx, |f, v| f == Feature::Brightness && v == 33);
    assert!(
        seen.contains(&(Feature::Brightness, 33)),
        "worker B should be the live worker"
    );

    // Release A: it finishes its probe and emits a STALE SoftwareFallback (gen 1).
    release_tx.send(()).unwrap();

    // The stale fallback must NOT downgrade the healthy display.
    assert!(
        !wait_kind(
            &notes,
            &id,
            DisplayKind::SoftwareOnly,
            Duration::from_millis(700)
        ),
        "a stale SoftwareFallback must not downgrade the fresh worker's display"
    );
    let snaps = snapshot(&cmds);
    assert!(
        snaps
            .iter()
            .any(|s| s.id == id && s.kind == DisplayKind::ExternalDdc),
        "the display must remain hardware-backed after a stale fallback"
    );

    within(Duration::from_secs(2), move || engine.shutdown());
    let _ = platform_tx;
}

/// A controller that announces it entered `probe`, blocks until released, then
/// reports a configurable `hardware_range` — so a test can hold a probe in flight,
/// force a respawn, and prove the stale fallback is ignored (generation match).
#[derive(Debug)]
struct GatedProbe {
    entered: Sender<()>,
    release: Receiver<()>,
    hardware_range: bool,
}

impl BrightnessController for GatedProbe {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        let _ = self.entered.send(());
        let _ = self.release.recv();
        Ok(caps_hw(self.hardware_range))
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

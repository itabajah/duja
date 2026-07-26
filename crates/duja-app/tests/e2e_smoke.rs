//! Headless end-to-end smoke test: the **assembly seam** the per-crate unit
//! tests structurally cannot see.
//!
//! Every layer here is the real one except the hardware. The test stands up the
//! same pipeline `duja --headless` assembles — engine actor + per-monitor
//! workers + [`HeadlessBridge`] + the OS IPC transport — against a fake backend
//! (an injected enumerator and controller factory, so it runs on a CI runner
//! with no displays), then drives `list` / `set` / `get` / `show-flyout` over a
//! real [`PipeClient`] and asserts that the request reached the hardware and the
//! resulting state came back.
//!
//! What it catches that unit tests do not: a bridge wired to the wrong channel,
//! a request→command mapping that no longer reaches a worker, a snapshot that
//! stops reflecting a write, a transport that starts but never answers, or a
//! teardown that hangs.
//!
//! Runs on **Windows** (named pipe) and **unix** (domain socket) — the two
//! transports are separate implementations, so both lanes are worth the run. On
//! any other target the transport is a no-op stub and the test compiles out.
//!
//! All synchronization is via channels and generous `recv_timeout` deadlines —
//! never bare sleeps for correctness — and every join is timeout-guarded, so a
//! regression surfaces as a failure rather than a hung CI job (the same rule
//! `engine.rs` states).
#![cfg(any(windows, unix))]
// RATIONALE: integration tests are a separate crate and do not inherit the
// library's `cfg(test)` lint allows. These tests use unwrap/expect for brevity;
// they never ship.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use duja_app::ipc::{HeadlessBridge, handle_request};
use duja_app::{Engine, EngineConfig, EngineNotification, Enumeration};
use duja_core::controller::{BrightnessController, ControlError};
use duja_core::id::StableDisplayId;
use duja_core::manager::DiscoveredDisplay;
use duja_core::model::{Capabilities, DisplayKind, Feature, FeatureRange};
use duja_core::testing::controller::FakeController;
use duja_ipc::{Request, Response};
use duja_platform::PipeServer;
use duja_platform::ipc::PipeClient;

// --- deadlines ------------------------------------------------------------

/// How long any single wait (a notification, a hardware write, a teardown) may
/// take before the test calls it a regression. Deliberately generous: a loaded
/// CI runner is slow, and every wait here is channel-driven, so a healthy run
/// never approaches it.
const DEADLINE: Duration = Duration::from_secs(10);

/// How long a client may spend reaching the server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

// --- the fake backend -----------------------------------------------------

/// A stable id per fake panel (`serial` distinguishes them).
fn display_id(serial: &str) -> StableDisplayId {
    StableDisplayId::from_parts("DUJ", 0x0E2E, Some(serial)).unwrap()
}

fn caps() -> Capabilities {
    Capabilities {
        features: [Feature::Brightness].into_iter().collect(),
        hardware_range: true,
        raw_capabilities: None,
        allowed_inputs: Vec::new(),
    }
}

fn discovered(serial: &str) -> DiscoveredDisplay {
    DiscoveredDisplay {
        id: display_id(serial),
        kind: DisplayKind::ExternalDdc,
        name: Some(format!("Fake {serial}")),
        capabilities: caps(),
    }
}

/// One fake display's controller: the shared core [`FakeController`] as the
/// hardware model (so a `get` reflects the last `set`), plus a channel report of
/// every completed write — the test's synchronization point for "the request
/// reached the hardware".
#[derive(Debug)]
struct SmokeController {
    id: StableDisplayId,
    inner: FakeController,
    writes: Sender<(StableDisplayId, Feature, u16)>,
}

impl BrightnessController for SmokeController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        self.inner.probe()
    }

    fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError> {
        self.inner.get(feature)
    }

    fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
        let outcome = self.inner.set(feature, value);
        if outcome.is_ok() {
            let _ = self.writes.send((self.id.clone(), feature, value));
        }
        outcome
    }
}

// --- the transport endpoint ----------------------------------------------

/// A unique endpoint per test so parallel tests (and a Duja running on the dev
/// box) never collide.
#[cfg(windows)]
fn endpoint(tag: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(r"\\.\pipe\duja-e2e-{}-{tag}-{n}", std::process::id())
}

/// The unix twin: a socket under one per-process directory the server creates
/// and `chmod 0700`s.
#[cfg(unix)]
fn endpoint(tag: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("/tmp/duja-e2e-{}/{tag}-{n}.sock", std::process::id())
}

// --- the harness ----------------------------------------------------------

/// The assembled headless pipeline: fake backend → engine → bridge → transport.
struct Smoke {
    endpoint: String,
    engine: Option<Engine>,
    server: Option<PipeServer>,
    notifications: Receiver<EngineNotification>,
    writes: Receiver<(StableDisplayId, Feature, u16)>,
    /// Kept alive so the engine's platform-tick channel never disconnects (the
    /// real pipeline holds the pump's sender for the engine's lifetime).
    _ticks: Sender<()>,
}

impl Smoke {
    /// Assemble the pipeline over `serials`, exactly as `run::headless` does but
    /// with injected fakes in place of the DDC/WMI backends.
    fn start(tag: &str, serials: &[&'static str]) -> Self {
        let (write_tx, writes) = unbounded();
        let (ticks, tick_rx) = unbounded::<()>();

        let table: Vec<DiscoveredDisplay> = serials.iter().map(|s| discovered(s)).collect();
        let enumerator = {
            let table = table.clone();
            Box::new(move || Enumeration {
                displays: table.clone(),
            }) as duja_app::Enumerator
        };
        let factory = {
            let write_tx = write_tx.clone();
            Box::new(move |id: &StableDisplayId| {
                let id = id.clone();
                let write_tx = write_tx.clone();
                Box::new(move || {
                    Some(Box::new(SmokeController {
                        id,
                        inner: FakeController::with_capabilities(caps()),
                        writes: write_tx,
                    }) as Box<dyn BrightnessController>)
                }) as duja_app::ControllerOpener
            }) as duja_app::ControllerFactory
        };

        let (engine, notifications) =
            Engine::spawn(EngineConfig::default(), enumerator, factory, tick_rx);

        // The real bridge and the real request mapping, over the real transport
        // — the exact closure `bin_support::ipc::start` builds, on a test-only
        // endpoint instead of the process-wide default name.
        let bridge: std::sync::Arc<dyn duja_app::ipc::IpcBridge> =
            std::sync::Arc::new(HeadlessBridge::new(engine.sender()));
        let endpoint = endpoint(tag);
        let server = PipeServer::serve_named(&endpoint, move |request| {
            handle_request(bridge.as_ref(), request)
        })
        .expect("the IPC server must start on a fresh endpoint");

        Smoke {
            endpoint,
            engine: Some(engine),
            server: Some(server),
            notifications,
            writes,
            _ticks: ticks,
        }
    }

    /// One request/response exchange over a fresh client connection (how
    /// `dujactl` talks to a running Duja).
    fn ask(&self, request: &Request) -> Response {
        let mut client = PipeClient::connect_named(&self.endpoint, CONNECT_TIMEOUT)
            .expect("a running IPC server must accept a client");
        client.request(request).expect("the exchange must complete")
    }

    /// Wait up to [`DEADLINE`] for a notification satisfying `pred`.
    fn wait_note(&self, pred: impl Fn(&EngineNotification) -> bool) -> bool {
        let deadline = Instant::now().checked_add(DEADLINE).unwrap();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            match self
                .notifications
                .recv_timeout(deadline.saturating_duration_since(now))
            {
                Ok(note) => {
                    if pred(&note) {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
    }

    /// Drain hardware writes up to [`DEADLINE`], returning whether one matching
    /// `pred` arrived (the initial probe/learn traffic is skipped past).
    fn wait_write(&self, pred: impl Fn(&StableDisplayId, Feature, u16) -> bool) -> bool {
        let deadline = Instant::now().checked_add(DEADLINE).unwrap();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            match self
                .writes
                .recv_timeout(deadline.saturating_duration_since(now))
            {
                Ok((id, feature, value)) => {
                    if pred(&id, feature, value) {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
    }

    /// Tear the pipeline down, asserting it completes inside [`DEADLINE`] — a
    /// shutdown regression must surface as a failure, not a hung CI job.
    fn shutdown(mut self) {
        let server = self.server.take();
        let engine = self.engine.take();
        let (done_tx, done_rx) = unbounded();
        thread::spawn(move || {
            if let Some(server) = server {
                server.shutdown();
            }
            if let Some(engine) = engine {
                engine.shutdown();
            }
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(DEADLINE).is_ok(),
            "the headless pipeline must shut down cleanly within {DEADLINE:?}"
        );
    }
}

// --- the smoke ------------------------------------------------------------

#[test]
fn headless_pipeline_serves_list_set_and_get_over_the_real_transport() {
    let smoke = Smoke::start("main", &["A", "B"]);
    let a = display_id("A");

    // 1. The pipeline came up: the first enumeration reached the notification
    //    channel with both fake panels.
    assert!(
        smoke.wait_note(|note| match note {
            EngineNotification::DisplaysChanged(snaps) => snaps.len() == 2,
            _ => false,
        }),
        "the engine must publish its first enumeration"
    );

    // 2. `list` over the wire projects both displays.
    match smoke.ask(&Request::ListDisplays) {
        Response::Displays { displays } => {
            assert_eq!(displays.len(), 2, "both fake panels must be listed");
            assert!(
                displays.iter().any(|d| d.id == a.as_str()),
                "the listing must carry the resolved stable ids"
            );
        }
        other => panic!("expected Displays, got {other:?}"),
    }

    // 3. `set` over the wire is acked AND reaches the fake hardware. This is the
    //    whole seam: transport → bridge → engine command → worker → controller.
    assert_eq!(
        smoke.ask(&Request::SetBrightness {
            id: a.as_str().to_owned(),
            pct: 37,
        }),
        Response::Ok
    );
    assert!(
        smoke.wait_write(|id, feature, value| {
            *id == a && feature == Feature::Brightness && value == 37
        }),
        "the IPC set must land as a hardware write of 37 on the addressed display"
    );

    // 4. `get` reflects it. Ordered behind the set on the engine's own command
    //    channel, so this is deterministic rather than timing-dependent.
    assert_eq!(
        smoke.ask(&Request::GetBrightness {
            id: a.as_str().to_owned(),
        }),
        Response::Brightness {
            id: a.as_str().to_owned(),
            pct: 37,
        }
    );

    // 5. `show-flyout` is the documented headless no-op, still answered Ok.
    assert_eq!(smoke.ask(&Request::ShowFlyout), Response::Ok);

    smoke.shutdown();
}

#[test]
fn unknown_display_is_refused_end_to_end() {
    // The error path across the same seam: a request naming a display the engine
    // does not know is refused with the stable code, not silently accepted.
    let smoke = Smoke::start("unknown", &["A"]);
    assert!(
        smoke.wait_note(
            |note| matches!(note, EngineNotification::DisplaysChanged(s) if !s.is_empty())
        ),
        "the engine must publish its first enumeration"
    );

    let response = smoke.ask(&Request::SetBrightness {
        id: "DUJ-0E2E-nope".to_owned(),
        pct: 20,
    });
    assert!(
        matches!(response, Response::Error { ref code, .. } if code == "unknown_display"),
        "expected an unknown_display error, got {response:?}"
    );

    smoke.shutdown();
}

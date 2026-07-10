//! Duja's concurrency engine: the controller actor, per-monitor worker
//! threads, write coalescing, and the stuck-driver watchdog (ADR-0005).
//!
//! This crate is **pure cross-platform std-Rust** — no OS APIs, no `unsafe`.
//! It turns the pure decisions of [`duja_core::manager::DisplayManager`] and
//! the pure timing of [`duja_core::debounce`] into a running actor system:
//!
//! - **[`Engine`]** — one thread owning the
//!   [`DisplayManager`](duja_core::manager::DisplayManager) and all policy
//!   state. It runs a single `crossbeam_channel` select loop over a command
//!   channel, a raw platform-event channel (debounced with core's
//!   [`Debouncer`](duja_core::debounce::Debouncer)), worker acks, and
//!   shutdown. The idle engine parks with **zero wakeups**: the select
//!   deadline is armed only while a debounce or a watchdog timer is pending.
//! - **Per-monitor workers** — each exclusively owns its
//!   [`BrightnessController`]
//!   (moved in at spawn). A worker drains newest-per-feature (latest-wins
//!   coalescing; distinct features never cross-coalesce), performs the
//!   blocking write under the engine's min-gap policy, and acks the engine.
//! - **Watchdog** — the engine stamps every dispatched write; an unacked write
//!   older than [`EngineConfig::watchdog_timeout`] marks the display
//!   unresponsive and **leaks** the stuck thread (never joins a thread stuck
//!   in a GPU driver). A later enumeration that sights the display spawns a
//!   fresh worker with a freshly-opened controller.
//! - **Supervision** — worker operations run under
//!   [`std::panic::catch_unwind`]; a worker panic becomes a failed ack that
//!   marks the display unresponsive, and never takes down the engine. The
//!   engine thread body is itself `catch_unwind`-wrapped.
//!
//! # Injection seams
//!
//! The engine talks to the outside world through two injected callbacks so it
//! stays pure and fully testable:
//!
//! - an [`Enumerator`] — produces one [`Enumeration`] (plain
//!   [`DiscoveredDisplay`] data) per
//!   pass; integration builds this from `duja-ddc::enumerate()` +
//!   `duja-panel::enumerate()`.
//! - a [`ControllerFactory`] — (re)opens a
//!   [`BrightnessController`] for a
//!   display id; tests inject fakes, integration injects the real backends.
//!
//! # Example
//!
//! ```no_run
//! use std::time::Duration;
//! use duja_app::{Engine, EngineConfig, EngineCommand, Enumeration};
//!
//! let (platform_tx, platform_rx) = crossbeam_channel::unbounded::<()>();
//! let (engine, _notifications) = Engine::spawn(
//!     EngineConfig::default(),
//!     Box::new(|| Enumeration { displays: Vec::new() }),
//!     Box::new(|_id| Box::new(|| None)),
//!     platform_rx,
//! );
//! let commands = engine.sender();
//! let _ = commands.send(EngineCommand::RefreshNow);
//! engine.shutdown();
//! # let _ = platform_tx;
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// RATIONALE: mirrors the workspace policy (see duja-core/src/lib.rs) — tests
// may unwrap/expect/panic; production code may not.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use duja_core::controller::BrightnessController;
use duja_core::id::StableDisplayId;
use duja_core::manager::DiscoveredDisplay;
use duja_core::model::DisplaySnapshot;

mod engine;
mod protocol;
mod worker;

/// Tuning knobs for an [`Engine`], all durations.
///
/// Defaults match the plan / ADR-0005: a 100 ms minimum gap between hardware
/// writes to one display, a 5 s stuck-driver watchdog, and a 750 ms
/// trailing-edge debounce for display-change storms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    /// Minimum wall-clock gap the engine enforces between successive hardware
    /// writes to a single display (latest-wins during the gap).
    pub write_min_gap: Duration,
    /// How long a dispatched write may go unacked before the engine declares
    /// the display unresponsive and leaks the stuck worker thread.
    pub watchdog_timeout: Duration,
    /// Trailing-edge quiet period applied to raw platform display-change ticks
    /// before an enumeration is run.
    pub displaychange_debounce: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            write_min_gap: Duration::from_millis(100),
            watchdog_timeout: Duration::from_secs(5),
            displaychange_debounce: Duration::from_millis(750),
        }
    }
}

/// One enumeration pass, as plain data.
///
/// Integration builds this from the platform backends; the engine treats it as
/// opaque input to [`DisplayManager::apply_enumeration`](duja_core::manager::DisplayManager::apply_enumeration).
#[derive(Debug, Clone, Default)]
pub struct Enumeration {
    /// The displays sighted in this pass, in connector order.
    pub displays: Vec<DiscoveredDisplay>,
}

/// A pluggable enumeration source, invoked on the engine thread once per
/// (debounced) refresh. `FnMut` so a real enumerator may cache handles.
pub type Enumerator = Box<dyn FnMut() -> Enumeration + Send>;

/// A deferred controller open, run **on the worker thread** the controller will
/// live on.
///
/// Returns `None` when the controller cannot be opened (e.g. the display
/// vanished between enumeration and open, or a probe failed); the worker then
/// reports the failure and exits, leaving the display worker-less until the
/// next sighting.
pub type ControllerOpener = Box<dyn FnOnce() -> Option<Box<dyn BrightnessController>> + Send>;

/// How the engine (re)opens a controller for a display.
///
/// The factory runs on the **engine** thread and returns a [`ControllerOpener`]
/// that the worker invokes as the very first thing on its **own** thread. This
/// guarantees every controller — and any thread-affine resource it acquires
/// (e.g. a COM apartment, a physical-monitor handle) — is constructed, used,
/// and dropped on one and the same thread. Injected so tests use fakes and
/// integration uses `duja-ddc` / `duja-panel`.
pub type ControllerFactory = Box<dyn Fn(&StableDisplayId) -> ControllerOpener + Send>;

/// A message to the engine actor.
#[derive(Debug)]
pub enum EngineCommand {
    /// Set the unified user brightness level (0–100, clamped) for a display.
    ///
    /// The level is recorded for hot-plug restore and, if the display is
    /// connected and responsive, dispatched to its worker (scaled onto the
    /// probed brightness range).
    SetUserLevel {
        /// The target display.
        id: StableDisplayId,
        /// Desired level in percent (values above 100 are clamped).
        pct: u8,
    },
    /// Switch a display's active input source (VCP `0x60`).
    ///
    /// `value` is a raw MCCS input code. The engine rejects (drops with a
    /// warning) any code not in the display's probed
    /// [`allowed_inputs`](duja_core::model::Capabilities::allowed_inputs); an
    /// accepted code is dispatched to the worker as a
    /// [`Feature::InputSource`](duja_core::model::Feature::InputSource) write
    /// (never verified by readback — ADR-0002).
    SetInput {
        /// The target display.
        id: StableDisplayId,
        /// The raw MCCS input-source code to select.
        value: u8,
    },
    /// Run one enumeration pass immediately, bypassing the debounce.
    RefreshNow,
    /// Request the current UI-facing snapshots, delivered on `reply`.
    Snapshot {
        /// Where the engine sends the snapshot vector.
        reply: Sender<Vec<DisplaySnapshot>>,
    },
    /// Drain workers (bounded), join non-leaked threads, and stop the engine.
    Shutdown,
}

/// An event the engine publishes to its subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineNotification {
    /// The set of connected displays, or one of their levels, changed. Carries
    /// the fresh snapshot list.
    DisplaysChanged(Vec<DisplaySnapshot>),
    /// The named display was marked unresponsive (watchdog or worker panic).
    DisplayUnresponsive(StableDisplayId),
    /// A previously-unresponsive display was sighted again and re-armed.
    DisplayResponsive(StableDisplayId),
}

/// A handle to a running [`Engine`] actor: a command sender plus the join
/// handle for its thread.
///
/// Dropping the handle shuts the engine down gracefully (same as
/// [`shutdown`](Engine::shutdown)); shutdown is idempotent.
#[derive(Debug)]
pub struct Engine {
    cmd_tx: Sender<EngineCommand>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    /// Spawn the engine actor thread and run its first enumeration immediately.
    ///
    /// `platform_events` carries raw display-change ticks (`()`); the engine
    /// debounces them internally. Returns the [`Engine`] handle and the
    /// receiver on which the engine publishes [`EngineNotification`]s.
    #[must_use]
    pub fn spawn(
        cfg: EngineConfig,
        enumerator: Enumerator,
        factory: ControllerFactory,
        platform_events: Receiver<()>,
    ) -> (Engine, Receiver<EngineNotification>) {
        let (cmd_tx, notif_rx, join) =
            engine::EngineState::launch(cfg, enumerator, factory, platform_events);
        (
            Engine {
                cmd_tx,
                join: Some(join),
            },
            notif_rx,
        )
    }

    /// A fresh sender for issuing [`EngineCommand`]s (cloneable, `Send`).
    #[must_use]
    pub fn sender(&self) -> Sender<EngineCommand> {
        self.cmd_tx.clone()
    }

    /// Shut the engine down and join its thread (leaked workers excepted).
    ///
    /// Idempotent and also run on [`Drop`]; safe to call exactly once here.
    pub fn shutdown(mut self) {
        self.stop();
    }

    /// Send `Shutdown` and join the engine thread, at most once.
    fn stop(&mut self) {
        if let Some(join) = self.join.take() {
            // Best-effort: if the receiver is already gone the engine has
            // stopped, and a failed join simply means it already returned.
            let _ = self.cmd_tx.send(EngineCommand::Shutdown);
            let _ = join.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

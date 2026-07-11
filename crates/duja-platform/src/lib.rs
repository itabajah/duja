//! OS integration: display reconfiguration events, power/session events,
//! single-instance enforcement, autostart registration.
//!
//! # Event pump
//!
//! [`EventPump`] owns a dedicated OS-event thread and translates raw platform
//! notifications into a small, cross-platform vocabulary of [`PlatformEvent`]s
//! delivered over a [`crossbeam_channel::Receiver`]. The pump performs **no
//! debouncing**: display-change notifications arrive in bursts and the consumer
//! (the controller, via `duja_core`'s pure `Debouncer`) collapses them.
//!
//! Event sources (P3+):
//! - **Windows** — a hidden **top-level** window (message-only `HWND`s do *not*
//!   receive `WM_DISPLAYCHANGE`, which is the whole reason for a real window) +
//!   `RegisterDeviceNotification` for the monitor device interface +
//!   `WM_POWERBROADCAST` + `WTSRegisterSessionNotification`.
//! - **macOS** (P6) — a dedicated `CFRunLoop` thread with
//!   `CGDisplayRegisterReconfigurationCallback` (display topology) and `IOKit`
//!   `IORegisterForSystemPower` (suspend/resume). See the `mac` module.
//! - **Linux** (P7) — udev `drm` monitor.
//!
//! On the remaining non-Windows, non-macOS targets the pump is still a no-op:
//! [`spawn`](EventPump::spawn) succeeds and returns a receiver that stays open
//! but never yields an event (so `recv_timeout` blocks and times out rather than
//! reporting a disconnect). The real Linux backend replaces it in P7.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use crossbeam_channel::Receiver;

#[cfg(windows)]
mod win;

#[cfg(target_os = "macos")]
mod mac;

// Pure raw-code → `PlatformEvent` mapping for the macOS backend. Compiled on
// macOS (where `mac::sys` calls it) and, under `cfg(test)`, on every host so its
// logic is unit-tested cross-platform without macOS hardware.
#[cfg(any(test, target_os = "macos"))]
mod mac_events;

pub mod autostart;
pub mod ipc;
mod single_instance;

pub use autostart::{Autostart, AutostartError};
pub use ipc::{IpcTransportError, PipeClient, PipeServer};
pub use single_instance::SingleInstance;

/// A normalized OS event relevant to display management.
///
/// The vocabulary is deliberately platform-agnostic; each backend maps its raw
/// notifications onto these variants. Events are *not* debounced by the pump —
/// `DisplaysChanged` in particular is bursty and the consumer coalesces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlatformEvent {
    /// Display topology may have changed (`WM_DISPLAYCHANGE`, or a monitor
    /// device arrival/removal). Bursty; the consumer debounces before
    /// re-enumerating.
    DisplaysChanged,
    /// The system is about to suspend. Persist/park state now.
    Suspending,
    /// The system resumed from suspend; re-apply brightness and overlays.
    Resumed,
    /// The interactive session was unlocked; re-apply state that the lock
    /// screen or another session may have disturbed.
    SessionUnlocked,
}

/// An error raised while starting or running the platform event pump.
///
/// Variants carry a human-readable description of the underlying OS failure
/// rather than a platform-specific error type, so the public surface stays
/// identical on every target.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The event thread could not be started (OS refused to spawn a thread).
    #[error("failed to start the platform event thread: {0}")]
    ThreadSpawn(String),
    /// The event thread started but failed to initialize its OS resources
    /// (window class, hidden window, or notification registration).
    #[error("failed to initialize the platform event source: {0}")]
    Init(String),
}

/// A running platform event pump.
///
/// Construct one with [`EventPump::spawn`]; it owns an OS-event thread that
/// lives until [`shutdown`](EventPump::shutdown) is called or the handle is
/// dropped. Shutdown destroys the OS window, unregisters every notification,
/// and joins the thread; it is idempotent and also runs on `Drop`.
pub struct EventPump {
    backend: Backend,
}

impl EventPump {
    /// Spawn the platform event thread.
    ///
    /// Returns the pump handle and the receiver on which [`PlatformEvent`]s
    /// arrive. On Windows the call blocks until the event thread has created
    /// its window and registered every notification, so any initialization
    /// failure surfaces here as [`PlatformError`] (the thread is joined before
    /// returning). On other targets it always succeeds with a silent receiver.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::ThreadSpawn`] if the OS refuses to start the
    /// thread, or [`PlatformError::Init`] if window creation or notification
    /// registration fails.
    pub fn spawn() -> Result<(EventPump, Receiver<PlatformEvent>), PlatformError> {
        let (backend, rx) = Backend::spawn()?;
        Ok((EventPump { backend }, rx))
    }

    /// Stop the pump: destroy the OS window, unregister notifications, and join
    /// the event thread.
    ///
    /// This consumes the handle. It is equivalent to dropping it, but makes the
    /// teardown point explicit and deterministic. Dropping without calling this
    /// performs the same work (see the [`Drop`] impl on the backend).
    pub fn shutdown(self) {
        let mut this = self;
        // Deterministic teardown now; the backend's own `Drop` is the idempotent
        // safety net if this handle is instead just dropped.
        this.backend.shutdown();
    }
}

// -- Windows backend selection --------------------------------------------

#[cfg(windows)]
type Backend = win::Pump;

// -- macOS backend selection ----------------------------------------------

#[cfg(target_os = "macos")]
type Backend = mac::Pump;

// -- Non-Windows, non-macOS no-op backend ---------------------------------

/// Placeholder backend for targets without a real event source yet (Linux, P7).
///
/// Holds the sending half of the channel open so the returned receiver blocks
/// (and times out) rather than reporting an immediate disconnect — matching the
/// "silent but live" contract documented on [`EventPump::spawn`].
#[cfg(not(any(windows, target_os = "macos")))]
struct Noop {
    _tx: crossbeam_channel::Sender<PlatformEvent>,
}

#[cfg(not(any(windows, target_os = "macos")))]
type Backend = Noop;

#[cfg(not(any(windows, target_os = "macos")))]
impl Noop {
    // RATIONALE (clippy::unnecessary_wraps): the backend `spawn` contract is
    // fallible on Windows; the no-op backend keeps the identical signature so
    // `EventPump::spawn` is platform-agnostic.
    #[allow(clippy::unnecessary_wraps)]
    fn spawn() -> Result<(Self, Receiver<PlatformEvent>), PlatformError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        Ok((Noop { _tx: tx }, rx))
    }

    /// No-op teardown; the channel closes when the backend drops.
    // RATIONALE (clippy::unused_self): mirrors the stateful Windows backend's
    // `shutdown(&mut self)` so `EventPump` calls it uniformly on every target.
    #[allow(clippy::unused_self)]
    fn shutdown(&mut self) {}
}

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_against_core() {
        assert_eq!(version(), duja_core::version());
    }

    #[test]
    fn platform_event_is_a_plain_value() {
        // The cross-platform vocabulary must derive the traits the rest of the
        // system relies on, on every target.
        let a = PlatformEvent::DisplaysChanged;
        let b = a;
        assert_eq!(a, b);
        assert_eq!(format!("{a:?}"), "DisplaysChanged");
    }

    #[test]
    fn error_is_display_and_debug() {
        let e = PlatformError::Init("boom".into());
        assert!(e.to_string().contains("boom"));
        assert!(format!("{e:?}").contains("Init"));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn noop_backend_spawns_and_shuts_down_without_firing() {
        use std::time::Duration;

        let (pump, rx) = EventPump::spawn().expect("noop spawn is infallible");
        // The receiver is live but silent: recv times out (would-block), it
        // does not report a disconnect.
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(50)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout)
        );
        pump.shutdown();
    }
}

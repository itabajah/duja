//! macOS backend for the platform event pump.
//!
//! A dedicated thread runs its own `CFRunLoop` and registers the OS
//! notification sources that feed [`PlatformEvent`]s, mirroring the Windows
//! backend's contract: [`spawn`](Pump::spawn) blocks until every source is live
//! (or reports [`PlatformError`]), teardown stops the loop and joins the thread,
//! and the pump performs no debouncing. See [`sys`] for the FFI and the pattern
//! that reaches the channel from the C callbacks.
//!
//! # Event sources
//!
//! - **`DisplaysChanged`** — `CGDisplayRegisterReconfigurationCallback`
//!   (CoreGraphics). The callback fires on the run loop of the registering
//!   thread, so we register on our dedicated `CFRunLoop` thread. It fires twice
//!   per reconfiguration (a `begin` pre-notification, then the real change); the
//!   pure mapping in [`mac_events`](crate::mac_events) drops the pre-notification
//!   and emits on the completed change. This source is **mandatory**: like the
//!   Windows window creation, a failure to register fails
//!   [`spawn`](Pump::spawn).
//! - **`Suspending` / `Resumed`** — `IOKit` `IORegisterForSystemPower`
//!   root-domain notifications (`kIOMessageSystemWillSleep` /
//!   `kIOMessageSystemHasPoweredOn`), delivered on the same run loop. This is
//!   chosen over `NSWorkspace.notificationCenter` deliberately: it is the
//!   classic daemon-grade API, needs no `AppKit`/main-thread run loop, and fits
//!   a dedicated background thread cleanly. It is **best-effort**: a host
//!   without system-power notifications yields no power events rather than
//!   failing initialization (graceful degradation). Sleep transitions are
//!   acknowledged with `IOAllowPowerChange` so the system does not stall waiting
//!   on us.
//! - **`SessionUnlocked`** — *not mapped.* The only signal for a screen unlock,
//!   `com.apple.screenIsUnlocked`, is a private distributed notification we will
//!   not depend on. macOS re-apply therefore relies on `Resumed` plus
//!   `DisplaysChanged`, which the consumer already tolerates (the pump is one of
//!   several event sources and a missing source is not an error).
//!
//! # Run loop liveness and shutdown
//!
//! A private no-op `CFRunLoopSource` is always added so `CFRunLoopRun` blocks
//! even when the power source is absent. Teardown calls `CFRunLoopStop` from the
//! owning handle (thread-safe), which returns the loop; the thread then removes
//! every source and callback before its state drops, and the handle joins it.
//! Idempotent, and also runs on `Drop`.

mod sys;

use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

use self::sys::RunLoopHandle;
use crate::{PlatformError, PlatformEvent};

/// A running macOS event pump: a retained handle to the thread's run loop (for
/// cross-thread `CFRunLoopStop`) plus the join handle of its thread.
pub struct Pump {
    run_loop: RunLoopHandle,
    join: Option<JoinHandle<()>>,
}

impl Pump {
    /// Spawn the event thread and block until it has registered every source
    /// (or failed to).
    pub fn spawn() -> Result<(Self, Receiver<PlatformEvent>), PlatformError> {
        let (tx, rx) = crossbeam_channel::unbounded::<PlatformEvent>();
        // One-shot init channel: the thread reports its run-loop handle, or the
        // error that stopped it, exactly once.
        let (init_tx, init_rx) =
            std::sync::mpsc::sync_channel::<Result<RunLoopHandle, PlatformError>>(1);

        let join = std::thread::Builder::new()
            .name("duja-platform-events".to_owned())
            .spawn(move || thread_main(tx, &init_tx))
            .map_err(|e| PlatformError::ThreadSpawn(e.to_string()))?;

        match init_rx.recv() {
            Ok(Ok(handle)) => Ok((
                Pump {
                    run_loop: handle,
                    join: Some(join),
                },
                rx,
            )),
            Ok(Err(e)) => {
                // Thread reported an init failure and is exiting; reap it.
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                // Thread died before reporting; reap it and surface a generic
                // init error.
                let _ = join.join();
                Err(PlatformError::Init(
                    "event thread exited before initialization".to_owned(),
                ))
            }
        }
    }

    /// Stop the run loop and join the thread. Idempotent: the join handle is
    /// taken on the first call, so later calls (including from `Drop`) are
    /// no-ops.
    pub(crate) fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            sys::stop(&self.run_loop);
            let _ = join.join();
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Body of the event thread: register sources, report readiness, pump the run
/// loop until stopped, then tear everything down.
fn thread_main(
    tx: Sender<PlatformEvent>,
    init_tx: &SyncSender<Result<RunLoopHandle, PlatformError>>,
) {
    // `run_pump` reports readiness through the closure; its `bool` return says
    // whether to enter the run loop (`false` if the spawner already vanished, so
    // the thread tears down instead of blocking forever).
    sys::run_pump(tx, |result| init_tx.send(result).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn pump_spawns_and_shuts_down_cleanly() {
        let (pump, rx) = Pump::spawn().expect("spawn on macOS");
        // A display reconfiguration cannot be scripted in CI, so the receiver is
        // live but (almost surely) silent: `recv` times out rather than reporting
        // a disconnect. A genuine spurious event is tolerated too.
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("pump channel disconnected while the pump was still alive");
            }
        }
        pump.shutdown_for_test();
    }

    #[test]
    fn second_spawn_after_teardown_succeeds() {
        let (pump, _rx) = Pump::spawn().expect("first spawn");
        drop(pump); // Drop stops the loop and joins.
        let (pump2, _rx2) = Pump::spawn().expect("second spawn after teardown");
        pump2.shutdown_for_test();
    }

    #[test]
    fn drop_shuts_down_without_hanging() {
        let (pump, _rx) = Pump::spawn().expect("spawn");
        // Dropping must not hang: stop-and-join completes promptly.
        drop(pump);
    }

    /// A stop requested *before* the run loop starts must still stop it.
    ///
    /// This is the deterministic form of the race that makes
    /// [`drop_shuts_down_without_hanging`] flaky on CI: `Pump::spawn` returns as
    /// soon as the pump thread pushes into a **buffered** `sync_channel(1)`,
    /// which happens at `sys.rs`'s `on_ready` call — *before* `CFRunLoopRun` is
    /// entered a few statements later. A `CFRunLoopStop` landing in that gap is
    /// silently dropped (it no-ops when the loop's `_currentMode` is `NULL`), and
    /// since the keep-alive source holds the loop open by construction, the loop
    /// then runs forever and the owner's `join` blocks forever.
    ///
    /// Rather than widen that window with a sleep and hope to lose the race, the
    /// test drives [`sys::run_pump`] directly and issues the stop from *inside*
    /// the readiness callback — the one moment guaranteed to precede
    /// `CFRunLoopRun`. That is the same code path `Pump::spawn`/`shutdown` take,
    /// with the interleaving pinned instead of sampled.
    ///
    /// It asserts by **timeout, not by hanging**: `run_pump` runs on its own
    /// thread and the assertion is that it returns. So this fails in bounded time
    /// rather than wedging the suite the way the flake does.
    #[test]
    fn stop_requested_before_the_run_loop_starts_is_not_lost() {
        let (tx, _rx) = crossbeam_channel::unbounded::<PlatformEvent>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

        std::thread::Builder::new()
            .name("pre-run-stop".to_owned())
            .spawn(move || {
                sys::run_pump(tx, |result| {
                    let handle = result.expect("sources registered");
                    // The race, pinned: the loop is not running yet.
                    sys::stop(&handle);
                    true
                });
                let _ = done_tx.send(());
            })
            .expect("spawn test pump thread");

        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "run_pump never returned: a stop requested before CFRunLoopRun was \
             entered got dropped, so the loop ran forever"
        );
    }

    impl Pump {
        /// Explicit teardown for tests (mirrors `EventPump::shutdown`).
        fn shutdown_for_test(mut self) {
            self.shutdown();
        }
    }
}

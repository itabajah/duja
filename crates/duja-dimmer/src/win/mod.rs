//! The Windows overlay backend.
//!
//! [`WindowsDimmer`] owns a dedicated thread that holds every overlay window and
//! runs their message loop. The pattern mirrors `duja-platform`'s event pump:
//! spawn → HWND-ready handshake over a one-shot channel → serve commands →
//! shutdown destroys the windows and joins ([`Drop`] is the safety net).
//!
//! [`apply`](WindowsDimmer::apply) diffs the desired [`DimCommand`] set against
//! the overlays on screen using the pure [`plan`](crate::plan) kernel, then the
//! worker executes the resulting [`OverlayOp`](crate::plan::OverlayOp)s. Commands
//! travel over a channel and wake the worker with a posted private message, so
//! `apply`/`clear` are synchronous from the caller's view (they block on a reply)
//! while all window ownership stays on the one thread Win32 requires.
//!
//! Gamma and the HDR probe live in [`gamma`] and [`hdr`]; they are deliberately
//! **not** part of the overlay [`apply`] path (a routine dim never touches the
//! persistent gamma ramp).

// RATIONALE: the backend's public vocabulary (`WindowsDimmer`, `ScreenStateGuard`)
// namespaces the crate concept; the qualified names read best at call sites.
#![allow(clippy::module_name_repetitions)]

mod gamma;
mod hdr;
mod sys;

use std::fmt;
use std::sync::mpsc::{Receiver as MpscReceiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use windows::Win32::Foundation::HWND;

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};

use crate::plan::{OverlayEntry, OverlayOp, plan_transition};

pub use gamma::{
    GammaDisplay, GammaRamp, RestoreReport, ScreenStateGuard, clear_marker,
    enumerate_gamma_displays, mark_dirty, marker_present, restore_all, restore_identity, set_gamma,
};
pub use hdr::{GammaSupport, display_supports_gamma, gamma_support_from_hdr, is_hdr_active};

/// Upper bound on how long [`WindowsDimmer::dispatch`] waits for the overlay
/// worker's one-shot reply before giving up and degrading to a backend failure.
///
/// A healthy apply/clear round-trips through the worker (channel hop +
/// `SetLayeredWindowAttributes`/`CreateWindowExW`) well under the crate's ~16 ms
/// frame budget, so this ceiling is orders of magnitude larger and never trips
/// on a working worker. Its job is the pathological case: a worker wedged inside
/// a hung global CBT/shell hook, an AV shim, or RDP shadowing must not freeze the
/// calling thread — the Slint UI thread — indefinitely. Bounding the wait lets
/// the caller fall back to software failure and keep the UI (and `begin_quit`)
/// responsive. Mirrors the engine's ADR-0017 bounded-wait philosophy.
const DIMMER_REPLY_BUDGET: Duration = Duration::from_secs(2);

/// A command sent to the overlay worker thread, each carrying a one-shot reply.
enum Command {
    /// Apply a full desired state; reply with the diff-execution result.
    Apply(Vec<DimCommand>, SyncSender<Result<(), DimmerError>>),
    /// Remove every overlay; reply when done.
    Clear(SyncSender<Result<(), DimmerError>>),
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Apply(cmds, _) => f.debug_tuple("Apply").field(&cmds.len()).finish(),
            Command::Clear(_) => f.write_str("Clear"),
        }
    }
}

/// The Windows software-dimming backend: per-monitor click-through overlays on a
/// dedicated owner thread.
///
/// Construct with [`spawn`](Self::spawn). Drop (or [`shutdown`](Self::shutdown))
/// destroys every overlay and joins the thread.
pub struct WindowsDimmer {
    /// The control window handle (as `isize`) for cross-thread wake/stop posts.
    control: isize,
    /// Command sink for the worker.
    tx: Sender<Command>,
    /// The worker's join handle, taken on the first shutdown.
    join: Option<JoinHandle<()>>,
}

impl fmt::Debug for WindowsDimmer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsDimmer")
            .field("running", &self.join.is_some())
            .finish_non_exhaustive()
    }
}

impl WindowsDimmer {
    /// Spawn the overlay owner thread and block until it has created its control
    /// window (or failed to initialise).
    ///
    /// # Errors
    /// [`DimmerError::Os`] if the thread could not be spawned or the worker
    /// failed to register its window class / create the control window.
    pub fn spawn() -> Result<Self, DimmerError> {
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let (init_tx, init_rx) = sync_channel::<Result<isize, DimmerError>>(1);

        let join = std::thread::Builder::new()
            .name("duja-dimmer-overlays".to_owned())
            .spawn(move || worker_main(&rx, &init_tx))
            .map_err(|e| DimmerError::Os(format!("failed to spawn overlay thread: {e}")))?;

        match init_rx.recv() {
            Ok(Ok(control)) => Ok(WindowsDimmer {
                control,
                tx,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                let _ = join.join();
                Err(DimmerError::Os(
                    "overlay thread exited before initialising".to_owned(),
                ))
            }
        }
    }

    /// Post the shutdown message and join the worker. Idempotent: the join
    /// handle is taken on the first call, so later calls (and [`Drop`]) are
    /// no-ops.
    ///
    /// # Limitation
    /// The [`apply`](Self::apply)/[`clear`](Self::clear) reply wait is bounded
    /// (see `DIMMER_REPLY_BUDGET`), but this `join` is **not**: a worker wedged
    /// inside a hung Win32 call never processes the shutdown post, so the join
    /// blocks until it does. Stable `std` has no timed join; a bounded teardown
    /// would need a detach-or-timeout mechanism and is left as follow-up. In
    /// practice the same wedge that would hang the join already degrades
    /// apply/clear to a backend failure, so the UI stays responsive until quit.
    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            sys::post_shutdown(self.control);
            let _ = join.join();
        }
    }

    /// Send a command to the worker, wake it, and block for its reply.
    fn dispatch(
        &self,
        make: impl FnOnce(SyncSender<Result<(), DimmerError>>) -> Command,
    ) -> Result<(), DimmerError> {
        if self.join.is_none() {
            return Err(DimmerError::Backend);
        }
        let (reply_tx, reply_rx) = sync_channel::<Result<(), DimmerError>>(1);
        self.tx
            .send(make(reply_tx))
            .map_err(|_| DimmerError::Backend)?;
        sys::post_wake(self.control);
        // Bounded wait: a wedged worker degrades to a backend failure instead of
        // freezing the caller (the Slint UI thread). A late reply that arrives
        // after this returns lands on the now-dropped `reply_rx`, which just
        // fails the worker's `reply.send(..)` harmlessly (it uses `let _ =`).
        recv_reply(&reply_rx, DIMMER_REPLY_BUDGET)
    }
}

/// Wait for the overlay worker's one-shot reply, but never longer than `budget`.
///
/// A healthy worker replies within a frame; a wedged one (a hung global hook
/// inside `CreateWindowExW`/`DestroyWindow`, an AV shim, RDP shadowing) would
/// otherwise block the caller — the Slint UI thread — forever. On a timeout *or*
/// a disconnected worker we degrade to [`DimmerError::Backend`] so the caller
/// can fall back to software failure instead of freezing.
fn recv_reply(
    rx: &MpscReceiver<Result<(), DimmerError>>,
    budget: Duration,
) -> Result<(), DimmerError> {
    // Timeout OR disconnect both degrade to `Backend`: a bounded wait, never an
    // unbounded block on a wedged worker.
    match rx.recv_timeout(budget) {
        Ok(reply) => reply,
        Err(_) => Err(DimmerError::Backend),
    }
}

impl Dimmer for WindowsDimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        let owned = commands.to_vec();
        self.dispatch(move |reply| Command::Apply(owned, reply))
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        self.dispatch(Command::Clear)
    }
}

impl Drop for WindowsDimmer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// One overlay the worker is currently showing: its bookkeeping entry plus the
/// live window handle.
struct Overlay {
    entry: OverlayEntry,
    hwnd: HWND,
}

/// The worker thread's overlay state and executor.
struct Worker {
    hinstance: windows::Win32::Foundation::HINSTANCE,
    overlays: Vec<Overlay>,
}

impl Worker {
    fn new(hinstance: windows::Win32::Foundation::HINSTANCE) -> Self {
        Worker {
            hinstance,
            overlays: Vec::new(),
        }
    }

    /// The current visible-overlay entries, for [`plan_transition`].
    fn entries(&self) -> Vec<OverlayEntry> {
        self.overlays.iter().map(|o| o.entry.clone()).collect()
    }

    /// Diff `desired` against the current overlays and execute the plan.
    fn apply(&mut self, desired: &[DimCommand]) -> Result<(), DimmerError> {
        let current = self.entries();
        for op in plan_transition(&current, desired) {
            self.exec(op)?;
        }
        Ok(())
    }

    /// Execute one overlay operation, updating bookkeeping on success.
    fn exec(&mut self, op: OverlayOp) -> Result<(), DimmerError> {
        match op {
            OverlayOp::Create { id, bounds, alpha } => {
                let hwnd = sys::create_overlay(self.hinstance, bounds, alpha)?;
                self.overlays.push(Overlay {
                    entry: OverlayEntry { id, bounds, alpha },
                    hwnd,
                });
            }
            OverlayOp::MoveResize { id, bounds } => {
                let idx = self.index_of(&id)?;
                if let Some(o) = self.overlays.get_mut(idx) {
                    sys::move_overlay(o.hwnd, bounds)?;
                    o.entry.bounds = bounds;
                }
            }
            OverlayOp::SetAlpha { id, alpha } => {
                let idx = self.index_of(&id)?;
                if let Some(o) = self.overlays.get_mut(idx) {
                    sys::set_overlay_alpha(o.hwnd, alpha)?;
                    o.entry.alpha = alpha;
                }
            }
            OverlayOp::Destroy { id } => {
                if let Some(idx) = self.overlays.iter().position(|o| o.entry.id == id) {
                    let removed = self.overlays.remove(idx);
                    sys::destroy_window(removed.hwnd);
                }
            }
        }
        Ok(())
    }

    /// Index of the overlay for `id`. The plan only ever names existing
    /// overlays for move/alpha ops, so a miss is an internal invariant break.
    fn index_of(&self, id: &duja_core::id::StableDisplayId) -> Result<usize, DimmerError> {
        self.overlays
            .iter()
            .position(|o| &o.entry.id == id)
            .ok_or(DimmerError::Backend)
    }

    /// Destroy every overlay (used by `clear` and teardown).
    fn clear(&mut self) {
        for o in self.overlays.drain(..) {
            sys::destroy_window(o.hwnd);
        }
    }

    /// Drain and process every queued command, replying to each.
    fn drain(&mut self, rx: &Receiver<Command>) {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Command::Apply(desired, reply) => {
                    let _ = reply.send(self.apply(&desired));
                }
                Command::Clear(reply) => {
                    self.clear();
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }
}

/// The overlay worker thread body: initialise, hand back the control HWND, then
/// serve commands until a shutdown message, then destroy every window.
fn worker_main(rx: &Receiver<Command>, init_tx: &SyncSender<Result<isize, DimmerError>>) {
    sys::ensure_dpi_awareness();

    let hinstance = match sys::module_handle() {
        Ok(h) => h,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    if let Err(e) = sys::register_classes(hinstance) {
        let _ = init_tx.send(Err(e));
        return;
    }
    let control = match sys::create_control_window(hinstance) {
        Ok(h) => h,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    if init_tx.send(Ok(sys::hwnd_to_isize(control))).is_err() {
        // Spawner vanished before receiving; tear the control window down.
        sys::destroy_window(control);
        return;
    }

    let mut worker = Worker::new(hinstance);
    loop {
        match sys::pump_next() {
            None => break,                                  // WM_QUIT
            Some(m) if m == sys::WM_DUJA_SHUTDOWN => break, // requested stop
            Some(m) if m == sys::WM_DUJA_WAKE => worker.drain(rx),
            Some(_) => {} // an incidental window message, already dispatched
        }
    }

    worker.clear();
    sys::destroy_window(control);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the "wedged worker freezes the UI thread" bug: the reply
    /// wait must be *bounded*. We run it on a helper thread and watchdog that
    /// thread, so a regression to a blocking `recv()` fails this test cleanly
    /// instead of hanging the whole suite.
    #[test]
    fn recv_reply_is_bounded_when_worker_never_replies() {
        // The worker is "wedged": the sender stays alive (so the wait is a real
        // timeout, not a disconnect) but no reply is ever sent.
        let (_never_replies, reply_rx) = sync_channel::<Result<(), DimmerError>>(1);
        let budget = Duration::from_millis(150);

        let (done_tx, done_rx) = sync_channel::<Result<(), DimmerError>>(1);
        let handle = std::thread::spawn(move || {
            let _ = done_tx.send(recv_reply(&reply_rx, budget));
        });

        // Generous watchdog: far above the budget so a healthy bounded wait
        // always lands, but finite so a blocking recv() surfaces as a failure.
        // On timeout we must NOT join `handle` — the wedged wait would never
        // return and joining it would hang the test instead of failing it.
        let watchdog = Duration::from_secs(5);
        match done_rx.recv_timeout(watchdog) {
            Ok(reply) => {
                handle.join().ok();
                assert!(
                    matches!(reply, Err(DimmerError::Backend)),
                    "expected Backend on no-reply, got {reply:?}"
                );
            }
            Err(e) => panic!(
                "recv_reply did not return within {watchdog:?} ({e:?}): it blocked past \
                 its {budget:?} budget (regression to a blocking recv())"
            ),
        }
    }

    /// A worker that replies passes its result straight through, unchanged.
    #[test]
    fn recv_reply_propagates_worker_result() {
        let (ok_tx, ok_rx) = sync_channel::<Result<(), DimmerError>>(1);
        ok_tx.send(Ok(())).unwrap();
        assert!(recv_reply(&ok_rx, Duration::from_secs(1)).is_ok());

        let (err_tx, err_rx) = sync_channel::<Result<(), DimmerError>>(1);
        err_tx
            .send(Err(DimmerError::Os("boom".to_owned())))
            .unwrap();
        assert!(matches!(
            recv_reply(&err_rx, Duration::from_secs(1)),
            Err(DimmerError::Os(_))
        ));
    }
}

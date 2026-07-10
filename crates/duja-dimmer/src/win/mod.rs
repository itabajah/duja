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
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use windows::Win32::Foundation::HWND;

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};

use crate::plan::{OverlayEntry, OverlayOp, plan_transition};

pub use gamma::{
    GammaDisplay, GammaRamp, RestoreReport, ScreenStateGuard, clear_marker,
    enumerate_gamma_displays, mark_dirty, marker_present, restore_all, restore_identity, set_gamma,
};
pub use hdr::{GammaSupport, display_supports_gamma, gamma_support_from_hdr, is_hdr_active};

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
        reply_rx.recv().map_err(|_| DimmerError::Backend)?
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

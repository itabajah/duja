//! The X11 overlay backend: one click-through, translucent, always-on-top window
//! per dimmed display.
//!
//! ADR-0003's primary mechanism, on the display server that needs the most care
//! to get it right. The shape follows the Windows backend — a dedicated thread
//! owns every window, [`apply`](duja_core::dimmer::Dimmer::apply) diffs with the
//! pure [`crate::plan`] kernel and executes the ops on that thread — with the
//! decisions that are arithmetic rather than windowing pulled out into
//! [`crate::linux_overlay`], where every lane can test them.
//!
//! # Translucency on X11 is a contract with a third party
//!
//! X ignores a window's alpha channel: it draws the window's colour bytes at full
//! coverage. A **compositing manager** is what redirects the window to an
//! off-screen pixmap and blends it, so the overlay only dims if one is running —
//! which is why [`crate::linux_caps`] refuses the overlay when nothing owns
//! `_NET_WM_CM_S<n>`, and why every alpha would otherwise paint the same solid
//! black rectangle over the whole monitor.
//!
//! That contract can be broken after the window is mapped, in two ways, and this
//! module handles both because nothing above it can:
//!
//! - **The compositing manager stops.** `picom` crashes, or the user restarts it.
//!   The overlays are then unredirected and the screen goes black. So the worker
//!   watches the selection itself — `XFixesSelectSelectionInput` on
//!   `_NET_WM_CM_S<n>` — and **tears every overlay down** the moment the owner
//!   changes to `None`. Losing the dimming is a visible, recoverable degradation;
//!   keeping it is a screen the user cannot see to fix.
//! - **A fullscreen window is unredirected.** Compositors do this as a
//!   performance optimisation, and an always-on-top fullscreen window is exactly
//!   what an overlay is. Every overlay therefore carries
//!   `_NET_WM_BYPASS_COMPOSITOR = 2` ("never bypass") — a mitigation rather than
//!   a guarantee, for the reason [`crate::linux_overlay`] gives.
//!
//! # There is no always-on-top on X11
//!
//! `CreateWindow` places a window above its siblings, and every top-level is a
//! sibling of an override-redirect overlay under the root. So raising once at map
//! time is not enough: the first window the user opens afterwards sits *undimmed*
//! on top of the overlay. The watcher therefore also selects `SubstructureNotify`
//! on the root and the worker re-raises on anything that is not one of its own
//! windows. A raise-war with another always-on-top client is possible and is a
//! documented limit rather than something this can prevent.
//!
//! # Input must pass through
//!
//! The security invariant ADR-0003 states for every platform. Here it is the
//! **`XFixes` input shape**: each overlay's input region is set to an empty region,
//! so the server routes every pointer and keyboard event to whatever is beneath.
//! SHAPE's own `ShapeInput` kind can express the same thing, so `XFixes` is not
//! the only mechanism X offers — it is the one this uses, and the backend refuses
//! to start without it rather than mapping a window that would swallow clicks.
//!
//! # Crash safety
//!
//! Overlay windows are owned by the connection and the X server destroys them
//! when it closes, so a crash cannot leave one on screen. That is the opposite of
//! the Windows gamma ramp, which persists and needs a marker file; nothing of the
//! sort is needed here.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{Receiver as MpscReceiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use tracing::warn;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::shape::SK;
use x11rb::protocol::xfixes::{self, ConnectionExt as _, Region};
use x11rb::protocol::xproto::{
    self, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, StackMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};
use duja_core::id::StableDisplayId;

use crate::linux_overlay::{
    ARGB_DEPTH, BYPASS_COMPOSITOR_NEVER, Recorded, VisualCandidate, XFIXES_INPUT_SHAPE_VERSION,
    choose_argb_visual, plan_record, premultiplied_black, x11_rect,
};
use crate::plan::{OverlayEntry, OverlayOp, apply_ops, plan_transition};

/// How long a caller waits for the worker's reply before degrading.
///
/// Same contract as the Windows backend's: the caller is the Slint UI thread, and
/// a worker wedged in an X round trip against an unresponsive server must not
/// freeze it. A late reply lands on a dropped receiver and is discarded.
const REPLY_BUDGET: Duration = Duration::from_secs(2);

/// The shortest gap between two re-raises.
///
/// Long enough that a raise-war cannot spin a CPU, short enough that a user
/// opening a window never sees the overlay lag behind it. The exact value is not
/// load-bearing: anything in the tens of milliseconds does both jobs.
const RAISE_DAMPING: Duration = Duration::from_millis(100);

/// A command for the overlay worker.
enum Command {
    /// Apply a full desired state; reply with the diff-execution result.
    Apply(Vec<DimCommand>, SyncSender<Result<(), DimmerError>>),
    /// Remove every overlay; reply when done.
    Clear(SyncSender<Result<(), DimmerError>>),
    /// The compositing manager is gone: tear everything down and latch, no reply.
    CompositorLost,
    /// Something other than an overlay was mapped or restacked, so the overlays
    /// are no longer on top. Carries the window that moved, so the worker can
    /// ignore its own.
    Restacked(Window),
    /// Stop the worker.
    Stop,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Apply(cmds, _) => f.debug_tuple("Apply").field(&cmds.len()).finish(),
            Command::Clear(_) => f.write_str("Clear"),
            Command::CompositorLost => f.write_str("CompositorLost"),
            Command::Restacked(window) => f.debug_tuple("Restacked").field(window).finish(),
            Command::Stop => f.write_str("Stop"),
        }
    }
}

/// The X11 software-dimming backend.
///
/// Construct with [`spawn`](Self::spawn). Drop (or [`shutdown`](Self::shutdown))
/// destroys every overlay and joins both threads.
pub struct X11Dimmer {
    tx: Sender<Command>,
    /// Kept so `shutdown` can wake the watcher, which is blocked in
    /// `wait_for_event` and has no other way to be interrupted.
    waker: Waker,
    worker: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
}

/// The window and message type that mean "stop watching".
///
/// Copied into the watcher so it can tell Duja's own wake from any other client
/// message that happens to reach this connection.
#[derive(Clone, Copy)]
struct Wake {
    window: Window,
    atom: xproto::Atom,
}

/// What it takes to wake a thread blocked in `wait_for_event`.
///
/// X11 has no "interrupt this connection" call. The portable way is to make an
/// event happen: send one to a window we own, of a type only we send.
struct Waker {
    connection: Arc<RustConnection>,
    wake: Wake,
}

impl Waker {
    /// Deliver the wake event. Best effort: a connection already broken is a
    /// watcher already returning.
    fn wake(&self) {
        let event = xproto::ClientMessageEvent::new(
            32,
            self.wake.window,
            self.wake.atom,
            [0_u32, 0, 0, 0, 0],
        );
        let _ =
            self.connection
                .send_event(false, self.wake.window, EventMask::STRUCTURE_NOTIFY, event);
        let _ = self.connection.flush();
    }
}

impl fmt::Debug for X11Dimmer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("X11Dimmer")
            .field("running", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

impl X11Dimmer {
    /// Connect, verify the session can host a translucent click-through window,
    /// and start the worker and the compositor watcher.
    ///
    /// # Errors
    /// [`DimmerError::Os`] when the connection fails, when the server offers no
    /// ARGB visual (the overlay would be opaque black), or when `XFixes` is absent
    /// (the overlay would swallow input). Each of those is refused at startup
    /// rather than discovered by a user, because all three fail *silently* at the
    /// point of use.
    ///
    /// [`DimmerError::Unsupported`] when no compositing manager is running. That
    /// one is not a fault and is expected to change during a session, so it is
    /// distinguishable from the rest.
    pub fn spawn() -> Result<Self, DimmerError> {
        let (connection, screen_index) = x11rb::connect(None)
            .map_err(|e| DimmerError::Os(format!("cannot reach the X server: {e}")))?;
        let connection = Arc::new(connection);

        let setup = connection.setup();
        let screen = setup
            .roots
            .get(screen_index)
            .ok_or_else(|| DimmerError::Os("the X server named no such screen".to_owned()))?;
        let root = screen.root;
        let visual = choose_argb_visual(&visual_candidates(screen)).ok_or_else(|| {
            DimmerError::Os(
                "the X server offers no 32-bit visual, so an overlay would be opaque".to_owned(),
            )
        })?;

        // `XFixes` is what makes the overlay click-through. Refuse without it: an
        // overlay that swallows every click is worse than no dimming, and it is
        // the one failure a user cannot work around from inside the app.
        //
        // The version is negotiated, not assumed. The server answers with the
        // minimum of what was asked and what it has, and `SetWindowShapeRegion`
        // is an XFixes **2.0** request - so a server that negotiates 1 would take
        // this request as a `BadRequest`, and without checking, that failure
        // would surface as a mapped window that eats every click.
        let negotiated = connection
            .xfixes_query_version(5, 0)
            .map_err(|e| DimmerError::Os(format!("XFixes query failed: {e}")))?
            .reply();
        let too_old = |version: u32| {
            DimmerError::Os(format!(
                "the X server offers XFixes {version}, and an input shape needs 2 or later"
            ))
        };
        match negotiated {
            Ok(reply) if reply.major_version >= XFIXES_INPUT_SHAPE_VERSION => {}
            Ok(reply) => return Err(too_old(reply.major_version)),
            Err(_) => {
                return Err(DimmerError::Os(
                    "the X server has no XFixes, so an overlay could not pass input through"
                        .to_owned(),
                ));
            }
        }

        let compositor_selection = intern(
            &connection,
            &crate::linux_caps::compositor_selection(screen_index),
        )
        .ok_or(DimmerError::Unsupported)?;
        if !owned(&connection, compositor_selection) {
            return Err(DimmerError::Unsupported);
        }

        let atoms = Atoms::intern(&connection)?;
        let waker_window = create_waker_window(&connection, root)?;
        let wake = Wake {
            window: waker_window,
            atom: atoms.wake,
        };

        // One colormap for every overlay: they all use the same visual on the same
        // root. A per-window one would have to be freed explicitly - a colormap
        // does not die with the window that used it - and every dim/undim cycle
        // would leak one for the life of the connection.
        let colormap = request(connection.generate_id())?;
        checked(connection.create_colormap(xproto::ColormapAlloc::NONE, colormap, root, visual))?;

        // Ask for owner changes on the compositor selection *before* the watcher
        // starts, so a manager that dies during startup is not missed.
        // Checked, not merely queued. If this request fails the watcher receives
        // no selection events ever, the compositor-loss guard is silently absent,
        // and the failure mode is precisely the black screen it exists to
        // prevent - which makes it more dangerous than window creation, which is
        // also checked.
        checked(connection.xfixes_select_selection_input(
            waker_window,
            compositor_selection,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER
                | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE,
        ))
        .map_err(|e| DimmerError::Os(format!("cannot watch the compositor selection: {e}")))?;
        // And for anything being mapped or restacked on the root. X has no
        // always-on-top: `CreateWindow` puts a new window above its siblings, and
        // every top-level is a sibling of an override-redirect overlay. Without
        // this the desktop dims and the first window the user opens sits undimmed
        // on top of it.
        // Checked for the same reason: without it no restack is ever reported and
        // the overlay silently stops being on top - the desktop dims and the first
        // window the user opens sits undimmed over it.
        checked(connection.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        ))?;
        connection
            .flush()
            .map_err(|e| DimmerError::Os(format!("X flush failed: {e}")))?;

        let (tx, rx) = crossbeam_channel::unbounded::<Command>();

        let worker_connection = Arc::clone(&connection);
        let worker = std::thread::Builder::new()
            .name("duja-dimmer-x11".to_owned())
            .spawn(move || {
                let mut state = Worker {
                    connection: worker_connection,
                    root,
                    visual,
                    colormap,
                    atoms,
                    windows: Vec::new(),
                    current: Vec::new(),
                    last_raise: None,
                    lost: false,
                };
                state.run(&rx);
            })
            .map_err(|e| DimmerError::Os(format!("failed to spawn the overlay thread: {e}")))?;

        let watcher_connection = Arc::clone(&connection);
        let watcher_tx = tx.clone();
        let watcher = std::thread::Builder::new()
            .name("duja-dimmer-x11-watch".to_owned())
            .spawn(move || {
                watch_compositor(&watcher_connection, &watcher_tx, compositor_selection, wake);
            })
            .map_err(|e| DimmerError::Os(format!("failed to spawn the watcher thread: {e}")))?;

        Ok(X11Dimmer {
            tx,
            waker: Waker { connection, wake },
            worker: Some(worker),
            watcher: Some(watcher),
        })
    }

    /// Destroy every overlay and stop both threads. Idempotent.
    ///
    /// # Limitation
    ///
    /// The [`apply`](Dimmer::apply)/[`clear`](Dimmer::clear) reply wait is bounded
    /// by `REPLY_BUDGET`, but these joins are **not**. A worker inside a `flush`
    /// against a wedged X server never reaches the `Stop`, and stable `std` has no
    /// timed join. Same trade the Windows backend documents: the wedge that would
    /// hang this join has already degraded apply/clear to a backend failure, so the
    /// UI stays responsive until quit — at which point `Drop` runs this.
    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.tx.send(Command::Stop);
            let _ = worker.join();
        }
        if let Some(watcher) = self.watcher.take() {
            // The watcher is blocked in `wait_for_event` and there is no way to
            // interrupt that; the only portable wake is an event it will receive.
            self.waker.wake();
            let _ = watcher.join();
        }
    }

    /// Send a command and block, bounded, for the reply.
    fn dispatch(
        &self,
        make: impl FnOnce(SyncSender<Result<(), DimmerError>>) -> Command,
    ) -> Result<(), DimmerError> {
        if self.worker.is_none() {
            return Err(DimmerError::Backend);
        }
        let (reply_tx, reply_rx) = sync_channel::<Result<(), DimmerError>>(1);
        self.tx
            .send(make(reply_tx))
            .map_err(|_| DimmerError::Backend)?;
        recv_reply(&reply_rx)
    }
}

/// Wait for the worker's one-shot reply, but never longer than the budget.
fn recv_reply(rx: &MpscReceiver<Result<(), DimmerError>>) -> Result<(), DimmerError> {
    match rx.recv_timeout(REPLY_BUDGET) {
        Ok(reply) => reply,
        Err(_) => Err(DimmerError::Backend),
    }
}

impl Drop for X11Dimmer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Dimmer for X11Dimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        let sanitized: Vec<DimCommand> = commands.iter().map(DimCommand::sanitized).collect();
        self.dispatch(|reply| Command::Apply(sanitized, reply))
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        self.dispatch(Command::Clear)
    }
}

/// The atoms the overlay sets on every window it creates.
#[derive(Clone, Copy)]
struct Atoms {
    bypass_compositor: xproto::Atom,
    window_type: xproto::Atom,
    window_type_notification: xproto::Atom,
    /// A private message type, so the watcher's shutdown wake cannot be confused
    /// with a client message from anyone else.
    wake: xproto::Atom,
}

impl Atoms {
    fn intern(connection: &RustConnection) -> Result<Self, DimmerError> {
        let missing = || DimmerError::Os("the X server refused to intern an atom".to_owned());
        Ok(Atoms {
            bypass_compositor: intern_always(connection, "_NET_WM_BYPASS_COMPOSITOR")
                .ok_or_else(missing)?,
            window_type: intern_always(connection, "_NET_WM_WINDOW_TYPE").ok_or_else(missing)?,
            window_type_notification: intern_always(connection, "_NET_WM_WINDOW_TYPE_NOTIFICATION")
                .ok_or_else(missing)?,
            wake: intern_always(connection, "_DUJA_OVERLAY_WAKE").ok_or_else(missing)?,
        })
    }
}

/// One overlay window and which display it covers.
struct Overlay {
    id: StableDisplayId,
    window: Window,
    region: Region,
}

/// The thread that owns every overlay window.
struct Worker {
    connection: Arc<RustConnection>,
    root: Window,
    visual: u32,
    /// One colormap for every overlay. They all use the same visual on the same
    /// root, so one is enough — and a per-window one would leak, because a
    /// colormap is freed explicitly rather than with the window that used it.
    colormap: xproto::Colormap,
    atoms: Atoms,
    windows: Vec<Overlay>,
    current: Vec<OverlayEntry>,
    /// When the overlays were last raised, for damping. See [`Worker::raise_above`].
    last_raise: Option<Instant>,
    /// Latched once the compositing manager goes away.
    ///
    /// Tearing the overlays down is only half the answer: without this the very
    /// next `apply` would plan a full set of `Create`s and map fresh windows onto
    /// a session that can no longer blend them, which is a black screen — the
    /// exact failure the teardown exists to prevent, one slider sample later.
    /// This is the [`crate::linux_caps::SurfaceCaps::refuse_gamma`] analogue for
    /// the overlay arm, and like it, it never un-latches: a compositor that comes
    /// back needs a fresh backend, because this connection's windows are gone and
    /// nothing here re-runs the startup checks.
    lost: bool,
}

impl Worker {
    fn run(&mut self, rx: &Receiver<Command>) {
        while let Ok(command) = rx.recv() {
            match command {
                Command::Apply(commands, reply) => {
                    let result = self.apply(&commands);
                    let _ = reply.send(result);
                }
                Command::Clear(reply) => {
                    // Always allowed, even after the compositor is lost: there is
                    // then nothing to clear and the caller gets the state it
                    // asked for.
                    let result = self.destroy_all();
                    let _ = reply.send(result);
                }
                // Nothing asked for this, so there is no caller to report a
                // failure to. The windows die with the connection regardless.
                Command::CompositorLost => {
                    self.lost = true;
                    let _ = self.destroy_all();
                }
                Command::Restacked(window) => self.raise_above(window, rx),
                Command::Stop => {
                    let _ = self.destroy_all();
                    return;
                }
            }
        }
        let _ = self.destroy_all();
    }

    /// Diff, execute, and record **what actually happened**.
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        if self.lost {
            return Err(DimmerError::Unsupported);
        }
        let ops = plan_transition(&self.current, commands);
        let (applied, outcome) = self.execute(&ops);
        // Folded from `applied`, never from `ops`, and on the failure path too.
        // The two diverge whenever an op is skipped (a rectangle X11 cannot
        // express) or the list stops early, and recording an op that did not
        // happen leaves `current` describing a window that does not exist — after
        // which the planner never emits `Create` for that display again and it
        // can never be dimmed for the rest of the session.
        self.current = apply_ops(&self.current, &applied);
        outcome
    }

    /// Run the planner's ops, stopping at the first failure.
    ///
    /// Returns the ops that genuinely took effect, which is what the caller folds
    /// into its record of the screen. **What each op does, and what it records,
    /// is decided by [`plan_record`]** — a pure rule tested on every lane, because
    /// getting it wrong is how a display becomes permanently undimmable and no
    /// test of the windowing itself can run.
    fn execute(&mut self, ops: &[OverlayOp]) -> (Vec<OverlayOp>, Result<(), DimmerError>) {
        let mut applied: Vec<OverlayOp> = Vec::new();
        for op in ops {
            let id = op_id(op);
            let outcome: Result<Option<OverlayOp>, DimmerError> =
                match plan_record(op, self.find(id).is_some()) {
                    Recorded::Nothing => Ok(None),
                    // Either there is no window to act on, or the op would put one
                    // where X11 cannot express it. Destroying is a no-op in the
                    // first case; recording the destroy is what drops the stale
                    // entry so the next plan emits a fresh `Create`.
                    Recorded::DestroyInstead => self
                        .destroy(id)
                        .map(|()| Some(OverlayOp::Destroy { id: id.clone() })),
                    Recorded::AsPlanned => self.perform(op).map(|()| Some(op.clone())),
                };
            match outcome {
                Ok(Some(done)) => applied.push(done),
                Ok(None) => {}
                Err(e) => return (applied, flush(&self.connection).and(Err(e))),
            }
        }
        (applied, flush(&self.connection))
    }

    /// Do exactly what an op says, having already been told it is doable.
    ///
    /// Every lookup here is infallible by construction: [`plan_record`] answered
    /// `AsPlanned`, which for the two arms that need a window means there is one
    /// and for the two that need a rectangle means it converts. A `None` would be
    /// a bug in that rule rather than a state to handle, so it is treated as a
    /// no-op rather than given a second, divergent policy.
    fn perform(&mut self, op: &OverlayOp) -> Result<(), DimmerError> {
        match op {
            OverlayOp::Create { id, bounds, alpha } => match x11_rect(*bounds) {
                Some(rect) => self.create(id.clone(), rect, *alpha),
                None => Ok(()),
            },
            OverlayOp::MoveResize { id, bounds } => {
                let (Some((x, y, width, height)), Some(overlay)) =
                    (x11_rect(*bounds), self.find(id))
                else {
                    return Ok(());
                };
                let config = xproto::ConfigureWindowAux::new()
                    .x(i32::from(x))
                    .y(i32::from(y))
                    .width(u32::from(width))
                    .height(u32::from(height))
                    .stack_mode(StackMode::ABOVE);
                // The input region is in window coordinates and stays empty
                // across a resize, so it needs no rebuilding.
                request(self.connection.configure_window(overlay.window, &config)).map(|_| ())
            }
            OverlayOp::SetAlpha { id, alpha } => {
                let Some(overlay) = self.find(id) else {
                    return Ok(());
                };
                let window = overlay.window;
                self.repaint(window, *alpha)
            }
            OverlayOp::Destroy { id } => self.destroy(id),
        }
    }

    fn find(&self, id: &StableDisplayId) -> Option<&Overlay> {
        self.windows.iter().find(|o| &o.id == id)
    }

    /// Put every overlay back on top after something else was mapped or raised.
    ///
    /// X has no always-on-top: `CreateWindow` places a new window above its
    /// siblings, and every top-level is a sibling of an override-redirect overlay
    /// under the root. Without this the desktop dims and then the first window the
    /// user opens sits *undimmed* on top of it, which reads as the feature simply
    /// not working.
    ///
    /// Two things bound the work, and both matter because the trigger is an event
    /// stream Duja does not control:
    ///
    /// - **Coalescing.** A window being dragged emits a `ConfigureNotify` per
    ///   motion sample. Every pending restack is drained first, because only the
    ///   last one has any effect and each costs one request per overlay plus a
    ///   flush.
    /// - **Damping.** Raising is itself a restack, so any other client that also
    ///   re-raises on root restacks — a second OSD, an on-screen keyboard, a
    ///   magnifier — would trade raises with Duja as fast as the server can
    ///   deliver them: two pegged CPUs and a flickering screen. [`RAISE_DAMPING`]
    ///   turns that unbounded loop into a bounded, visible flicker. It cannot
    ///   *fix* a raise-war, which no X client can; it stops one from becoming a
    ///   spin.
    ///
    /// Duja's own windows are skipped outright, so it never fights itself.
    fn raise_above(&mut self, window: Window, rx: &Receiver<Command>) {
        let mut latest = window;
        // Drain the rest of the burst. Anything that is not a restack is put back
        // by being handled here, which only `Stop` and `CompositorLost` could be
        // — and both are better served immediately than after a raise.
        while let Ok(pending) = rx.try_recv() {
            match pending {
                Command::Restacked(next) => latest = next,
                Command::CompositorLost => {
                    self.lost = true;
                    let _ = self.destroy_all();
                    return;
                }
                Command::Stop => {
                    let _ = self.destroy_all();
                    return;
                }
                // An `Apply`/`Clear` in the burst is a caller waiting on a reply.
                // Serve it, and let the raise fall out: the apply restacks
                // everything it touches anyway.
                Command::Apply(commands, reply) => {
                    let result = self.apply(&commands);
                    let _ = reply.send(result);
                    return;
                }
                Command::Clear(reply) => {
                    let result = self.destroy_all();
                    let _ = reply.send(result);
                    return;
                }
            }
        }
        if self.windows.iter().any(|o| o.window == latest) {
            return;
        }
        let now = Instant::now();
        if self
            .last_raise
            .is_some_and(|last| now.duration_since(last) < RAISE_DAMPING)
        {
            return;
        }
        self.last_raise = Some(now);
        let above = xproto::ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
        for overlay in &self.windows {
            let _ = self.connection.configure_window(overlay.window, &above);
        }
        let _ = self.connection.flush();
    }

    /// Create one overlay: an override-redirect ARGB window with an empty input
    /// region, mapped above everything.
    ///
    /// # Everything up to the map is **checked**
    ///
    /// An x11rb void request returns a cookie meaning "queued", not "accepted".
    /// Dropping that cookie discards the reply and routes any protocol error to
    /// the event loop, where nothing here would act on it — so a `BadMatch` from
    /// `create_window`, or a failed input-region call, would both read as success.
    /// The second of those is the one that matters: it would leave a mapped,
    /// full-screen, override-redirect window that **swallows every click**, with
    /// the flyout you would use to turn it off underneath it.
    ///
    /// So window creation, both input-region calls and the map are `check`ed —
    /// four round trips, once per display per dim session. The two property
    /// writes are not: a failed property is a hint the compositor does not get,
    /// which costs a mitigation rather than the feature.
    fn create(
        &mut self,
        id: StableDisplayId,
        rect: (i16, i16, u16, u16),
        alpha: u8,
    ) -> Result<(), DimmerError> {
        let (x, y, width, height) = rect;
        let window = request(self.connection.generate_id())?;

        let attributes = CreateWindowAux::new()
            // Black at this alpha, with the alpha in the top byte. The compositor
            // blends it; with no compositor every alpha would paint the same
            // opaque rectangle, which is why `spawn` refuses to start without one.
            .background_pixel(premultiplied_black(alpha))
            // Required whenever the window's depth differs from its parent's:
            // the border pixel would otherwise be inherited from a colormap that
            // does not describe this visual, and the server answers `BadMatch`.
            .border_pixel(0)
            .colormap(self.colormap)
            // The window manager never sees this window: no decorations, no
            // focus, no placement policy, no stacking interference.
            .override_redirect(1_u32)
            .event_mask(EventMask::NO_EVENT);

        checked(self.connection.create_window(
            ARGB_DEPTH,
            window,
            self.root,
            x,
            y,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            self.visual,
            &attributes,
        ))?;

        // ADR-0003's security invariant: an empty input region means the server
        // routes every pointer and key event to whatever is underneath.
        let region = request(self.connection.generate_id())?;
        if let Err(e) = checked(self.connection.xfixes_create_region(region, &[])) {
            // The region was never created, so destroying it would be a second,
            // misleading error for one real failure. The window exists and is
            // still unmapped; the colormap is shared and outlives every window,
            // so it is not freed here.
            let _ = self.connection.destroy_window(window);
            let _ = self.connection.flush();
            return Err(e);
        }
        if let Err(e) =
            checked(
                self.connection
                    .xfixes_set_window_shape_region(window, SK::INPUT, 0, 0, region),
            )
        {
            // Destroy it rather than leave an input-stealing rectangle one
            // `map_window` away.
            let _ = self.connection.xfixes_destroy_region(region);
            let _ = self.connection.destroy_window(window);
            let _ = self.connection.flush();
            return Err(e);
        }

        // Registered here, before anything that could fail again: from this point
        // the window is click-through and safe, and `destroy`/`destroy_all` own
        // its cleanup. Pushing it at the end instead would leak the window and the
        // region on any later failure, since neither list would know about them.
        self.windows.push(Overlay { id, window, region });

        // Ask no compositing manager to unredirect this window. See
        // `linux_overlay::BYPASS_COMPOSITOR_NEVER` for what that does and does
        // not guarantee.
        request(self.connection.change_property32(
            PropMode::REPLACE,
            window,
            self.atoms.bypass_compositor,
            AtomEnum::CARDINAL,
            &[BYPASS_COMPOSITOR_NEVER],
        ))?;
        // Advisory for compositors that special-case window types (some skip
        // effects and shadows for notifications). Harmless where it is ignored:
        // an override-redirect window is outside the WM's policy either way.
        request(self.connection.change_property32(
            PropMode::REPLACE,
            window,
            self.atoms.window_type,
            AtomEnum::ATOM,
            &[self.atoms.window_type_notification],
        ))?;

        // Checked too, and for a reason the first draft got wrong: a failed map is
        // not merely "a window that is not there". `create` returning `Ok` records
        // a `Create`, so the record would claim an overlay that is unmapped and
        // invisible - and the planner never emits `Create` for a display it thinks
        // is already covered, leaving it undimmable for the session.
        checked(self.connection.map_window(window))?;
        request(self.connection.configure_window(
            window,
            &xproto::ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        ))?;
        self.last_raise = Some(Instant::now());
        Ok(())
    }

    /// Change an existing overlay's opacity.
    ///
    /// The alpha lives in the window's background pixel, so re-alpha is a
    /// background change plus a `ClearArea` that repaints from it. No graphics
    /// context, no drawing, and nothing to keep in sync on expose.
    fn repaint(&self, window: Window, alpha: u8) -> Result<(), DimmerError> {
        request(self.connection.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().background_pixel(premultiplied_black(alpha)),
        ))?;
        request(self.connection.clear_area(false, window, 0, 0, 0, 0)).map(|_| ())
    }

    fn destroy(&mut self, id: &StableDisplayId) -> Result<(), DimmerError> {
        let Some(index) = self.windows.iter().position(|o| &o.id == id) else {
            return Ok(());
        };
        // `swap_remove` keeps this indexing-free and the order of `windows` is
        // not meaningful: lookup is by id and stacking is the server's.
        let overlay = self.windows.swap_remove(index);
        request(self.connection.xfixes_destroy_region(overlay.region))?;
        request(self.connection.destroy_window(overlay.window)).map(|_| ())
    }

    fn destroy_all(&mut self) -> Result<(), DimmerError> {
        let mut outcome = Ok(());
        while let Some(overlay) = self.windows.pop() {
            let _ = self.connection.xfixes_destroy_region(overlay.region);
            if let Err(e) = request(self.connection.destroy_window(overlay.window)) {
                outcome = Err(e);
            }
        }
        self.current.clear();
        flush(&self.connection).and(outcome)
    }
}

/// The single event loop: the compositing-manager selection, root restacking,
/// and protocol errors.
///
/// Blocks in `wait_for_event` on its own thread with a clone of the connection,
/// which x11rb supports — its locking is built for exactly this, and the worker
/// keeps issuing requests on the same connection meanwhile.
///
/// It keeps running after reporting a lost compositor, for two reasons. It is
/// still the only consumer of this connection's event queue, and protocol errors
/// arrive here rather than at the request that caused them, so a thread that
/// returned early would leave them accumulating unread.
///
/// It does **not** watch for a compositor coming back. The overlays are gone and
/// this connection's worker has latched, so recovering means a fresh backend —
/// and nothing re-runs `spawn` today, because the app starts its dimmer once.
/// `docs/debt.md` carries that.
fn watch_compositor(
    connection: &RustConnection,
    tx: &Sender<Command>,
    selection: xproto::Atom,
    wake: Wake,
) {
    loop {
        let Ok(event) = connection.wait_for_event() else {
            return;
        };
        let outcome = match event {
            Event::XfixesSelectionNotify(_) => {
                // **Do not read `notify.owner`.** It is only meaningful for the
                // orderly-disown subtype: when a compositing manager *crashes* the
                // server reports `SelectionWindowDestroy` / `SelectionClientClose`
                // and fills `owner` with the id of the window that just died,
                // because the callback runs before the selection record is
                // cleared. Treating a non-zero owner as "a restart that already
                // has a new manager" would miss exactly the case this watcher
                // exists for - a crashed compositor, overlays left up and
                // unredirected, every dimmed monitor solid black.
                //
                // So re-ask the server. One round trip on a rare event, and it
                // cannot be wrong under either reading of who fills that field.
                if owned(connection, selection) {
                    Ok(())
                } else {
                    tx.send(Command::CompositorLost)
                }
            }
            // Something appeared or moved in the stacking order. The worker
            // decides whether it was one of ours.
            Event::MapNotify(notify) => tx.send(Command::Restacked(notify.window)),
            Event::ConfigureNotify(notify) => tx.send(Command::Restacked(notify.window)),
            // A protocol error from a request whose cookie was dropped. The
            // checked calls in `create` catch the ones that could trap a user;
            // this is where the rest surface, and a silent drop is how an overlay
            // that never appeared looks like one that did.
            Event::Error(error) => {
                warn!(?error, "the X server refused an overlay request");
                Ok(())
            }
            // The shutdown wake, and only that. Returning on *any* client
            // message would let a stray one from another client silently kill
            // this thread, taking the compositor guard and the re-raising with
            // it and leaving `shutdown` none the wiser.
            Event::ClientMessage(notify) => {
                if notify.window == wake.window && notify.type_ == wake.atom {
                    return;
                }
                Ok(())
            }
            _ => Ok(()),
        };
        // The worker is gone, so there is nobody left to tell.
        if outcome.is_err() {
            return;
        }
    }
}

/// Flatten a screen's depths into the plain data `choose_argb_visual` decides on.
fn visual_candidates(screen: &xproto::Screen) -> Vec<VisualCandidate> {
    screen
        .allowed_depths
        .iter()
        .flat_map(|depth| {
            depth.visuals.iter().map(move |visual| VisualCandidate {
                id: visual.visual_id,
                depth: depth.depth,
                true_color: visual.class == xproto::VisualClass::TRUE_COLOR,
                red_mask: visual.red_mask,
                green_mask: visual.green_mask,
                blue_mask: visual.blue_mask,
            })
        })
        .collect()
}

/// A 1x1 unmapped window that exists to receive the selection notifications and
/// the shutdown wake.
///
/// `InputOnly` because it is never drawn: it needs an id the server will deliver
/// events to and nothing else.
fn create_waker_window(connection: &RustConnection, root: Window) -> Result<Window, DimmerError> {
    let window = request(connection.generate_id())?;
    request(
        connection.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .override_redirect(1_u32)
                .event_mask(EventMask::STRUCTURE_NOTIFY),
        ),
    )?;
    Ok(window)
}

/// Intern an atom that must exist, creating it if it does not.
fn intern_always(connection: &RustConnection, name: &str) -> Option<xproto::Atom> {
    connection
        .intern_atom(false, name.as_bytes())
        .ok()?
        .reply()
        .ok()
        .map(|reply| reply.atom)
}

/// Look up an atom only if something has already created it.
fn intern(connection: &RustConnection, name: &str) -> Option<xproto::Atom> {
    connection
        .intern_atom(true, name.as_bytes())
        .ok()?
        .reply()
        .ok()
        .map(|reply| reply.atom)
        .filter(|atom| *atom != x11rb::NONE)
}

/// Whether anything owns `selection`.
fn owned(connection: &RustConnection, selection: xproto::Atom) -> bool {
    connection
        .get_selection_owner(selection)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.owner != x11rb::NONE)
}

/// Which display an op targets. Every variant carries one.
fn op_id(op: &OverlayOp) -> &StableDisplayId {
    match op {
        OverlayOp::Create { id, .. }
        | OverlayOp::MoveResize { id, .. }
        | OverlayOp::SetAlpha { id, .. }
        | OverlayOp::Destroy { id } => id,
    }
}

/// Turn an x11rb request result into a [`DimmerError`].
///
/// This reports only that the request could be **queued**. A protocol error from
/// the server arrives later, on the event queue, which is why anything that could
/// trap a user goes through [`checked`] instead.
fn request<T, E: fmt::Display>(result: Result<T, E>) -> Result<T, DimmerError> {
    result.map_err(|e| DimmerError::Os(format!("X request failed: {e}")))
}

/// Issue a void request and **wait for the server to accept it**.
///
/// One round trip, and the difference between "queued" and "worked". Used where a
/// silent failure would leave the user worse off than no overlay: a window that
/// was never created (so a display is never dimmed and never retried), or one
/// whose input region was never set (so it swallows every click, including on the
/// flyout that would turn it off).
fn checked<C: x11rb::connection::Connection, E: fmt::Display>(
    result: Result<x11rb::cookie::VoidCookie<'_, C>, E>,
) -> Result<(), DimmerError> {
    request(result)?
        .check()
        .map_err(|e| DimmerError::Os(format!("the X server refused a request: {e}")))
}

/// Flush pending requests so the screen actually changes.
fn flush(connection: &RustConnection) -> Result<(), DimmerError> {
    request(connection.flush())
}

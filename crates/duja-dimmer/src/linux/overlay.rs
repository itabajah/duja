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
//!   `_NET_WM_BYPASS_COMPOSITOR = 2` ("never bypass").
//!
//! # Input must pass through
//!
//! The security invariant ADR-0003 states for every platform. Here it is the
//! **`XFixes` input shape**: each overlay's input region is set to an empty region,
//! so the server routes every pointer and keyboard event to whatever is beneath.
//! Without `XFixes` there is no way to do this on X11 at all, so the backend
//! refuses to start rather than mapping a window that would swallow clicks.
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
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
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
    ARGB_DEPTH, BYPASS_COMPOSITOR_NEVER, VisualCandidate, choose_argb_visual, premultiplied_black,
    x11_rect,
};
use crate::plan::{OverlayEntry, OverlayOp, apply_ops, plan_transition};

/// How long a caller waits for the worker's reply before degrading.
///
/// Same contract as the Windows backend's: the caller is the Slint UI thread, and
/// a worker wedged in an X round trip against an unresponsive server must not
/// freeze it. A late reply lands on a dropped receiver and is discarded.
const REPLY_BUDGET: Duration = Duration::from_millis(750);

/// A command for the overlay worker.
enum Command {
    /// Apply a full desired state; reply with the diff-execution result.
    Apply(Vec<DimCommand>, SyncSender<Result<(), DimmerError>>),
    /// Remove every overlay; reply when done.
    Clear(SyncSender<Result<(), DimmerError>>),
    /// The compositing manager is gone: tear everything down, no reply.
    CompositorLost,
    /// Stop the worker.
    Stop,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Apply(cmds, _) => f.debug_tuple("Apply").field(&cmds.len()).finish(),
            Command::Clear(_) => f.write_str("Clear"),
            Command::CompositorLost => f.write_str("CompositorLost"),
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

/// What it takes to wake a thread blocked in `wait_for_event`.
///
/// X11 has no "interrupt this connection" call. The portable way is to make an
/// event happen: send one to a window we own. The watcher recognises it and
/// returns.
struct Waker {
    connection: Arc<RustConnection>,
    window: Window,
}

impl Waker {
    /// Deliver the wake event. Best effort: a connection already broken is a
    /// watcher already returning.
    fn wake(&self) {
        let event =
            xproto::ClientMessageEvent::new(32, self.window, AtomEnum::NONE, [0_u32, 0, 0, 0, 0]);
        let _ = self
            .connection
            .send_event(false, self.window, EventMask::STRUCTURE_NOTIFY, event);
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
        let xfixes = connection
            .xfixes_query_version(5, 0)
            .map_err(|e| DimmerError::Os(format!("XFixes query failed: {e}")))?
            .reply();
        if xfixes.is_err() {
            return Err(DimmerError::Os(
                "the X server has no XFixes, so an overlay could not pass input through".to_owned(),
            ));
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

        // Ask for owner changes on the compositor selection *before* the watcher
        // starts, so a manager that dies during startup is not missed.
        connection
            .xfixes_select_selection_input(
                waker_window,
                compositor_selection,
                xfixes::SelectionEventMask::SET_SELECTION_OWNER
                    | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                    | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE,
            )
            .map_err(|e| DimmerError::Os(format!("cannot watch the compositor selection: {e}")))?;
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
                    atoms,
                    windows: Vec::new(),
                    current: Vec::new(),
                };
                state.run(&rx);
            })
            .map_err(|e| DimmerError::Os(format!("failed to spawn the overlay thread: {e}")))?;

        let watcher_connection = Arc::clone(&connection);
        let watcher_tx = tx.clone();
        let watcher = std::thread::Builder::new()
            .name("duja-dimmer-x11-watch".to_owned())
            .spawn(move || watch_compositor(&watcher_connection, &watcher_tx))
            .map_err(|e| DimmerError::Os(format!("failed to spawn the watcher thread: {e}")))?;

        Ok(X11Dimmer {
            tx,
            waker: Waker {
                connection,
                window: waker_window,
            },
            worker: Some(worker),
            watcher: Some(watcher),
        })
    }

    /// Destroy every overlay and stop both threads. Idempotent.
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
    atoms: Atoms,
    windows: Vec<Overlay>,
    current: Vec<OverlayEntry>,
}

impl Worker {
    fn run(&mut self, rx: &Receiver<Command>) {
        while let Ok(command) = rx.recv() {
            match command {
                Command::Apply(commands, reply) => {
                    let ops = plan_transition(&self.current, &commands);
                    let result = self.execute(&ops);
                    if result.is_ok() {
                        self.current = apply_ops(&self.current, &ops);
                    }
                    let _ = reply.send(result);
                }
                Command::Clear(reply) => {
                    let result = self.destroy_all();
                    let _ = reply.send(result);
                }
                // Not a reply-bearing command: nothing asked for this, and the
                // only correct response is to stop covering the screen. Failures
                // are ignored because there is no caller to report them to and
                // the windows die with the connection regardless.
                Command::CompositorLost => {
                    let _ = self.destroy_all();
                }
                Command::Stop => {
                    let _ = self.destroy_all();
                    return;
                }
            }
        }
        let _ = self.destroy_all();
    }

    /// Run the planner's ops, stopping at the first failure.
    fn execute(&mut self, ops: &[OverlayOp]) -> Result<(), DimmerError> {
        for op in ops {
            match op {
                OverlayOp::Create { id, bounds, alpha } => {
                    let Some(rect) = x11_rect(*bounds) else {
                        // A rectangle X11 cannot describe. Skipping is right:
                        // the alternative is a wrapped window covering a display
                        // the user did not dim. See `linux_overlay::x11_rect`.
                        continue;
                    };
                    self.create(id.clone(), rect, *alpha)?;
                }
                OverlayOp::MoveResize { id, bounds } => {
                    let Some((x, y, width, height)) = x11_rect(*bounds) else {
                        self.destroy(id)?;
                        continue;
                    };
                    if let Some(overlay) = self.find(id) {
                        let config = xproto::ConfigureWindowAux::new()
                            .x(i32::from(x))
                            .y(i32::from(y))
                            .width(u32::from(width))
                            .height(u32::from(height))
                            .stack_mode(StackMode::ABOVE);
                        request(self.connection.configure_window(overlay.window, &config))?;
                        // The input region is in window coordinates and stays
                        // empty across a resize, so it does not need rebuilding.
                    }
                }
                OverlayOp::SetAlpha { id, alpha } => {
                    if let Some(overlay) = self.find(id) {
                        let window = overlay.window;
                        self.repaint(window, *alpha)?;
                    }
                }
                OverlayOp::Destroy { id } => self.destroy(id)?,
            }
        }
        flush(&self.connection)
    }

    fn find(&self, id: &StableDisplayId) -> Option<&Overlay> {
        self.windows.iter().find(|o| &o.id == id)
    }

    /// Create one overlay: an override-redirect ARGB window with an empty input
    /// region, mapped above everything.
    fn create(
        &mut self,
        id: StableDisplayId,
        rect: (i16, i16, u16, u16),
        alpha: u8,
    ) -> Result<(), DimmerError> {
        let (x, y, width, height) = rect;
        let window = request(self.connection.generate_id())?;
        let colormap = request(self.connection.generate_id())?;
        request(self.connection.create_colormap(
            xproto::ColormapAlloc::NONE,
            colormap,
            self.root,
            self.visual,
        ))?;

        let attributes = CreateWindowAux::new()
            // Premultiplied black at this alpha. The compositor blends it; with
            // no compositor every alpha would paint the same opaque rectangle,
            // which is why `spawn` refuses to start without one.
            .background_pixel(premultiplied_black(alpha))
            // Required whenever the window's depth differs from its parent's:
            // the border pixel would otherwise be inherited from a colormap that
            // does not describe this visual, and the server answers `BadMatch`.
            .border_pixel(0)
            .colormap(colormap)
            // The window manager never sees this window: no decorations, no
            // focus, no placement policy, no stacking interference.
            .override_redirect(1_u32)
            .event_mask(EventMask::NO_EVENT);

        request(self.connection.create_window(
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
        // routes every pointer and key event to whatever is underneath. This is
        // the whole click-through mechanism on X11 and there is no alternative.
        let region = request(self.connection.generate_id())?;
        request(self.connection.xfixes_create_region(region, &[]))?;
        request(
            self.connection
                .xfixes_set_window_shape_region(window, SK::INPUT, 0, 0, region),
        )?;

        // Never let a compositor unredirect this window. Without it a fullscreen
        // game or video turns the overlay opaque - the same black screen the
        // capability check exists to prevent, reached by a different route.
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

        request(self.connection.map_window(window))?;
        request(self.connection.configure_window(
            window,
            &xproto::ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        ))?;

        self.windows.push(Overlay { id, window, region });
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

/// Watch the compositing-manager selection and report the moment it is lost.
///
/// Blocks in `wait_for_event` on its own thread with a clone of the connection,
/// which x11rb supports: the worker keeps issuing requests on the same connection
/// meanwhile.
///
/// Returns on a broken connection, on a `Stop` wake, or once it has reported the
/// loss. There is nothing to watch for afterwards — the overlays are gone, and a
/// compositor that comes back is picked up by the next `spawn`, which the app
/// performs when its capability report is re-resolved.
fn watch_compositor(connection: &RustConnection, tx: &Sender<Command>) {
    loop {
        let Ok(event) = connection.wait_for_event() else {
            return;
        };
        match event {
            Event::XfixesSelectionNotify(notify) => {
                // Owner `NONE` is the manager going away. An owner *change* to a
                // live window is a restart that already has a new manager, and
                // the overlays are fine.
                if notify.owner == x11rb::NONE {
                    let _ = tx.send(Command::CompositorLost);
                    return;
                }
            }
            // The shutdown wake. Anything else is noise on a window that asked
            // for no events.
            Event::ClientMessage(_) => return,
            _ => {}
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

/// Turn an x11rb request result into a [`DimmerError`].
fn request<T, E: fmt::Display>(result: Result<T, E>) -> Result<T, DimmerError> {
    result.map_err(|e| DimmerError::Os(format!("X request failed: {e}")))
}

/// Flush pending requests so the screen actually changes.
fn flush(connection: &RustConnection) -> Result<(), DimmerError> {
    request(connection.flush())
}

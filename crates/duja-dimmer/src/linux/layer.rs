//! The Wayland overlay backend: one click-through layer surface per dimmed
//! output.
//!
//! ADR-0003's primary mechanism on the other Linux display server. The shape
//! follows [`super::overlay`] — a dedicated thread owns every surface,
//! [`apply`](duja_core::dimmer::Dimmer::apply) diffs with the pure
//! [`crate::plan`] kernel and executes the ops on that thread, and the decisions
//! that are data rather than windowing live in [`crate::linux_layer`] where every
//! lane can test them.
//!
//! Three things about Wayland make it a different program than the X11 one, and
//! each of them is a place a reader coming from that module will expect something
//! that is not here.
//!
//! # A surface is bound to an output, not placed at a rectangle
//!
//! `get_layer_surface` takes a `wl_output`. There is no root window and no global
//! coordinate space to put a window at, so the backend has to turn a
//! [`DimCommand`]'s rectangle back into the output it came from — see
//! [`take_output`] for why that is exact equality and why "already dimmed" is part
//! of the question.
//!
//! The consequence is that **`MoveResize` does nothing here**, which is the single
//! most surprising line in the module. A layer surface anchored to all four edges
//! with a delegated size *is* its output: when the output moves or changes mode,
//! the compositor sends a `configure` and the surface follows. What
//! [`Worker::perform`] still has to decide is whether the new rectangle is even
//! that output's any more, because if the user swapped two monitors around the
//! overlay now belongs somewhere else — and that is what the `placeable` argument
//! to [`plan_record`] carries.
//!
//! # Nothing fails at the call, and one class of failure kills everything
//!
//! A Wayland request is queued, never acknowledged. There is no `check` to wait on
//! the way X11 has one, so no per-request error to report — but a **protocol
//! error** terminates the whole connection, taking every other output's overlay
//! with it. That inverts where the care goes: instead of checking afterwards, the
//! two requests that can raise one are checked *before* they are sent
//! ([`crate::linux_layer::size_is_legal`] and
//! [`crate::linux_layer::viewport_destination`]), and everything else is fire and
//! forget.
//!
//! # The dim is one pixel
//!
//! `wl_shm` sizes a buffer in pixels, so covering a 4K output honestly would mean
//! a 33 MB framebuffer per output, rewritten on every slider sample. Instead the
//! backend attaches a **1x1 buffer and lets `wp_viewporter` scale it**, and all 256
//! of those buffers come out of one kilobyte written once at startup
//! ([`crate::linux_layer::dim_pool`]). Changing a dim level is therefore an attach
//! of a different existing buffer, not a write — which is also what makes the
//! shared mapping safe, since the compositor may be reading any of them at any
//! time and Duja never writes to the pool again.
//!
//! # Input must pass through
//!
//! The security invariant ADR-0003 states for every platform. Here it is an
//! **empty `wl_region` set as the surface's input region**, applied before the
//! first commit so there is no window in which the surface is up and opaque to
//! clicks. `set_keyboard_interactivity(none)` is the keyboard half and covers
//! nothing else; the two are separate requests because they are separate
//! questions.
//!
//! # Crash safety
//!
//! Surfaces are owned by the connection and the compositor destroys them when it
//! closes, so a crash cannot leave one on screen — the same as X11 overlays, and
//! unlike a gamma ramp.

use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::{Receiver as MpscReceiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use rustix::event::{PollFd, PollFlags, poll};
use rustix::fs::{MemfdFlags, memfd_create};
use rustix::io::Errno;
use rustix::pipe::{PipeFlags, pipe_with};
use tracing::warn;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy as _, QueueHandle, delegate_noop, globals,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, ZwlrLayerSurfaceV1,
};

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError, DisplayBounds};
use duja_core::id::StableDisplayId;

use crate::linux_layer::{
    ARGB_STRIDE, DIM_LEVELS, DIM_POOL_BYTES, PointerInput, dim_pool, dim_pool_offset,
    dimmer_surface, size_is_legal, take_output, viewport_destination,
};
use crate::linux_overlay::{Recorded, plan_record};
use crate::plan::{OverlayEntry, OverlayOp, apply_ops, plan_transition};

/// How long a caller waits for the worker's reply before degrading.
///
/// Same contract as the Windows and X11 backends': the caller is the Slint UI
/// thread, and a worker blocked writing to a compositor that has stopped reading
/// must not freeze it. A late reply lands on a dropped receiver and is discarded.
const REPLY_BUDGET: Duration = Duration::from_secs(2);

/// The `zwlr_layer_surface_v1` namespace every Duja overlay is created with.
///
/// Some compositors expose it to users for per-surface rules — Hyprland matches it
/// in `layerrule` — so it is a name someone might type. (sway does not: its
/// criteria are window attributes and there is no `layer` among them.) It is not an
/// identifier Duja reads back and nothing depends on its value.
const NAMESPACE: &str = "duja-dimmer";

/// The `wl_compositor` version `wl_surface.damage_buffer` arrives in.
///
/// The older `damage` takes surface-local coordinates, which under a viewport are
/// the *scaled* ones — so damaging the single pixel this backend actually changes
/// would mean describing it in output coordinates. Version 4 is from 2016 and
/// predates every compositor that has layer-shell, so requiring it costs nothing.
const COMPOSITOR_DAMAGE_BUFFER_VERSION: u32 = 4;

/// The `zxdg_output_manager_v1` versions this backend can use.
///
/// 1 is enough: `logical_position` and `logical_size` are there from the start.
/// The ceiling matches [`super::outputs`], which reads the same events.
const XDG_OUTPUT_VERSIONS: std::ops::RangeInclusive<u32> = 1..=3;

/// The `wl_output` version that added `release`.
///
/// Duja reads none of `wl_output`'s events, so this is the only reason to ask for
/// anything above 1: without it an unplugged monitor's proxy would have to be
/// leaked for the life of the process, there being no other way to tell the
/// compositor Duja is finished with it.
const WL_OUTPUT_RELEASE_VERSION: u32 = 3;

/// A command for the overlay worker.
enum Command {
    /// Apply a full desired state; reply with the diff-execution result.
    Apply(Vec<DimCommand>, SyncSender<Result<(), DimmerError>>),
    /// Remove every overlay; reply when done.
    Clear(SyncSender<Result<(), DimmerError>>),
    /// Stop the worker.
    Stop,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Apply(commands, _) => f.debug_tuple("Apply").field(&commands.len()).finish(),
            Command::Clear(_) => f.write_str("Clear"),
            Command::Stop => f.write_str("Stop"),
        }
    }
}

/// The Wayland software-dimming backend.
///
/// Construct with [`spawn`](Self::spawn). Drop (or [`shutdown`](Self::shutdown))
/// destroys every overlay and joins the worker.
pub struct WaylandDimmer {
    tx: Sender<Command>,
    /// The write end of the wake pipe.
    ///
    /// The worker sleeps in `poll` over the compositor's socket **and** this, so a
    /// command that arrives while nothing is happening on the wire still gets
    /// served. Every send is followed by a byte here.
    ///
    /// Closing it is a second, independent shutdown signal: the read end then
    /// reports `POLLHUP` and reads `0`, which [`drain_wake`] reports as
    /// [`Wake::Closed`] and the loop treats exactly like a `Stop`. That has to be
    /// handled rather than merely documented — `POLLHUP` on a pipe is level
    /// triggered and permanent, so a loop that woke on it and did nothing would
    /// spin a core for as long as the process lived.
    wake: Option<OwnedFd>,
    worker: Option<JoinHandle<()>>,
}

impl fmt::Debug for WaylandDimmer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaylandDimmer")
            .field("running", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

impl WaylandDimmer {
    /// Connect, bind everything an overlay needs, and start the worker.
    ///
    /// # Errors
    /// [`DimmerError::Unsupported`] when the compositor does not offer one of the
    /// three protocols an overlay is built from — `zwlr_layer_shell_v1` to place a
    /// surface above everything, `wp_viewporter` to fill it without a per-output
    /// framebuffer, and `zxdg_output_manager_v1` to know which output a display's
    /// rectangle is. [`crate::linux_caps`] reports exactly these three and in this
    /// order, so a refusal here and the report always name the same interface.
    /// Nothing *gates* this on that report — the app calls `spawn` directly and
    /// `probe_session` is `dujactl doctor`'s — which is why the two agreeing has to
    /// come from sharing the order rather than from one consulting the other.
    ///
    /// [`DimmerError::Os`] for a session that should have worked and did not: no
    /// compositor at `WAYLAND_DISPLAY`, a registry that answers without
    /// `wl_compositor` or `wl_shm`, or a kernel that refuses the anonymous file the
    /// dim levels live in.
    pub fn spawn() -> Result<Self, DimmerError> {
        let connection = Connection::connect_to_env()
            .map_err(|e| DimmerError::Os(format!("cannot reach the compositor: {e}")))?;
        let (globals, queue) = registry_queue_init::<Worker>(&connection)
            .map_err(|e| DimmerError::Os(format!("the compositor refused the registry: {e}")))?;
        let handle = queue.handle();

        // The two core interfaces. Their absence is not a capability Duja reports
        // on, because a compositor without them is not one anything can draw on.
        let compositor: WlCompositor = globals
            .bind(
                &handle,
                COMPOSITOR_DAMAGE_BUFFER_VERSION..=COMPOSITOR_DAMAGE_BUFFER_VERSION,
                (),
            )
            .map_err(|e| DimmerError::Os(format!("no usable wl_compositor: {e}")))?;
        let shm: WlShm = globals
            .bind(&handle, 1..=1, ())
            .map_err(|e| DimmerError::Os(format!("no usable wl_shm: {e}")))?;

        // The three the capability report names, refused in the same order it
        // names them so the backend and `dujactl doctor` cannot disagree about
        // which one is missing.
        let layer_shell: ZwlrLayerShellV1 = bind_reported(&globals, &handle)?;
        let viewporter: WpViewporter = bind_reported(&globals, &handle)?;
        let xdg_outputs: ZxdgOutputManagerV1 = globals
            .bind(&handle, XDG_OUTPUT_VERSIONS, ())
            .map_err(|_| DimmerError::Unsupported)?;

        let pool = dim_levels_pool(&shm, &handle)?;

        // A self-pipe rather than an eventfd, which is what `duja-platform`'s
        // uevent pump uses and for the same reason: the read end reaching EOF when
        // the write end drops is a second, independent shutdown signal, and an
        // eventfd has no equivalent of it.
        //
        // Non-blocking at both ends. The write end is touched from the UI thread,
        // where a full pipe would block on a worker that is already wedged — when
        // a full pipe in fact means a wake is *already* pending, so there is
        // nothing there to wait for. The read end for the mirror reason:
        // `drain_wake` reads until the pipe is empty, and on a blocking fd the read
        // that empties it would be followed by one that never returns.
        let (wake_read, wake_write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)
            .map_err(|e| DimmerError::Os(format!("cannot create the wake pipe: {e}")))?;

        let mut worker = Worker {
            handle: handle.clone(),
            compositor,
            layer_shell,
            viewporter,
            xdg_outputs,
            pool,
            levels: vec![None; DIM_LEVELS],
            outputs: Vec::new(),
            overlays: Vec::new(),
            current: Vec::new(),
            desired: Vec::new(),
            needs_replan: false,
            next_key: 0,
        };
        // Everything the compositor is advertising now. Anything that appears
        // later arrives as a registry `global` event and is bound the same way,
        // which is what makes a monitor plugged in mid-session dimmable.
        let registry = globals.registry().clone();
        let mut queue = queue;
        for global in globals.contents().clone_list() {
            worker.track_output(&registry, global.name, &global.interface, global.version);
        }
        // **One round trip, and it is not optional.** `get_xdg_output` is a request;
        // an output's logical rectangle arrives afterwards as an event, and until it
        // does `take_output` refuses that output and a `Create` for it records
        // nothing. Without this the first `apply` after `spawn` can dim nothing at
        // all and still answer `Ok`, because the worker holds applied state and has
        // nothing to re-drive from when the geometry lands.
        //
        // `wl_display.sync` is answered only after every request queued before it
        // and every event those requests generated, so one is enough by
        // construction — the same argument `super::outputs` makes for its own
        // enumeration pass. A failure here is a compositor that answered the
        // registry and then stopped talking, which is a fault rather than a session.
        queue
            .roundtrip(&mut worker)
            .map_err(|e| DimmerError::Os(format!("the compositor stopped answering: {e}")))?;

        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        let thread = std::thread::Builder::new()
            .name("duja-dimmer-wayland".to_owned())
            .spawn(move || run(&connection, queue, &mut worker, &rx, &wake_read))
            .map_err(|e| DimmerError::Os(format!("failed to spawn the overlay thread: {e}")))?;

        Ok(WaylandDimmer {
            tx,
            wake: Some(wake_write),
            worker: Some(thread),
        })
    }

    /// Destroy every overlay and stop the worker. Idempotent.
    ///
    /// # This join is bounded, unlike the other two backends'
    ///
    /// Windows and X11 both document an unbounded join, because a worker inside a
    /// blocking OS call against a wedged server never reaches its `Stop`. This
    /// worker has no such call: `wayland-backend` writes the socket with
    /// `MSG_DONTWAIT` (`rs/socket.rs`), so neither `flush` nor `read` can block on a
    /// compositor that has stopped reading, and the only wait in the loop is a
    /// `poll` this function's own `nudge` ends.
    ///
    /// The write end is closed **after** the join rather than before, which is the
    /// opposite of `duja-platform`'s uevent pump. Both work: there the close *is*
    /// the message, here the `Stop` is, and the pipe's EOF is the second, redundant
    /// one. Keeping the fd alive across the join is what makes the nudge above
    /// meaningful.
    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.tx.send(Command::Stop);
            self.nudge();
            let _ = worker.join();
        }
        // After the join, so the fd stays valid for the whole of the worker's life.
        self.wake = None;
    }

    /// Wake the worker out of `poll`.
    ///
    /// Best effort in both directions: a full pipe means a wake is already pending,
    /// and a closed one means a worker already returning.
    fn nudge(&self) {
        if let Some(wake) = self.wake.as_ref() {
            let _ = rustix::io::write(wake, &[0_u8]);
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
        // After the send, never before: the worker must find the command already
        // queued when the wake gets it out of `poll`, or it sleeps again with the
        // command still waiting and the caller times out on a backend that is
        // perfectly healthy.
        self.nudge();
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

impl Drop for WaylandDimmer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Dimmer for WaylandDimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        let sanitized: Vec<DimCommand> = commands.iter().map(DimCommand::sanitized).collect();
        self.dispatch(|reply| Command::Apply(sanitized, reply))
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        self.dispatch(Command::Clear)
    }
}

/// Bind one of the three interfaces the capability report names, mapping absence
/// to [`DimmerError::Unsupported`] rather than to a fault.
///
/// A compositor that does not implement layer-shell is an ordinary session, not a
/// broken one, and the caller disables software dimming with hardware control
/// intact either way — the two are distinguished because one is worth a log line
/// naming a cause and the other is not.
fn bind_reported<I>(
    globals: &globals::GlobalList,
    handle: &QueueHandle<Worker>,
) -> Result<I, DimmerError>
where
    I: wayland_client::Proxy + 'static,
    Worker: Dispatch<I, ()>,
{
    globals
        .bind(handle, 1..=1, ())
        .map_err(|_| DimmerError::Unsupported)
}

/// One anonymous file holding every dim level, and the `wl_shm_pool` over it.
///
/// Written once, before any buffer exists, and never again — see
/// [`crate::linux_layer::dim_pool`] for why that is the point rather than a
/// convenience. The `OwnedFd` is dropped as this returns: `wl_shm.create_pool`
/// gives the compositor its own duplicate, and the mapping outlives every fd on
/// this side.
fn dim_levels_pool(shm: &WlShm, handle: &QueueHandle<Worker>) -> Result<WlShmPool, DimmerError> {
    let fd = memfd_create("duja-dim-levels", MemfdFlags::CLOEXEC)
        .map_err(|e| DimmerError::Os(format!("cannot create the dim-level file: {e}")))?;
    let bytes = dim_pool();
    let mut written = 0_usize;
    while let Some(rest) = bytes.get(written..).filter(|rest| !rest.is_empty()) {
        match rustix::io::write(&fd, rest) {
            Ok(0) => {
                return Err(DimmerError::Os(
                    "the dim-level file stopped accepting bytes".to_owned(),
                ));
            }
            Ok(count) => written = written.saturating_add(count),
            Err(Errno::INTR) => {}
            Err(e) => {
                return Err(DimmerError::Os(format!("cannot write the dim levels: {e}")));
            }
        }
    }
    let size = i32::try_from(DIM_POOL_BYTES)
        .map_err(|_| DimmerError::Os("the dim-level pool does not fit an i32".to_owned()))?;
    Ok(shm.create_pool(fd.as_fd(), size, handle, ()))
}

/// One `wl_output` and everything known about where it is.
struct Tracked {
    /// The registry name it was advertised under, which is what `global_remove`
    /// carries and so the only stable key a hot-unplug can be matched on.
    global: u32,
    output: WlOutput,
    xdg: ZxdgOutputV1,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
}

impl Tracked {
    /// Where this output sits, or `None` until both events have arrived.
    ///
    /// Logical coordinates, which is the only unit that survives fractional
    /// scaling — the same reason [`super::outputs`] reads these events rather than
    /// `wl_output`'s own mode and integer scale.
    fn logical(&self) -> Option<DisplayBounds> {
        let (x, y) = self.position?;
        let (width, height) = self.size?;
        let width = u32::try_from(width).ok()?;
        let height = u32::try_from(height).ok()?;
        Some(DisplayBounds::new(x, y, width, height))
    }
}

/// One layer surface and which display it dims.
struct Overlay {
    /// This overlay's dispatch key, carried as the `zwlr_layer_surface_v1` user
    /// data so a `configure` can find its way back here.
    key: u32,
    id: StableDisplayId,
    /// The registry name of the output this surface is bound to. A `wl_output`
    /// proxy would do as well until the output is unplugged, at which point the
    /// name is the only thing `global_remove` gives to match on.
    output: u32,
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: WpViewport,
    alpha: u8,
    /// Whether the first `configure` has been acked and a buffer attached.
    ///
    /// Until it is, *"any attempts by a client to attach or manipulate a buffer
    /// prior to the first `layer_surface.configure` call must also be treated as
    /// errors"* — so an alpha change before then only updates `alpha`, and the
    /// configure handler picks it up.
    configured: bool,
}

/// The thread that owns every layer surface, and the dispatch state for the queue.
struct Worker {
    handle: QueueHandle<Worker>,
    compositor: WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    viewporter: WpViewporter,
    /// Kept for the life of the connection rather than used once at startup: a
    /// monitor plugged in mid-session needs a `zxdg_output_v1` of its own, and
    /// destroying the manager would take every existing one with it.
    xdg_outputs: ZxdgOutputManagerV1,
    pool: WlShmPool,
    /// One 1x1 `wl_buffer` per dim level, created on first use and then kept.
    ///
    /// Never destroyed before shutdown, which is what removes the need to track
    /// `wl_buffer.release` at all: a buffer this backend will never write to again
    /// is safe to leave attached to any number of surfaces for any length of time.
    levels: Vec<Option<WlBuffer>>,
    outputs: Vec<Tracked>,
    overlays: Vec<Overlay>,
    current: Vec<OverlayEntry>,
    /// The last state a caller asked for, kept so it can be re-planned.
    ///
    /// `current` alone is not enough. A `Create` for an output whose geometry has
    /// not arrived records **nothing** — correctly, since no surface was made — so
    /// nothing in `current` remembers that a display wanted dimming and did not get
    /// it. Without this the monitor stays dark-less until some later `apply`
    /// happens to come along, and on a hot-plug there may not be one.
    desired: Vec<DimCommand>,
    /// Set when the compositor said something that changes what is placeable.
    ///
    /// Only output geometry does that today. Re-planning is free when nothing
    /// moved: `plan_transition` diffs `desired` against `current` and emits no ops.
    needs_replan: bool,
    next_key: u32,
}

impl Worker {
    /// Bind a `wl_output` the registry is advertising, and ask for its geometry.
    ///
    /// Called for the compositor's initial list and again for every later `global`
    /// event, which is what makes a monitor plugged in mid-session dimmable
    /// without restarting the backend.
    fn track_output(&mut self, registry: &WlRegistry, name: u32, interface: &str, version: u32) {
        if interface != WlOutput::interface().name {
            return;
        }
        if self.outputs.iter().any(|known| known.global == name) {
            return;
        }
        // `wl_output`'s own events are not read here — the logical rectangle comes
        // from `xdg_output` — so the proxy exists only to name the output in
        // `get_layer_surface` and `get_xdg_output`. The version is capped at what
        // the compositor advertised, because binding above that is a protocol
        // error, and asked for above 1 at all only because `release` does not exist
        // below version 3; see `release`.
        let output = registry.bind::<WlOutput, _, _>(
            name,
            version.min(WL_OUTPUT_RELEASE_VERSION),
            &self.handle,
            (),
        );
        let xdg = self.xdg_outputs.get_xdg_output(&output, &self.handle, name);
        self.outputs.push(Tracked {
            global: name,
            output,
            xdg,
            position: None,
            size: None,
        });
    }

    /// Forget an output the compositor has taken away, and tear down anything
    /// dimming it.
    ///
    /// The compositor also sends `closed` on that output's layer surface, and the
    /// two arrive in an order the protocol deliberately does not fix — `wl_registry`
    /// keeps a removed global's objects valid *"to avoid races between the global
    /// going away and a client sending a request to it"*. So both paths have to be
    /// complete on their own, and both go through [`Worker::retire`] for that
    /// reason. Whichever runs second finds nothing, which is not a double free.
    fn drop_output(&mut self, name: u32) {
        while let Some(index) = self.overlays.iter().position(|o| o.output == name) {
            self.retire(index);
        }
        if let Some(index) = self.outputs.iter().position(|t| t.global == name) {
            let tracked = self.outputs.swap_remove(index);
            release(&tracked);
        }
    }

    /// Diff, execute, and record **what actually happened**.
    ///
    /// # There is no failure to report from here
    ///
    /// Unlike the X11 twin, which can be told a `CreateWindow` was refused. A
    /// Wayland request is queued and never acknowledged, and the one thing that
    /// *does* go wrong — a protocol error, or the compositor closing the socket —
    /// is fatal to the whole connection, which ends this thread. The caller learns
    /// about that from the channel: [`WaylandDimmer::dispatch`] gets a send error
    /// or a reply timeout and reports [`DimmerError::Backend`].
    ///
    /// A `dead` flag checked here would be unreachable code pretending to be a
    /// guard. There is no state in which this worker is running and its connection
    /// is not.
    fn apply(&mut self, commands: Vec<DimCommand>) {
        self.desired = commands;
        self.replan();
    }

    /// Re-plan the last desired state against the screen as it is now.
    ///
    /// Called for a fresh `apply` and again whenever the compositor tells us
    /// something that changes what is placeable, which is how a display whose
    /// output had no geometry yet gets dimmed at all rather than waiting for a
    /// later command that may never come.
    fn replan(&mut self) {
        self.needs_replan = false;
        // Moved out and back rather than cloned: `execute` needs `&mut self`, and
        // the desired state is not what it mutates.
        let desired = std::mem::take(&mut self.desired);
        self.plan_against(&desired);
        self.desired = desired;
    }

    fn plan_against(&mut self, commands: &[DimCommand]) {
        let ops = plan_transition(&self.current, commands);
        let applied = self.execute(&ops);
        // Folded from `applied`, never from `ops`. The two diverge whenever an op
        // is skipped — a rectangle no free output has — and recording an op that
        // did not happen leaves `current` describing a surface that does not
        // exist, after which the planner never emits `Create` for that display
        // again and it cannot be dimmed for the rest of the session.
        self.current = apply_ops(&self.current, &applied);
    }

    /// Run the planner's ops.
    ///
    /// Returns the ops that genuinely took effect, which is what the caller folds
    /// into its record of the screen. **What each op does, and what it records, is
    /// decided by [`plan_record`]** — a pure rule tested on every lane, because
    /// getting it wrong is how a display becomes permanently undimmable and no
    /// test of the windowing itself can run.
    ///
    /// There is no early return on failure, unlike the X11 twin: a Wayland request
    /// cannot be refused individually. It is queued and either the whole connection
    /// survives or none of it does.
    fn execute(&mut self, ops: &[OverlayOp]) -> Vec<OverlayOp> {
        let mut applied: Vec<OverlayOp> = Vec::new();
        for op in ops {
            let id = op_id(op);
            // Wayland's answer to "can a surface go there". A `Create` needs an
            // output nothing else is already dimming; a `MoveResize` needs its own
            // output to still be the one that rectangle names, because if the user
            // rearranged their monitors the overlay now belongs to a different one
            // and has to be rebuilt there.
            let output = match op {
                OverlayOp::Create { bounds, .. } => self.free_output(*bounds),
                OverlayOp::MoveResize { .. }
                | OverlayOp::SetAlpha { .. }
                | OverlayOp::Destroy { .. } => None,
            };
            let placeable = match op {
                OverlayOp::Create { .. } => output.is_some(),
                OverlayOp::MoveResize { id, bounds } => self.still_on_its_output(id, *bounds),
                OverlayOp::SetAlpha { .. } | OverlayOp::Destroy { .. } => true,
            };
            match plan_record(op, self.find(id).is_some(), placeable) {
                Recorded::Nothing => {}
                // Either there is no surface to act on, or the op would put one
                // where this backend cannot put it. Destroying is a no-op in the
                // first case; recording the destroy is what drops the stale entry
                // so the next plan emits a fresh `Create`.
                Recorded::DestroyInstead => {
                    self.destroy(id);
                    applied.push(OverlayOp::Destroy { id: id.clone() });
                }
                // Recorded **only if it happened**. `plan_record` has said it is
                // doable and every arm below is infallible under that promise, so
                // this can only differ from `true` on a bug in that pairing — but
                // the cost of getting it wrong is a `current` entry describing a
                // surface that does not exist, which is a permanently undimmable
                // display. The guard belongs on this side of the record.
                Recorded::AsPlanned => {
                    if self.perform(op, output) {
                        applied.push(op.clone());
                    }
                }
            }
        }
        applied
    }

    /// Do exactly what an op says, having already been told it is doable.
    ///
    /// `output` is the index [`plan_record`]'s `placeable` was computed from, so
    /// for a `Create` that reached this arm it is always `Some`. Returns whether the
    /// op actually happened, which the caller records — a `None` here, or a refused
    /// `create`, would be a bug in that pairing, and answering `false` keeps it a
    /// display that is retried next plan instead of one the record has silently
    /// written off.
    fn perform(&mut self, op: &OverlayOp, output: Option<usize>) -> bool {
        match op {
            OverlayOp::Create { id, alpha, .. } => match output {
                Some(index) => self.create(id.clone(), index, *alpha),
                None => false,
            },
            // **Nothing.** A layer surface anchored to all four edges of its output
            // with a delegated size follows that output on its own: the compositor
            // sends a `configure` with the new size and the handler resets the
            // viewport. The only thing that could need doing is moving the surface
            // to a *different* output, and `placeable` has already turned that case
            // into a destroy-and-recreate before reaching here.
            OverlayOp::MoveResize { .. } => true,
            OverlayOp::SetAlpha { id, alpha } => {
                self.set_alpha(id, *alpha);
                true
            }
            OverlayOp::Destroy { id } => {
                self.destroy(id);
                true
            }
        }
    }

    /// The output a display's rectangle names, if one is free.
    ///
    /// The rule is [`take_output`]; this is only the part that cannot be pure —
    /// reading each output's logical geometry off the tracked list, and working out
    /// which of them the live overlays already hold.
    fn free_output(&self, wanted: DisplayBounds) -> Option<usize> {
        let logical: Vec<Option<DisplayBounds>> =
            self.outputs.iter().map(Tracked::logical).collect();
        let taken: Vec<usize> = self
            .overlays
            .iter()
            .filter_map(|overlay| {
                self.outputs
                    .iter()
                    .position(|tracked| tracked.global == overlay.output)
            })
            .collect();
        take_output(wanted, &logical, &taken)
    }

    /// Whether this display's overlay is still on the output its new rectangle
    /// names.
    ///
    /// Deliberately strict: anything other than an exact match on the output this
    /// surface is already bound to means destroy-and-recreate. A lenient version —
    /// "keep it unless some *other* output matches" — would never flicker, and
    /// would leave an overlay dimming the wrong monitor for as long as nothing else
    /// disturbed it. [`run`] explains the cost that buys and how the loop's
    /// ordering keeps it rare.
    fn still_on_its_output(&self, id: &StableDisplayId, wanted: DisplayBounds) -> bool {
        let Some(overlay) = self.find(id) else {
            return false;
        };
        self.outputs
            .iter()
            .find(|tracked| tracked.global == overlay.output)
            .and_then(Tracked::logical)
            == Some(wanted)
    }

    /// Create one overlay: a click-through layer surface covering `output`.
    ///
    /// Nothing is attached here. The protocol requires an initial commit with no
    /// buffer, and forbids touching one before the first `configure` — so this
    /// leaves the surface unmapped and
    /// [`Worker::configured`](Worker::configured) finishes the job when that event
    /// arrives.
    fn create(&mut self, id: StableDisplayId, output: usize, alpha: u8) -> bool {
        let Some(tracked) = self.outputs.get(output) else {
            return false;
        };
        let wanted = dimmer_surface();
        // Checked before the request that would raise it, because `invalid_size` is
        // a protocol error and a protocol error takes every *other* output's
        // overlay down with it. The value is a constant, so this can only fire on
        // an edit to `dimmer_surface` — which is exactly the edit that must not
        // reach a user's screen.
        if !size_is_legal(wanted.anchor, wanted.width, wanted.height) {
            warn!("refusing to create a layer surface with an illegal anchor/size pair");
            return false;
        }
        let Ok(layer) = zwlr_layer_shell_v1::Layer::try_from(wanted.layer) else {
            warn!(layer = wanted.layer, "not a layer this protocol has");
            return false;
        };
        let Some(anchor) = zwlr_layer_surface_v1::Anchor::from_bits(wanted.anchor) else {
            warn!(anchor = wanted.anchor, "not an anchor this protocol has");
            return false;
        };

        let output_proxy = tracked.output.clone();
        let output_global = tracked.global;
        let surface = self.compositor.create_surface(&self.handle, ());

        // ADR-0003's security invariant, and it is set **before** the first commit
        // for a reason: an input region is double-buffered, so a surface committed
        // without one and given one afterwards is a full-screen click-eater for
        // however long that takes. The region is destroyed straight away — it is
        // copied into the surface's pending state, not referenced.
        match wanted.pointer_input {
            PointerInput::Empty => {
                let region = self.compositor.create_region(&self.handle, ());
                surface.set_input_region(Some(&region));
                region.destroy();
            }
            PointerInput::Inherited => {}
        }

        let key = self.next_key;
        self.next_key = self.next_key.wrapping_add(1);

        let layer_surface = self.layer_shell.get_layer_surface(
            &surface,
            Some(&output_proxy),
            layer,
            NAMESPACE.to_owned(),
            &self.handle,
            key,
        );
        layer_surface.set_anchor(anchor);
        layer_surface.set_exclusive_zone(wanted.exclusive_zone);
        layer_surface.set_size(wanted.width, wanted.height);
        // The keyboard half of "do not take input". Separate from the input region
        // because it is a separate question: `none` alone still swallows clicks.
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        let viewport = self.viewporter.get_viewport(&surface, &self.handle, ());

        // The initial commit the protocol requires: no buffer, and the compositor
        // answers with the `configure` that says how big this output is.
        surface.commit();

        self.overlays.push(Overlay {
            key,
            id,
            output: output_global,
            surface,
            layer: layer_surface,
            viewport,
            alpha,
            configured: false,
        });
        true
    }

    /// Finish mapping an overlay, or re-size one whose output changed mode.
    ///
    /// Both are the same event and the same work, which is why there is one
    /// function: ack, size the viewport to what the compositor just assigned,
    /// attach the current dim level, commit.
    fn configured(&mut self, key: u32, serial: u32, width: u32, height: u32) {
        let Some(alpha) = self
            .overlays
            .iter()
            .find(|overlay| overlay.key == key)
            .map(|overlay| overlay.alpha)
        else {
            return;
        };
        // Before the borrow below, because creating a level takes `&mut self`.
        let buffer = self.level(alpha);
        let Some(overlay) = self.overlays.iter_mut().find(|overlay| overlay.key == key) else {
            return;
        };
        overlay.layer.ack_configure(serial);
        let Some((width, height)) = viewport_destination(width, height) else {
            // The compositor answered "you decide your own size", and this backend
            // cannot — a dim has no natural extent. Passing the zero on would be
            // `wp_viewport.bad_value`, which is fatal to the connection, so the
            // surface stays unmapped and this output is simply not dimmed.
            warn!(key, "the compositor left this surface's size to us");
            return;
        };
        overlay.viewport.set_destination(width, height);
        overlay.surface.attach(Some(&buffer), 0, 0);
        // The whole buffer, which is one pixel. Buffer coordinates rather than
        // surface ones precisely because the viewport scales between the two.
        overlay.surface.damage_buffer(0, 0, 1, 1);
        overlay.surface.commit();
        overlay.configured = true;
    }

    /// Change one overlay's dim level.
    ///
    /// An attach of a different pre-existing buffer, never a write: see
    /// [`crate::linux_layer::dim_pool`].
    fn set_alpha(&mut self, id: &StableDisplayId, alpha: u8) {
        let Some(key) = self
            .overlays
            .iter()
            .find(|overlay| &overlay.id == id)
            .map(|overlay| overlay.key)
        else {
            return;
        };
        let buffer = self.level(alpha);
        let Some(overlay) = self.overlays.iter_mut().find(|overlay| overlay.key == key) else {
            return;
        };
        overlay.alpha = alpha;
        // Before the first `configure`, attaching a buffer is a protocol error.
        // Recording the level and returning is not a lost update: `configured`
        // reads `alpha` when it runs, so the surface maps at the current level
        // rather than the one it was created with.
        if overlay.configured {
            overlay.surface.attach(Some(&buffer), 0, 0);
            overlay.surface.damage_buffer(0, 0, 1, 1);
            overlay.surface.commit();
        }
    }

    /// The 1x1 buffer for one dim level, created on first use.
    fn level(&mut self, alpha: u8) -> WlBuffer {
        let index = usize::from(alpha);
        if let Some(Some(existing)) = self.levels.get(index) {
            return existing.clone();
        }
        let buffer = self.pool.create_buffer(
            dim_pool_offset(alpha),
            1,
            1,
            ARGB_STRIDE,
            // `argb8888`, not `xrgb8888`. The protocol says every renderer
            // *should* support both and that any other format is optional, so this
            // is the pair that can be assumed — and the wrong one of the two is a
            // fully opaque black rectangle over the monitor at every dim level,
            // the Wayland shape of the X11 hazard `choose_argb_visual` exists for.
            wl_shm::Format::Argb8888,
            &self.handle,
            (),
        );
        if let Some(slot) = self.levels.get_mut(index) {
            *slot = Some(buffer.clone());
        }
        buffer
    }

    fn find(&self, id: &StableDisplayId) -> Option<&Overlay> {
        self.overlays.iter().find(|overlay| &overlay.id == id)
    }

    fn destroy(&mut self, id: &StableDisplayId) {
        let Some(index) = self.overlays.iter().position(|overlay| &overlay.id == id) else {
            return;
        };
        self.retire(index);
    }

    /// Take one overlay down and forget the planner ever placed it.
    ///
    /// **Dropping the `current` entry is the half that is easy to leave out**, and
    /// it is the half that matters. An overlay removed from `overlays` while its
    /// entry stays in `current` is a display the planner believes is already dimmed
    /// at exactly the level it wants — so on an unplug and replug at the same
    /// rectangle and the same level, `plan_transition` emits **no op at all** and
    /// that monitor is undimmed until the user next moves the slider.
    ///
    /// Harmless on the planner's own path, where `execute` records the `Destroy`
    /// and `apply_ops` would have dropped the entry a moment later. Load-bearing on
    /// the two asynchronous paths — `closed` and `global_remove` — where there is
    /// no op being recorded and nothing else would ever drop it.
    ///
    /// `swap_remove` keeps this indexing-free: lookup is by id, and stacking is the
    /// compositor's, so the order of `overlays` means nothing.
    fn retire(&mut self, index: usize) {
        if index >= self.overlays.len() {
            return;
        }
        let overlay = self.overlays.swap_remove(index);
        Worker::forget(&overlay);
        self.current.retain(|entry| entry.id != overlay.id);
    }

    /// Destroy one overlay's three objects, the `wl_surface` last.
    ///
    /// **Only that last part is required**, and it is required by the core
    /// protocol rather than by either extension: a `wl_surface` destroyed while it
    /// still has a role object raises `defunct_role_object`. `wp_viewport`'s
    /// `no_surface` error does not apply here in either order, because the protocol
    /// excepts the one request this sends: `no_surface` is raised by all of that
    /// interface's requests *"except 'destroy'"*, and `zwlr_layer_surface_v1` has
    /// no such error at all — its five are `invalid_surface_state`,
    /// `invalid_size`, `invalid_anchor`, `invalid_keyboard_interactivity` and
    /// `invalid_exclusive_edge`. Viewport first is tidiness; surface last is the
    /// rule.
    fn forget(overlay: &Overlay) {
        overlay.viewport.destroy();
        overlay.layer.destroy();
        overlay.surface.destroy();
    }

    /// Take every overlay down.
    ///
    /// Infallible, and not because the failures are swallowed: destroying a proxy
    /// is a queued request that cannot be refused individually, so there is no
    /// outcome for a caller to branch on. The X11 twin returns a `Result` because
    /// an X `DestroyWindow` can come back as a protocol error at the flush.
    fn destroy_all(&mut self) {
        while let Some(overlay) = self.overlays.pop() {
            Worker::forget(&overlay);
        }
        self.current.clear();
        self.desired.clear();
        self.needs_replan = false;
    }

    /// Give back everything this backend holds on the compositor.
    ///
    /// Belt and braces: the compositor destroys all of it when the connection
    /// closes a moment later. It is done explicitly so that a future caller which
    /// keeps the connection for something else does not inherit a screen full of
    /// surfaces.
    fn teardown(&mut self) {
        self.destroy_all();
        for level in self.levels.drain(..).flatten() {
            level.destroy();
        }
        self.pool.destroy();
        for tracked in self.outputs.drain(..) {
            release(&tracked);
        }
        // The two globals that *can* be given back at the versions bound here.
        // `wl_compositor.release` is version 7, `wl_shm.release` version 2 and
        // `zwlr_layer_shell_v1.destroy` version 3, so those three stay — sending one
        // of them would be the unknown opcode this module refuses everywhere else.
        self.viewporter.destroy();
        self.xdg_outputs.destroy();
    }
}

/// The worker loop: dispatch compositor events, serve commands, sleep on both.
///
/// # Why events are parsed before commands are served
///
/// Not latency — it costs a little — but *staleness*. A command carries rectangles
/// the app read from its own, separate connection, and the worker decides what to
/// do with them by comparing against each output's logical geometry as it last
/// heard it. Change a monitor's resolution and both of those move: the compositor
/// sends this connection new `xdg_output` events at the same moment the app starts
/// re-enumerating on its own. Serving first would compare a new rectangle against
/// geometry from before the change, conclude the display has moved to another
/// output, and destroy a perfectly good overlay to rebuild it one apply later.
///
/// This narrows that window to almost nothing rather than closing it — the app
/// could still get there first. What is left is a visible flicker on a resolution
/// change, which is why the ordering is here and not the rule in
/// [`Worker::still_on_its_output`]: making that rule *lenient* instead would leave
/// an overlay dimming the wrong monitor indefinitely, and a wrong monitor is worse
/// than a flicker.
///
/// # Why no wake can be lost
///
/// Commands are drained **before** the read is armed, and
/// [`WaylandDimmer::nudge`] writes to the pipe **after** the send. So a command
/// queued at any point is either found by the drain or makes the following `poll`
/// return immediately; there is no window between the two that swallows one.
fn run(
    connection: &Connection,
    mut queue: EventQueue<Worker>,
    worker: &mut Worker,
    rx: &Receiver<Command>,
    wake: &OwnedFd,
) {
    // Borrowed once, outside the loop: `PollFd::new` takes a reference, so building
    // these inline would borrow a temporary that dies before `poll` sees it.
    let connection_fd = connection.as_fd();
    let wake_fd = wake.as_fd();
    // Whether the last flush ran out of socket buffer. See the flush below for why
    // that is a wait rather than a failure, and why the poll has to ask for
    // writability while it is set. Uninitialised on purpose: every turn assigns it
    // before reading it, and an initialiser here would be a value that is never
    // used and could go stale against the match below.
    let mut flush_pending;
    loop {
        // Everything already read off the socket: the `configure` that maps a
        // surface created last turn, and the output geometry the commands below are
        // about to be judged against.
        //
        // A failure here is a protocol error or a closed socket, and either way
        // every proxy this worker holds is already dead. There is nothing to give
        // back and nobody to tell — the caller finds out when its next command
        // lands on a dropped receiver.
        if queue.dispatch_pending(worker).is_err() {
            return;
        }
        // Anything those events made placeable that was not before. Must come after
        // the dispatch and before the commands, for the ordering reason above.
        if worker.needs_replan {
            worker.replan();
        }
        match serve_pending(worker, rx) {
            Turn::Continue => {}
            Turn::Stop => {
                worker.teardown();
                let _ = queue.flush();
                return;
            }
        }
        // One flush for both of the above: the events may have queued requests
        // (a `configure` acks and commits) and so may the commands.
        //
        // **`WouldBlock` here is a wait, not a failure**, and treating it as one
        // would be the worst bug in this file. `wayland-backend` sends with
        // `MSG_DONTWAIT` and deliberately does *not* record a `WouldBlock` as the
        // connection's `last_error` (`store_if_not_wouldblock_and_return_error`) —
        // that asymmetry is the library saying "the kernel buffer is full, poll for
        // writability and call me again". The unsent bytes stay in the outgoing
        // buffer, so retrying resends exactly what is left.
        //
        // A compositor that stops draining its client socket for a moment — frozen,
        // stopped, swapping, a long GPU stall — is enough to fill ~208 KiB of
        // `AF_UNIX` buffer. Returning here would end the worker, drop `rx`, and make
        // every later apply fail with `Backend` for the rest of the session: a
        // transient stall would permanently kill software dimming. So the flag is
        // set and the poll below asks for writability too.
        match queue.flush() {
            Ok(()) => flush_pending = false,
            Err(e) if would_block(&e) => flush_pending = true,
            Err(_) => return,
        }
        // Arming the read is what makes the wait safe: between here and `poll`,
        // anything the compositor sends lands in the socket and makes it readable
        // rather than being parsed and forgotten.
        let Some(guard) = queue.prepare_read() else {
            // Events arrived while flushing. Round again and dispatch them.
            continue;
        };
        // `OUT` only while a flush is waiting for room. Asking for it
        // unconditionally would make `poll` return immediately every time — the
        // socket is almost always writable — and spin this thread on a core.
        let connection_interest = if flush_pending {
            PollFlags::IN | PollFlags::OUT
        } else {
            PollFlags::IN
        };
        let mut fds = [
            PollFd::new(&connection_fd, connection_interest),
            PollFd::new(&wake_fd, PollFlags::IN),
        ];
        // No timeout: everything this thread does is a reaction to one of these two
        // descriptors, and a tick would only add wakeups to a process whose
        // idle-wakeup budget is a stated product property.
        match poll(&mut fds, None) {
            Ok(_) => {}
            // A signal interrupting the wait is not an error.
            Err(Errno::INTR) => continue,
            // Not the compositor's fault and not necessarily fatal to the
            // connection, so unlike the paths above this one still hands the
            // surfaces back before going.
            Err(_) => {
                worker.teardown();
                let _ = queue.flush();
                return;
            }
        }
        let (connection_ready, wake_ready) = match (fds.first(), fds.get(1)) {
            (Some(connection_fd), Some(wake_fd)) => (connection_fd.revents(), wake_fd.revents()),
            _ => return,
        };
        let ready = PollFlags::IN | PollFlags::HUP | PollFlags::ERR;
        // **The socket is read first, even when a command is also waiting**, and
        // that ordering is the whole of the argument above rather than a
        // preference. Every command arrives with a nudge, so both descriptors ready
        // at once is the *normal* case, not a rare one — and it is exactly the case
        // the ordering exists for: a resolution change makes the compositor's new
        // geometry and the app's new rectangles ready together. Cancelling the read
        // to serve the command first would leave those bytes in the socket,
        // `dispatch_pending` would find an empty queue next turn, and the command
        // would be judged against geometry from before the change — destroying a
        // working overlay to rebuild it, which is precisely what this ordering is
        // written to prevent.
        //
        // Dropping the guard is what cancels the read; a guard carried across the
        // loop would deadlock the next `prepare_read`.
        if (connection_ready & ready).is_empty() {
            drop(guard);
        } else if let Err(e) = guard.read() {
            // `WouldBlock` is another queue's reader having taken the bytes first.
            // There is only one queue here, so it is unreachable rather than
            // expected — and treating it as fatal would end the thread on a
            // condition that resolves itself.
            if !would_block(&e) {
                warn!(%e, "the compositor connection ended");
                return;
            }
        }
        if !(wake_ready & ready).is_empty() && drain_wake(wake) == Wake::Closed {
            worker.teardown();
            let _ = queue.flush();
            return;
        }
    }
}

/// Whether the worker should keep going after draining the command channel.
enum Turn {
    Continue,
    Stop,
}

/// Serve every command already waiting, without blocking.
fn serve_pending(worker: &mut Worker, rx: &Receiver<Command>) -> Turn {
    loop {
        match rx.try_recv() {
            Ok(Command::Apply(commands, reply)) => {
                worker.apply(commands);
                let _ = reply.send(Ok(()));
            }
            Ok(Command::Clear(reply)) => {
                // Always allowed, even after the connection died: there is then
                // nothing to clear and the caller gets the state it asked for.
                //
                // `destroy_all` drops the desired state as well as the surfaces, and
                // that is the whole point: a `clear` that left it standing would be
                // undone by the next output-geometry event, which re-plans.
                worker.destroy_all();
                let _ = reply.send(Ok(()));
            }
            Err(TryRecvError::Empty) => return Turn::Continue,
            // An explicit stop, or every sender gone so nothing will ever ask for
            // anything again. `shutdown` produces both and either alone is enough.
            Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return Turn::Stop,
        }
    }
}

/// What draining the wake pipe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wake {
    /// Some number of nudges, now consumed.
    Nudged,
    /// The write end is gone. [`WaylandDimmer`] has been dropped without its
    /// `Stop` being served, and this is the second, independent shutdown signal.
    Closed,
}

/// Consume the wake bytes so the pipe does not stay readable.
///
/// Any number of nudges collapse into one turn of the loop, which is the intent:
/// the channel is drained in full each time, so a second byte would only buy an
/// extra empty pass.
///
/// **`Closed` has to end the loop, not merely be reported.** `POLLHUP` on a pipe
/// whose write end is gone is level triggered and permanent, so a caller that woke
/// on it, read its zero bytes and went back to `poll` would be handed the same
/// readiness immediately and forever — a hot loop with no blocking syscall in it,
/// burning a core for the life of the process and hanging any `join`.
fn drain_wake(wake: &OwnedFd) -> Wake {
    let mut sink = [0_u8; 64];
    loop {
        match rustix::io::read(wake, &mut sink[..]) {
            // A full buffer may not be all of it; go round again.
            Ok(count) if count == sink.len() => {}
            // End of file: every write end has been closed.
            Ok(0) => return Wake::Closed,
            Err(Errno::INTR) => {}
            // A short read, or `EAGAIN` on an empty non-blocking pipe. The latter
            // is the ordinary exit.
            Ok(_) | Err(_) => return Wake::Nudged,
        }
    }
}

/// Give an output's two proxies back.
///
/// `wl_output.release` is a version 3 request. Sending it to a version 1 or 2
/// proxy is an unknown opcode, and an unknown opcode is a protocol error that
/// takes the whole connection down — every other monitor's overlay included — so
/// the version the bind actually got is what is checked, not the one it asked for.
fn release(tracked: &Tracked) {
    tracked.xdg.destroy();
    if tracked.output.version() >= WL_OUTPUT_RELEASE_VERSION {
        tracked.output.release();
    }
}

/// Whether a read failed because there was nothing there rather than because the
/// connection is gone.
fn would_block(error: &wayland_client::backend::WaylandError) -> bool {
    match error {
        wayland_client::backend::WaylandError::Io(e) => e.kind() == std::io::ErrorKind::WouldBlock,
        wayland_client::backend::WaylandError::Protocol(_) => false,
    }
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

/// The registry, which stays bound for the life of the connection.
///
/// Unlike [`super::wayland`]'s one-shot probe, this backend cares what happens
/// *after* the first round trip: an output appearing is a monitor that can now be
/// dimmed, and one disappearing is a surface that has to go.
impl Dispatch<WlRegistry, GlobalListContents> for Worker {
    fn event(
        worker: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _globals: &GlobalListContents,
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => worker.track_output(registry, name, &interface, version),
            wl_registry::Event::GlobalRemove { name } => worker.drop_output(name),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for Worker {
    fn event(
        worker: &mut Self,
        _surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        key: &u32,
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => worker.configured(*key, serial, width, height),
            // *"Further changes to the surface will be ignored. The client should
            // destroy the resource"* — so this is not a hint. Dropping the record
            // as well as the objects is what lets the display be dimmed again if
            // its output comes back.
            zwlr_layer_surface_v1::Event::Closed => {
                if let Some(index) = worker.overlays.iter().position(|o| o.key == *key) {
                    let overlay = worker.overlays.swap_remove(index);
                    let id = overlay.id.clone();
                    Worker::forget(&overlay);
                    worker.current.retain(|entry| entry.id != id);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for Worker {
    fn event(
        worker: &mut Self,
        _output: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        global: &u32,
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        let Some(tracked) = worker
            .outputs
            .iter_mut()
            .find(|tracked| tracked.global == *global)
        else {
            return;
        };
        let before = tracked.logical();
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => tracked.position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                tracked.size = Some((width, height));
            }
            _ => return,
        }
        // An output that has just become placeable, or moved. Either way the last
        // desired state may now be satisfiable where it was not, so ask the loop to
        // re-plan; `Worker::replan` is a no-op when nothing has actually changed.
        if tracked.logical() != before {
            worker.needs_replan = true;
        }
    }
}

// No events, or none this backend reads. `wl_surface` and `wl_output` both have
// some — enter/leave, scale, mode — and none of them changes what a full-output
// dim does: the layer surface's size comes from `configure` and its position from
// the output it is bound to.
// `ignore` on every one of them, including the interfaces that have no events at
// all: the plain form of this macro generates an `unreachable!()` body, and a
// panic path on a thread nothing supervises is worse than an empty one, however
// unreachable it looks from here.
delegate_noop!(Worker: ignore WlCompositor);
delegate_noop!(Worker: ignore WlShmPool);
delegate_noop!(Worker: ignore WpViewporter);
delegate_noop!(Worker: ignore WpViewport);
delegate_noop!(Worker: ignore ZwlrLayerShellV1);
delegate_noop!(Worker: ignore ZxdgOutputManagerV1);
delegate_noop!(Worker: ignore WlShm);
delegate_noop!(Worker: ignore WlSurface);
delegate_noop!(Worker: ignore WlOutput);
delegate_noop!(Worker: ignore wayland_client::protocol::wl_region::WlRegion);
// `release` is the one event here, and this backend never needs it: a dim level's
// buffer is created once and never written to again, so there is nothing to wait
// for before reusing it.
delegate_noop!(Worker: ignore WlBuffer);

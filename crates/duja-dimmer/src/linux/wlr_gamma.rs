//! The opt-in gamma path on Wayland (`zwlr_gamma_control_v1`), and why it needs
//! none of the crash machinery its X11 sibling does.
//!
//! Every decision is on the other side of [`crate::linux_wlr_gamma`], which is
//! pure and tested on all three CI lanes; this module connects, asks the
//! compositor how long the table is, writes it to an anonymous file and hands the
//! file over. A GitHub runner has no compositor, so what is here is untested by
//! construction and is kept correspondingly thin.
//!
//! # The object *is* the ramp
//!
//! This is the property that shapes everything below, and it is the opposite of
//! X11's. An `XRandR` ramp is server state with no owner: it survives the client
//! that wrote it, which is why `xrandr --gamma` works as a one-shot command and
//! why Linux needs a crash marker on that transport. A `zwlr_gamma_control_v1`
//! ramp is held by the compositor **for exactly as long as the client's object
//! lives** — *"when the gamma control object is destroyed, the gamma table is
//! restored to its original value"* — and a compositor destroys every object a
//! client holds when its socket closes.
//!
//! Three consequences, and each of them removes machinery rather than adding it:
//!
//! - **A dim is a live object, not a write.** The session below holds one
//!   `zwlr_gamma_control_v1` per dimmed output for as long as the dim lasts.
//!   Dropping the session is the restore.
//! - **A restore is a `destroy`, not an identity table.** That is strictly better
//!   than what X11 can do: writing the identity table *flattens* a running
//!   `gammastep`'s warm evening curve, while destroying puts it back, because the
//!   compositor kept the original. It also releases the output, which an identity
//!   write would not — the protocol grants one client exclusive access per output,
//!   so a Duja that keeps holding a control keeps every colour-temperature tool
//!   locked out.
//! - **There is nothing for a rescue pass to find.** The guarantee survives
//!   `SIGKILL`, so a Wayland session cannot be left dark by a crash and
//!   [`restore_all`] never has to walk anything it did not itself engage. The
//!   crash marker and RAII guard `docs/debt.md` owes Linux are X11's alone.
//!
//! # A wrong table length kills the connection, so this one is its own
//!
//! wlroots answers a short `pread` with
//! `wl_resource_post_error(..., INVALID_GAMMA, ...)`, which terminates the client
//! rather than the object. That is why [`crate::linux_wlr_gamma`] exists, and it
//! is also why this module opens a **second** Wayland connection instead of
//! sharing the overlay backend's: a protocol error is fatal to the connection it
//! is raised on and to everything else riding it, so the two mechanisms are kept
//! on separate sockets and a fatal gamma bug cannot take the layer-shell overlay
//! down with it. The cost is one file descriptor, which is the same trade
//! `linux::gamma` documents for X11 and for the same reason.
//!
//! # It never takes gamma just to answer a question
//!
//! [`enumerate_gamma_displays`] does **not** bind a control per output, and that
//! is a deliberate difference from the X11 walk, which reads each CRTC's table
//! length before reporting it. Here the length only arrives on an object that has
//! already claimed the output *exclusively*, so a truthful enumeration would mean
//! taking gamma away from whatever else wanted it — for the duration of a query,
//! from a read-only call, on a protocol whose commonest other user is a
//! colour-temperature daemon the user chose to run. So the enumeration reports
//! every named output and the availability question is settled where ADR-0011
//! already puts it: at the attempt, by the `failed` event, which is what
//! [`crate::linux_caps::SurfaceCaps::refuse_gamma`] was written for.
//!
//! # Nothing engages this yet
//!
//! Stated rather than left to be discovered, exactly as `linux::gamma` states it.
//! The engage path is the app's Linux gamma sink, which the tray owns, and the
//! tray is not built on Linux until the ksni wave. There is a second gate beyond
//! that one and it is specific to this transport: `is_hdr_active` answers `None`
//! on Wayland, so [`crate::GammaSupport`] is `Unknown` and a caller that respects
//! it will plan an overlay instead. That is the honest answer today — Wayland is
//! where Linux HDR actually happens and this protocol has no query for it — and
//! `docs/debt.md` carries the remedy, which is `wp_color_management_v1`'s
//! per-output `tf_named`.

use std::io::Write as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::{Mutex, OnceLock, PoisonError};

use rustix::fs::{MemfdFlags, memfd_create};
use tracing::debug;
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{
    Connection, Dispatch, DispatchError, EventQueue, Proxy as _, QueueHandle, delegate_noop,
};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1;
use wayland_protocols_wlr::gamma_control::v1::client::zwlr_gamma_control_v1::{
    self, ZwlrGammaControlV1,
};

use duja_core::dimmer::DimmerError;

use crate::gamma_support::RestoreReport;
use crate::linux_caps::GAMMA_CONTROL;
use crate::linux_wlr_gamma::{gamma_table, ramp_size, wlr_gamma_refusal};

/// The only version of `zwlr_gamma_control_manager_v1` there is, and the only one
/// wlroots has ever created its global at.
const GAMMA_CONTROL_VERSION: std::ops::RangeInclusive<u32> = 1..=1;

/// The `zxdg_output_manager_v1` versions this backend can use.
///
/// 1 is enough for what it reads, and the ceiling matches [`super::outputs`] and
/// [`super::layer`], which bind the same global.
const XDG_OUTPUT_VERSIONS: std::ops::RangeInclusive<u32> = 1..=3;

/// The `wl_output` version that added the `name` event.
///
/// A ceiling rather than a requirement — binding above what the compositor
/// advertises is a protocol error, which on this connection is fatal — and the
/// same one [`super::outputs`] uses, because the name this module addresses an
/// output by has to be the string that module stamped as its token. (Not
/// [`super::layer`]'s cap of 3: that backend matches outputs by rectangle and
/// reads no `wl_output` event at all, so it asks for `release` and nothing more.)
const WL_OUTPUT_NAME_VERSION: u32 = 4;

/// The `wl_output` version that added `release`, so an unplugged monitor's proxy
/// can be given back instead of leaked for the life of the connection.
const WL_OUTPUT_RELEASE_VERSION: u32 = 3;

/// A display whose gamma table can be driven, identified by its **`wl_output`**.
///
/// The output and not a CRTC: Wayland grants no view of the hardware behind an
/// output, and `zwlr_gamma_control_manager_v1.get_gamma_control` takes a
/// `wl_output`, so per-output is the granularity the protocol has. The name is
/// the connector name (`DP-1`), which is also the token `linux::outputs` stamps
/// on every placed Wayland display — so the app's gamma sink can address this
/// without a second enumeration, exactly as the CRTC id does on X11.
#[derive(Debug, Clone)]
pub struct OutputDisplay {
    name: String,
}

impl OutputDisplay {
    /// Wrap an output's connector name.
    ///
    /// The constructor the app's gamma sink will use, because a `gamma_token`
    /// carries the name and nothing else. The X11 sibling is
    /// `CrtcDisplay::from_crtc`, and the pair is deliberately parallel: two
    /// tokens, two channels, one per transport.
    #[must_use]
    pub fn from_output(name: &str) -> Self {
        OutputDisplay {
            name: name.to_owned(),
        }
    }

    /// The connector name, which is both the address and the label.
    ///
    /// There is no id to append the way `crtc_label` appends a CRTC number: a
    /// `wl_output` global's numeric name is a per-connection registry id that
    /// changes between runs and means nothing to a user, so the connector name is
    /// the whole of what can be shown.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Drive `display`'s gamma to scale output brightness by `factor`.
///
/// # Errors
/// [`DimmerError::Os`] if this session is not Wayland, if the connection failed,
/// if the compositor does not offer `zwlr_gamma_control_manager_v1`, if no output
/// carries that name, if the compositor **refused** the control (another client
/// holds it, or the output has no gamma table), or if it reported a table length
/// nothing can be built for. The caller falls back to overlay dimming.
///
/// Unlike its X11 sibling, an `Err` here proves no ramp of Duja's is live: this
/// transport's ramp exists only while the object does, and every path that fails
/// destroys the object on the way out.
pub fn set_gamma(display: &OutputDisplay, factor: f32) -> Result<(), DimmerError> {
    with_session(|session| session.write(&display.name, factor)).map_err(Into::into)
}

/// Give `display`'s output back to the compositor, which restores whatever gamma
/// table it had before Duja took it.
///
/// This is the Wayland spelling of the crate's `restore_identity`, and the two are
/// **not the same end state** — this one is better. X11 has no baseline to put
/// back, so it writes the identity table and flattens a running `gammastep`'s
/// tint; here the compositor kept the original and hands it back on `destroy`.
///
/// A display Duja never engaged is a silent success: there is no object to
/// destroy and nothing was changed.
///
/// Unlike [`restore_all`] this **will** open a connection if none is open, and
/// the asymmetry is deliberate rather than an oversight. `restore_all` is what
/// `duja --restore` calls in a process whose entire job is that call, so a connect
/// there is guaranteed useless; this is called by the app's sink about a display
/// it has been managing, so the session it needs is the one already open. Opening
/// one is then the rare path, and going through [`with_session`] is what keeps the
/// transport gate on it.
///
/// # Errors
/// [`DimmerError::Os`] for a session that has no gamma channel at all, or a
/// connection that failed. Never for an output that simply is not dimmed.
pub fn release(display: &OutputDisplay) -> Result<(), DimmerError> {
    with_session(|session| {
        session.state.release(&display.name);
        session.flush()
    })
    .map_err(Into::into)
}

/// Enumerate the outputs a gamma table could be driven on.
///
/// Every `wl_output` the compositor has named, **without** taking a control on any
/// of them — see the module docs for why a read-only call must not claim an
/// exclusive resource, and where the availability answer is settled instead. An
/// output with no name is skipped, because a name is the only address this
/// protocol has and it is the token a display would be joined by.
///
/// Returns an empty vector (never an error) when this session has no gamma
/// channel or the connection failed, which is the graceful-degradation contract
/// the other backends' enumerations keep.
#[must_use]
pub fn enumerate_gamma_displays() -> Vec<OutputDisplay> {
    match with_session(|session| Ok(session.state.named())) {
        Ok(displays) => displays,
        Err(e) => {
            debug!(error = %e.reason(), "no Wayland gamma outputs");
            Vec::new()
        }
    }
}

/// Hand back every output this process is holding a gamma control on.
///
/// Not a rescue, and it cannot be one: nothing on this transport survives the
/// process that set it (module docs), so there is never a stale ramp for a later
/// run to find. What this does is the *orderly* half — releasing the outputs
/// before exit so the compositor restores each original table while Duja is still
/// there to watch it, rather than a moment later when the socket closes.
///
/// So it deliberately **does not open a connection**. A `duja --restore` invoked
/// as its own process holds no controls, and connecting only to discover that
/// would be a socket, a registry round trip and an answer that was already known.
/// An empty clean report is the truth for such a process, not a shrug.
#[must_use]
pub fn restore_all() -> RestoreReport {
    let Some(cell) = SESSION.get() else {
        return RestoreReport::default();
    };
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    let SessionSlot::Open(session) = &mut *guard else {
        return RestoreReport::default();
    };
    let restored = session.state.release_all();
    // A flush failure is not a failed restore. Every control was destroyed on
    // this side, so the compositor puts each table back either when the request
    // arrives or when it notices the socket is gone — and the second of those is
    // what a flush failure means has already happened.
    let _ = session.flush();
    RestoreReport {
        restored,
        failed: Vec::new(),
    }
}

/// One `wl_output`, everything the compositor has said about it, and the gamma
/// control Duja holds on it if it is dimmed.
struct Tracked {
    /// The registry name it was advertised under: what `global_remove` carries,
    /// and so the only stable key a hot-unplug can be matched on.
    global: u32,
    /// The dispatch key, which is stable across removals in a way an index into
    /// the vector is not.
    key: u32,
    output: WlOutput,
    /// `wl_output.name` on a v4 compositor, `xdg_output.name` on an older one —
    /// the same precedence, from the same two sources, as `linux::outputs`, which
    /// is what makes the token that module stamps addressable here.
    name: Option<String>,
    /// Kept only so it can be destroyed; the name is the one thing read from it.
    xdg: Option<ZxdgOutputV1>,
    control: Option<Control>,
}

/// A `zwlr_gamma_control_v1` and what the compositor has said about it.
struct Control {
    object: ZwlrGammaControlV1,
    /// The `gamma_size` event, which arrives immediately on creation. `None`
    /// while the round trip that fetches it is still in flight.
    size: Option<u32>,
    /// The compositor sent `failed`: this object is dead and the protocol says to
    /// destroy it. Never cached beyond that — the reasons are all transient
    /// (another client holds the output, the compositor moved control elsewhere),
    /// so the next attempt binds afresh.
    failed: bool,
}

/// Everything the gamma connection knows.
struct State {
    manager: ZwlrGammaControlManagerV1,
    /// `None` on a compositor with no `zxdg_output_manager_v1`, which costs only
    /// the name fallback: such a compositor is `wl_output` v4 or its outputs have
    /// no name at all and cannot be addressed either way.
    xdg: Option<ZxdgOutputManagerV1>,
    outputs: Vec<Tracked>,
    next_key: u32,
}

impl State {
    /// Start tracking a `wl_output` global, binding it and its `xdg_output`.
    fn track(
        &mut self,
        registry: &WlRegistry,
        global: u32,
        version: u32,
        handle: &QueueHandle<Self>,
    ) {
        let key = self.next_key;
        self.next_key = self.next_key.wrapping_add(1);
        // Capped at what the compositor advertised: binding above it is a
        // protocol error, which on this connection is fatal.
        let output = registry.bind::<WlOutput, _, _>(
            global,
            version.min(WL_OUTPUT_NAME_VERSION),
            handle,
            key,
        );
        let xdg = self
            .xdg
            .as_ref()
            .map(|manager| manager.get_xdg_output(&output, handle, key));
        self.outputs.push(Tracked {
            global,
            key,
            output,
            name: None,
            xdg,
            control: None,
        });
    }

    /// The tracked output with this connector name.
    fn find(&mut self, name: &str) -> Option<&mut Tracked> {
        self.outputs
            .iter_mut()
            .find(|tracked| tracked.name.as_deref() == Some(name))
    }

    /// The tracked output a dispatched event belongs to.
    fn entry(&mut self, key: u32) -> Option<&mut Tracked> {
        self.outputs.iter_mut().find(|tracked| tracked.key == key)
    }

    /// Every output that can be addressed, as a display.
    fn named(&self) -> Vec<OutputDisplay> {
        self.outputs
            .iter()
            .filter_map(|tracked| tracked.name.as_deref().map(OutputDisplay::from_output))
            .collect()
    }

    /// Destroy one output's gamma control, if it holds one.
    ///
    /// Queues the request; the caller flushes. The compositor restores that
    /// output's original table when it arrives.
    fn release(&mut self, name: &str) {
        if let Some(tracked) = self.find(name)
            && let Some(held) = tracked.control.take()
        {
            held.object.destroy();
        }
    }

    /// Destroy every gamma control this session holds, and name the outputs.
    fn release_all(&mut self) -> Vec<String> {
        let mut released = Vec::new();
        for tracked in &mut self.outputs {
            if let Some(held) = tracked.control.take() {
                held.object.destroy();
                released.extend(tracked.name.clone());
            }
        }
        released
    }

    /// Stop tracking an output the compositor has withdrawn.
    ///
    /// Its gamma control is already gone on the server side — wlroots destroys
    /// every control on an output when the output goes away — so this is the
    /// client-side half: give the proxies back rather than leak them for the life
    /// of the connection.
    fn forget(&mut self, global: u32) {
        let Some(index) = self
            .outputs
            .iter()
            .position(|tracked| tracked.global == global)
        else {
            return;
        };
        let tracked = self.outputs.swap_remove(index);
        if let Some(held) = tracked.control {
            held.object.destroy();
        }
        if let Some(xdg) = tracked.xdg {
            xdg.destroy();
        }
        // `release` is since version 3; below that there is no way to give a
        // `wl_output` back and the proxy is simply dropped.
        if tracked.output.version() >= WL_OUTPUT_RELEASE_VERSION {
            tracked.output.release();
        }
    }
}

/// The Wayland gamma connection, the queue it is driven by, and what it knows.
struct Session {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Session {
    /// Send everything queued, without waiting for an answer.
    fn flush(&self) -> Result<(), Fault> {
        self.connection
            .flush()
            .map_err(|e| Fault::wayland("flush", &e))
    }

    /// Send everything queued and wait until the compositor has answered it.
    ///
    /// The synchronisation this whole module is built on: `gamma_size` and
    /// `failed` both arrive as events, so nothing about a control is known until
    /// a round trip has happened. It is one `wl_display.sync` on a local socket,
    /// on a path that runs per slider batch rather than per frame.
    fn roundtrip(&mut self, context: &str) -> Result<(), Fault> {
        self.queue
            .roundtrip(&mut self.state)
            .map(|_| ())
            .map_err(|e| Fault::dispatch(context, &e))
    }

    /// Write one output's gamma table.
    ///
    /// The length is re-read from the control on every write rather than assumed:
    /// it is the number the compositor `pread`s, and sending a different one is a
    /// protocol error that kills the connection.
    fn write(&mut self, name: &str, factor: f32) -> Result<(), Fault> {
        let key = self
            .state
            .find(name)
            .map(|tracked| tracked.key)
            .ok_or_else(|| Fault::refused(format!("no Wayland output is named {name}")))?;
        let advertised = self.acquire(key, name)?;
        let Some(size) = ramp_size(advertised) else {
            // Only reachable for a compositor that is not wlroots: wlroots answers
            // a zero gamma size with `failed` and never sends `gamma_size` at all.
            self.state.release(name);
            return Err(Fault::refused(format!(
                "{name} reports a gamma table of {advertised} entries, which is not a \
                 table this crate can build"
            )));
        };
        let Some(table) = gamma_table(factor, size) else {
            // Unreachable under `ramp_size`, which is the stricter of the two.
            // Kept as a refusal rather than an `unwrap_or_default`, because a
            // table of the wrong length is the one thing this must never send.
            self.state.release(name);
            return Err(Fault::refused(format!(
                "{name} reports {size} gamma entries, which no table can be built for"
            )));
        };
        let fd = table_file(&table)
            .map_err(|e| Fault::refused(format!("{name}: cannot stage the gamma table: {e}")))?;
        let Some(held) = self
            .state
            .find(name)
            .and_then(|tracked| tracked.control.as_ref())
        else {
            return Err(Fault::refused(format!(
                "{name} lost its gamma control between acquiring it and writing"
            )));
        };
        held.object.set_gamma(fd.as_fd());
        // Dropped immediately: the backend `dup()`s every file descriptor as it
        // serialises the request, so this side's copy has no job left. The
        // compositor closes its own after reading.
        drop(fd);
        // The write is confirmed rather than assumed. `set_gamma` has no reply, so
        // what this waits for is the *absence* of trouble: a `failed` event (the
        // compositor could not read the file, or handed the output to someone
        // else) arrives on this round trip, and a length mismatch arrives as a
        // protocol error that ends the connection. Without it, a refused ramp
        // would be reported to the caller as a live one — which is the failure
        // this crate's whole gamma path is shaped to avoid.
        self.roundtrip("set_gamma")?;
        if self.failed(name) {
            self.state.release(name);
            return Err(Fault::refused(format!(
                "{name} refused the gamma table after accepting the control"
            )));
        }
        Ok(())
    }

    /// The gamma-table length for an output, taking a control on it if this
    /// session does not already hold one.
    ///
    /// A control that has already answered is reused, so a slider drag is one
    /// `set_gamma` per sample and no round trip beyond the confirming one.
    ///
    /// # Reusing one means acting on a `failed` flag that is one call stale
    ///
    /// A `failed` event is only seen when the queue is dispatched, and the last
    /// dispatch was the previous call's confirming round trip. So a control the
    /// compositor withdrew *between* two calls still looks live here, and the write
    /// that follows goes to an object the server has already destroyed — which
    /// libwayland answers by killing the client, not by ignoring the request.
    ///
    /// Left as it is, for two reasons rather than one. The window is the gap
    /// between two calls on a path the caller drives in batches, and closing it
    /// would cost a second round trip on *every* sample to catch an event that
    /// arrives on almost none of them. And the consequence is bounded by a decision
    /// already made for a different reason: this connection carries nothing but
    /// gamma (module docs), so losing it costs one error and one reconnect, and the
    /// next call rebinds and re-dims. What it explicitly cannot do is take the
    /// layer-shell overlay with it.
    fn acquire(&mut self, key: u32, name: &str) -> Result<u32, Fault> {
        if let Some(tracked) = self.state.entry(key)
            && let Some(held) = &tracked.control
        {
            if let Some(size) = held.size.filter(|_| !held.failed) {
                return Ok(size);
            }
            // A control that failed, or that never answered, is dead weight: give
            // it back so the next attempt starts clean. Every reason for `failed`
            // is transient, so this is a refusal for *now* and not a latch.
            let held = tracked.control.take();
            if let Some(held) = held {
                held.object.destroy();
            }
        }
        // Cloned rather than borrowed across the call below, which would hold
        // `self.state` mutably while `self.state.manager` is read. A `WlOutput` is
        // a handle — an object id and a weak backend reference — so the clone is
        // the borrow checker's price and not the compositor's.
        let Some(output) = self.state.entry(key).map(|tracked| tracked.output.clone()) else {
            return Err(Fault::refused(format!(
                "{name} went away before its gamma control could be taken"
            )));
        };
        let control = self
            .state
            .manager
            .get_gamma_control(&output, &self.queue.handle(), key);
        if let Some(tracked) = self.state.entry(key) {
            tracked.control = Some(Control {
                object: control,
                size: None,
                failed: false,
            });
        }
        // `gamma_size` is sent the moment the object is created, so one round trip
        // settles it — and so does `failed`, which is what the compositor sends
        // instead when another client already holds this output.
        self.roundtrip("get_gamma_control")?;
        let Some(tracked) = self.state.entry(key) else {
            return Err(Fault::refused(format!(
                "{name} went away while its gamma control was being taken"
            )));
        };
        match &tracked.control {
            Some(held) if held.failed => {
                let held = tracked.control.take();
                if let Some(held) = held {
                    held.object.destroy();
                }
                Err(Fault::refused(format!(
                    "{name} refused a gamma control: another client holds it, or the \
                     output has no gamma table"
                )))
            }
            Some(held) => held.size.ok_or_else(|| {
                Fault::refused(format!(
                    "{name} answered neither a gamma size nor a refusal"
                ))
            }),
            None => Err(Fault::refused(format!(
                "{name} lost its gamma control while it was being taken"
            ))),
        }
    }

    /// Whether this output's control has been told it is no longer valid.
    fn failed(&mut self, name: &str) -> bool {
        self.state
            .find(name)
            .and_then(|tracked| tracked.control.as_ref())
            .is_some_and(|held| held.failed)
    }
}

/// Stage a gamma table in an anonymous file the compositor can read.
///
/// A **memfd**, and the seekability is the requirement rather than the anonymity:
/// wlroots reads the table with `pread(fd, table, size, 0)`, a positional read, so
/// a pipe fails outright with `ESPIPE` and would be answered with a `failed`
/// event for a table that was perfectly correct. It also sets `O_NONBLOCK` on the
/// descriptor before reading, which a pipe would turn into a short read and
/// therefore into a killed connection.
///
/// The file is left at whatever offset the write ended at, deliberately: `pread`
/// ignores the file position, so seeking back would be a no-op dressed as care.
fn table_file(table: &[u8]) -> std::io::Result<OwnedFd> {
    let fd = memfd_create("duja-gamma", MemfdFlags::CLOEXEC)?;
    let mut file = std::fs::File::from(fd);
    // `write_all` rather than a hand-rolled loop: it already retries a short write
    // and an `EINTR`, which is the whole of what the layer backend's own loop does
    // for a file this one has no reason to keep open afterwards.
    file.write_all(table)?;
    Ok(OwnedFd::from(file))
}

/// Why one gamma call failed, and whether the connection survived it.
///
/// The second field decides whether the session is put back or thrown away, and
/// the two are not interchangeable: a refused output is a per-output answer the
/// next call should retry over the same connection, while a dead socket means
/// every later call fails identically until something reconnects.
struct Fault {
    message: String,
    connection_lost: bool,
}

impl Fault {
    /// A refusal decided here or by the compositor's `failed` event; the
    /// connection is fine.
    fn refused(message: String) -> Self {
        Fault {
            message,
            connection_lost: false,
        }
    }

    /// A failure to send.
    fn wayland(context: &str, error: &WaylandError) -> Self {
        Fault {
            message: format!("Wayland {context} failed: {error}"),
            connection_lost: !survivable(error),
        }
    }

    /// A failure to send or to dispatch what came back.
    fn dispatch(context: &str, error: &DispatchError) -> Self {
        Fault {
            message: format!("Wayland {context} failed: {error}"),
            connection_lost: match error {
                DispatchError::Backend(e) => !survivable(e),
                // A message this client could not parse. The connection is
                // technically alive, but the object graph is no longer trusted —
                // and unlike a per-request X11 error there is no way to ask what
                // was missed, so the session is rebuilt.
                DispatchError::BadMessage { .. } => true,
            },
        }
    }
}

/// Whether the connection is still usable after this error.
///
/// Only one kind is: a `WouldBlock`, which means the compositor's socket buffer
/// was full at the moment of the write and nothing else. `wayland-backend` sends
/// with `MSG_DONTWAIT` and deliberately does **not** record a `WouldBlock` as the
/// connection's `last_error`, and `BufferedSocket::flush` keeps the unsent bytes,
/// so the request is still queued and the next call sends it. Treating it as
/// fatal is the defect `#130`'s review found in the layer backend, where it
/// cost the overlay for the rest of the session; here it would cost the dim a
/// frame and an error, which is smaller and still wrong.
///
/// Everything else — a protocol error (the compositor killed this client), a
/// closed socket, a missing library — is permanent for this connection.
fn survivable(error: &WaylandError) -> bool {
    match error {
        WaylandError::Io(e) => e.kind() == std::io::ErrorKind::WouldBlock,
        WaylandError::Protocol(_) => false,
    }
}

/// Why a gamma call could not reach the channel.
///
/// The same split `linux::gamma` makes, and for the same reason: one of these is
/// "this session never had a gamma channel" and can be cached for the life of the
/// process, while the other is "this attempt could not use it" and must not be.
enum Unavailable {
    /// There is no `wlr-gamma-control` channel in this session at all.
    NoChannel(String),
    /// There should be a channel and this call could not use it.
    Failed(String),
}

impl Unavailable {
    /// The human-readable reason, for a log line or a report row.
    fn reason(&self) -> &str {
        match self {
            Unavailable::NoChannel(reason) | Unavailable::Failed(reason) => reason,
        }
    }
}

impl From<Unavailable> for DimmerError {
    fn from(unavailable: Unavailable) -> Self {
        DimmerError::Os(match unavailable {
            Unavailable::NoChannel(reason) | Unavailable::Failed(reason) => reason,
        })
    }
}

/// The process-wide gamma connection.
///
/// Shared rather than per-call for a stronger reason than its X11 twin, which
/// shares one only to avoid a connect per slider frame. Here the connection
/// **is** the dim: every `zwlr_gamma_control_v1` this session holds keeps one
/// output's ramp alive, and dropping the connection restores every one of them.
/// A per-call connection would set a ramp and undo it before returning.
static SESSION: OnceLock<Mutex<SessionSlot>> = OnceLock::new();

/// What the process knows about its gamma connection.
#[derive(Default)]
enum SessionSlot {
    /// Not opened yet, or dropped after a connection failure so the next call
    /// reconnects.
    #[default]
    Empty,
    /// Live, and holding whatever controls have been taken.
    ///
    /// Boxed because a `Session` is an order of magnitude larger than the other
    /// variants and this enum is moved in and out of the slot on every call.
    Open(Box<Session>),
    /// This compositor has no gamma protocol and will not grow one while the
    /// process runs.
    ///
    /// Cached for the same reason X11 caches its Xwayland verdict: the gamma
    /// coordinator above retries a refused engage on every batch, so re-deriving
    /// this would mean a socket connect and a registry round trip per frame for
    /// as long as the user holds the slider.
    ///
    /// Only the *protocol's* absence is cached. A per-output `failed` never is —
    /// every reason for it is another program's doing and can stop being true.
    NoChannel(String),
}

/// Run `f` against the shared connection, opening it if needed.
///
/// The session is taken for the duration and put back afterwards — unless the
/// call reported the connection lost, in which case it is dropped and the next
/// call reconnects. Dropping it also destroys every gamma control on it, which is
/// the right thing on a connection that is already gone: the compositor has
/// restored those tables anyway.
fn with_session<T>(f: impl FnOnce(&mut Session) -> Result<T, Fault>) -> Result<T, Unavailable> {
    // The cheap gate first, before anything opens a socket. Read per call, so a
    // changed environment is caught immediately.
    if let Some(reason) = wlr_gamma_refusal(crate::linux::session_transport()) {
        return Err(Unavailable::NoChannel(reason.to_owned()));
    }
    let cell = SESSION.get_or_init(|| Mutex::new(SessionSlot::default()));
    // A poisoned lock means an earlier call panicked while holding it. Every path
    // through this function leaves the slot consistent, so recovering the guard is
    // right and refusing every later gamma call for the life of the process is not.
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    let mut session = match std::mem::take(&mut *guard) {
        SessionSlot::Open(session) => session,
        SessionSlot::NoChannel(reason) => {
            *guard = SessionSlot::NoChannel(reason.clone());
            return Err(Unavailable::NoChannel(reason));
        }
        SessionSlot::Empty => match open() {
            Ok(session) => Box::new(session),
            Err(Unavailable::NoChannel(reason)) => {
                *guard = SessionSlot::NoChannel(reason.clone());
                return Err(Unavailable::NoChannel(reason));
            }
            // Not cached: a connect that failed can succeed later, and it is the
            // one failure a user might fix without restarting Duja.
            Err(failed) => return Err(failed),
        },
    };
    let outcome = f(&mut session);
    match outcome {
        Ok(value) => {
            *guard = SessionSlot::Open(session);
            Ok(value)
        }
        Err(fault) => {
            if !fault.connection_lost {
                *guard = SessionSlot::Open(session);
            }
            Err(Unavailable::Failed(fault.message))
        }
    }
}

/// Open the gamma connection and bind what it needs.
///
/// The two failure kinds are kept apart all the way out: a compositor with no
/// gamma protocol is a session with **no channel** — cacheable, because a
/// compositor does not grow one mid-run — while a connect that failed is a
/// channel that could not be reached this time.
fn open() -> Result<Session, Unavailable> {
    let connection = Connection::connect_to_env()
        .map_err(|e| Unavailable::Failed(format!("Wayland connect failed: {e}")))?;
    let (globals, mut queue) = registry_queue_init::<State>(&connection)
        .map_err(|e| Unavailable::Failed(format!("Wayland registry init failed: {e}")))?;
    let handle = queue.handle();
    // The error is not inspected because at `1..=1` only one of the two is
    // reachable: `bind` answers `UnsupportedVersion` when the advertised version is
    // below the requested floor, and there is no version below 1. So a failure here
    // is `NotPresent`, and the message says so rather than hedging.
    let manager = globals
        .bind::<ZwlrGammaControlManagerV1, _, _>(&handle, GAMMA_CONTROL_VERSION, ())
        .map_err(|_| {
            Unavailable::NoChannel(format!(
                "this compositor does not offer {GAMMA_CONTROL}, so no client can drive \
                 its gamma tables"
            ))
        })?;
    let xdg = globals
        .bind::<ZxdgOutputManagerV1, _, _>(&handle, XDG_OUTPUT_VERSIONS, ())
        .ok();
    let mut state = State {
        manager,
        xdg,
        outputs: Vec::new(),
        next_key: 0,
    };
    let registry = globals.registry();
    for global in globals.contents().clone_list() {
        if global.interface == WlOutput::interface().name {
            state.track(registry, global.name, global.version, &handle);
        }
    }
    // One round trip is enough by construction: `wl_display.sync` is answered only
    // after every request queued before it and every event those requests
    // generated, so the whole `wl_output`/`xdg_output` name burst is delivered
    // before it returns.
    queue
        .roundtrip(&mut state)
        .map_err(|e| Unavailable::Failed(format!("Wayland roundtrip failed: {e}")))?;
    // `globals` is dropped here and that is safe: `GlobalList` has no destructor,
    // the `wl_registry` object stays alive on the server for the life of the
    // connection, and its `GlobalListContents` lives in the object data the
    // backend owns — which is what keeps the hot-plug events below arriving.
    Ok(Session {
        connection,
        queue,
        state,
    })
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    /// Hot-plug, which this connection has to handle itself because it is
    /// long-lived by design.
    ///
    /// `linux::outputs` re-enumerates from a fresh connection per pass and so
    /// needs none of this; here the session outlives every call, because the
    /// controls it holds *are* the dim, so a monitor plugged in after startup
    /// would otherwise be invisible to the gamma channel forever.
    ///
    /// Binding here cannot double-bind the initial list: `registry_queue_init`
    /// does its own round trip first and forwards nothing to this handler until
    /// that has finished, so every `Global` seen here is one that arrived after
    /// [`open`] read the list.
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                if interface == WlOutput::interface().name {
                    state.track(registry, name, version, handle);
                }
            }
            wl_registry::Event::GlobalRemove { name } => state.forget(name),
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for State {
    /// Only `name` is taken. This module needs an address, not a rectangle — the
    /// gamma control is created from the `wl_output` object itself — so the
    /// geometry and mode events that `linux::outputs` has to reason about are of
    /// no interest here.
    fn event(
        state: &mut Self,
        _output: &WlOutput,
        event: wayland_client::protocol::wl_output::Event,
        key: &u32,
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_output::Event::Name { name } = event
            && let Some(tracked) = state.entry(*key)
        {
            tracked.name = Some(name);
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for State {
    /// The name fallback, and only that.
    ///
    /// `xdg_output` has carried a name since version 2 and compositors are
    /// required to keep sending it even though it is deprecated in favour of
    /// `wl_output.name`. Without this arm, a compositor advertising `wl_output` v3
    /// and `xdg_output` v2 would hand over outputs with no name — which here means
    /// no address at all, so every one of them would be unreachable. `linux::outputs`
    /// reads the same two sources in the same order, which is what makes the token
    /// it stamps and the name this addresses by the same string.
    fn event(
        state: &mut Self,
        _xdg_output: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        key: &u32,
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        if let zxdg_output_v1::Event::Name { name } = event
            && let Some(tracked) = state.entry(*key)
            && tracked.name.is_none()
        {
            tracked.name = Some(name);
        }
    }
}

impl Dispatch<ZwlrGammaControlV1, u32> for State {
    /// The two things a gamma control ever says.
    ///
    /// `gamma_size` is the length of the table the compositor will read, sent
    /// immediately on creation. `failed` means the object is no longer valid —
    /// another client holds the output, the output has no gamma table, or the
    /// compositor moved control elsewhere — and the protocol says to destroy it,
    /// which the paths that read this flag do.
    fn event(
        state: &mut Self,
        _control: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        key: &u32,
        _connection: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
        let Some(tracked) = state.entry(*key) else {
            return;
        };
        let Some(held) = &mut tracked.control else {
            return;
        };
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => held.size = Some(size),
            zwlr_gamma_control_v1::Event::Failed => held.failed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ZwlrGammaControlManagerV1);
delegate_noop!(State: ignore ZxdgOutputManagerV1);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux_caps::Transport;

    /// The label a token-built display carries, which is all the app's gamma sink
    /// can give it. Runs on the Linux CI lane with no compositor: it touches no
    /// connection.
    #[test]
    fn a_display_built_from_a_token_is_named_by_its_connector() {
        let display = OutputDisplay::from_output("DP-1");
        assert_eq!(display.name(), "DP-1");
    }

    /// A session with no compositor must degrade rather than block, panic, or
    /// claim success — which is the state every CI lane runs in, and the only
    /// state in which this module's real entry points can be exercised at all.
    ///
    /// It returns instead of asserting the environment for the same reason its
    /// X11 twin does: a developer runs this suite inside their own session, and a
    /// test must not change the screen of the person running it. What that costs
    /// is worth naming — on a Wayland desktop this test pins nothing. Its coverage
    /// is the CI lanes, which is where the headless refusals are the only
    /// behaviour a runner can observe.
    #[test]
    fn a_session_with_no_compositor_degrades_rather_than_failing_loudly() {
        if crate::linux::session_transport() == Transport::Wayland {
            return;
        }
        let display = OutputDisplay::from_output("DP-1");
        assert!(
            enumerate_gamma_displays().is_empty(),
            "there is no compositor to enumerate outputs from"
        );
        assert!(
            set_gamma(&display, 0.5).is_err(),
            "a ramp must never report success with no compositor to accept it"
        );
        assert!(release(&display).is_err());
        let report = restore_all();
        assert!(report.restored.is_empty(), "nothing can have been restored");
        assert!(report.is_clean(), "nothing attempted cannot have failed");
    }

    /// `restore_all` must not open a connection. A `duja --restore` process holds
    /// no controls, so connecting would be a socket and a round trip to learn
    /// what was already known — and on a session that *is* Wayland it would also
    /// take a gamma manager for nothing.
    ///
    /// Asserted through the session slot rather than by observing a socket,
    /// because the slot is the thing the rule is about: untouched means unopened.
    #[test]
    fn a_restore_with_no_session_does_not_open_one() {
        let report = restore_all();
        assert!(report.is_clean());
        assert!(report.restored.is_empty());
        assert!(
            SESSION.get().is_none_or(|cell| matches!(
                &*cell.lock().unwrap_or_else(PoisonError::into_inner),
                SessionSlot::Empty | SessionSlot::NoChannel(_)
            )),
            "restore_all opened a connection it had no reason to open"
        );
    }
}

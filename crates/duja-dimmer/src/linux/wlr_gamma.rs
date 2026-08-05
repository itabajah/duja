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
//! - **A restore is a `destroy`, not an identity table** — and what that is worth
//!   is narrower than it first looks, so it is worth stating exactly. The protocol
//!   says destroying "restores the gamma table to its original value", and the
//!   tempting reading is that a running `gammastep`'s warm curve comes back. It
//!   does not, and the C says why: `gamma_control_destroy` emits `set_gamma` with
//!   **no control attached**, the compositor re-queries, finds none, and applies
//!   *no* colour transform. "Original" means the output's default, not some other
//!   client's table — wlroots stores no such thing. That is the same end state an
//!   X11 identity write produces.
//!
//!   What `destroy` really buys is the **release**. This protocol grants one client
//!   exclusive access per output, and an identity write has no way to say "I am
//!   finished with this"; a `destroy` does, so a colour-temperature tool can take
//!   the output back. On X11 there is no ownership to hand over at all — the LUT is
//!   shared and last writer wins.
//! - **There is nothing for a rescue pass to find.** The guarantee survives
//!   `SIGKILL`, so a Wayland session cannot be left dark by a crash and
//!   [`restore_all`] never has to walk anything it did not itself engage. The
//!   crash marker and RAII guard `docs/debt.md` owes Linux are X11's alone.
//!
//! # A wrong table length kills the connection, so this one is its own
//!
//! wlroots answers a short read with
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
//! every named output and the availability question is settled at the attempt, by
//! the `failed` event, which is where ADR-0011 already puts it.
//!
//! **What that does not do is reach the capability report.** ADR-0011's step 5 is
//! [`crate::linux_caps::SurfaceCaps::refuse_gamma`], and it still has no
//! production caller: this backend turns `failed` into a
//! [`DimmerError`] for the caller that asked, and
//! nothing downgrades the `gamma` arm of a report `dujactl doctor` has already
//! printed. That is not an oversight this PR could fix in passing — the only
//! report on Linux comes from [`super::probe_session`], which is read-only by
//! design, and a probe that attempted a bind would be doing the exact
//! output-stealing this section refuses. So `refuse_gamma` is waiting on a caller
//! that holds a report *across* an engage attempt, which is the app's gamma sink.
//! `docs/debt.md` carries it.
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
//! `docs/debt.md` carries the remedy, which is the colour-management protocol's
//! per-output `tf_named` (and its limits: `tf_power` names no function, so that
//! case stays unknown, and the XML is behind a cargo feature this workspace does
//! not enable). [`super::is_hdr_active`] is where that is spelled out.

use std::io::{Seek as _, Write as _};
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
/// **The floor is 2, and it is not the 1 its two neighbours use.** `xdg_output`'s
/// `name` event is `since="2"`, and a name is the only thing this backend reads
/// from the protocol at all — so against a v1 manager every `zxdg_output_v1` this
/// created would be an object that can never answer the one question it was made
/// for. Asking for 2 turns that into a clean `UnsupportedVersion` and `xdg: None`.
///
/// **It changes no output's addressability**, and an earlier draft of this comment
/// claimed it did. An output that has neither `wl_output` v4 nor `xdg_output` v2
/// has no name either way, so [`State::named`] skips it and [`State::find`] misses
/// it whichever floor is asked for. What the floor buys is not creating the
/// objects.
///
/// [`super::layer`] asks for 1 because it reads only `logical_position` and
/// `logical_size`, both there since 1. [`super::outputs`] also asks for 1, and it
/// **does** read `name` — opportunistically, because it needs the geometry from
/// v1 regardless and a name from `wl_output` when it can get one. Different needs,
/// not a disagreement; only the ceiling is genuinely shared.
const XDG_OUTPUT_VERSIONS: std::ops::RangeInclusive<u32> = 2..=3;

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
/// Unlike its X11 sibling, an `Err` here leaves **no ramp of Duja's behind**: this
/// transport's dim exists only while the object does, and every failing path
/// destroys the object on the way out.
///
/// "Behind" rather than "live", because one path is momentary rather than empty.
/// If the confirming round trip fails survivably — a `WouldBlock` flush — the
/// `set_gamma` is already buffered and will be sent, so the compositor may apply
/// the table for as long as it takes the `destroy` queued behind it to arrive.
/// That is a flicker, not a state: what cannot happen is the caller being told the
/// engage failed while the output stays dimmed and claimed, which is what this
/// wording used to promise and did not deliver.
pub fn set_gamma(display: &OutputDisplay, factor: f32) -> Result<(), DimmerError> {
    with_session(|session| session.write(&display.name, factor)).map_err(Into::into)
}

/// Give `display`'s output back to the compositor, which restores whatever gamma
/// table it had before Duja took it.
///
/// The Wayland spelling of the crate's `restore_identity`, and it reaches the same
/// *screen* by a different mechanism: destroying the control makes the compositor
/// apply no colour transform, which is what an X11 identity table also produces.
///
/// The difference is ownership rather than pixels. This protocol grants one client
/// exclusive access per output, and letting go is the only way to give it back —
/// so a colour-temperature tool can re-acquire afterwards, which on X11 has no
/// equivalent because nothing owns the LUT there in the first place.
///
/// It does **not** restore some other client's curve. An earlier draft of this
/// paragraph said the compositor "kept the original and hands it back"; wlroots
/// keeps no such table, and the protocol's "original value" is the output's
/// default.
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
/// connection that is gone. Never for an output that simply is not dimmed, and —
/// unlike [`set_gamma`] — never merely because the request could not be *sent*.
///
/// That asymmetry is deliberate and it matches [`restore_all`], which had it
/// first. The operation is the local `destroy`, and once that has happened the dim
/// is over whatever the socket does next: a `WouldBlock` leaves the request queued
/// for the next flush, and a connection that has genuinely died takes every gamma
/// control with it, which restores the very tables this was handing back. Reporting
/// `Err` there would tell a caller "still dimmed" about a display that is not, and
/// the caller would keep it marked dimmed while every later `release` was a no-op.
/// A lost connection is still propagated, because that is what tells
/// [`with_session`] to throw the session away.
pub fn release(display: &OutputDisplay) -> Result<(), DimmerError> {
    with_session(|session| {
        let Some(key) = session.state.find(&display.name).map(|tracked| tracked.key) else {
            // Not tracked, so certainly not dimmed by this session. No round trip
            // to look harder: unlike `write`, there is nothing to do even if the
            // output turned up.
            return Ok(());
        };
        session.state.release(key);
        // A round trip rather than a flush, so the events this long-lived session
        // would otherwise never read are dispatched — see `with_session` for why
        // that matters on a connection that only advances when something calls it.
        match session.roundtrip("release") {
            // A lost connection is propagated so `with_session` throws the session
            // away; anything else has already achieved what this call is for.
            Err(fault) if fault.connection_lost => Err(fault),
            Ok(()) | Err(_) => Ok(()),
        }
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
/// It **resynchronises first**, and that is not a formality. The session outlives
/// every call and only advances when something dispatches, so a monitor plugged in
/// after it opened is invisible to [`State::track`]'s registry handler until some
/// call happens to talk to the compositor. Without this, the one path whose entire
/// job is to say what is there could answer from a picture of the outputs that is
/// arbitrarily old, and the hot-plug handling this connection carries would be
/// decorative exactly where it is most visible.
///
/// A transient send failure therefore makes this answer **empty** rather than
/// stale, which is a change from the first draft: the session may know about live,
/// named outputs and still report none. Empty is the contract this call already
/// has for every other failure, and a stale list is the answer it exists to avoid.
///
/// Returns an empty vector (never an error) when this session has no gamma
/// channel or the connection failed, which is the graceful-degradation contract
/// the other backends' enumerations keep.
#[must_use]
pub fn enumerate_gamma_displays() -> Vec<OutputDisplay> {
    match with_session(|session| {
        session.resync("enumerate")?;
        Ok(session.state.named())
    }) {
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
/// before exit, so each one is handed back while Duja is still there to see it
/// rather than a moment later when the socket closes.
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
    // A failure here is not a failed restore. Every control was destroyed on this
    // side, so the compositor drops each output's transform either when the
    // request arrives or when it notices the socket is gone — and the second of
    // those is what a transport failure means has already happened.
    //
    // A round trip rather than a flush, for the same reason [`release`] uses one:
    // it is also the only thing that reads the socket, and this is one of the four
    // paths that ever touch this connection.
    let lost = session
        .roundtrip("restore_all")
        .is_err_and(|fault| fault.connection_lost);
    // This is the one entry point that does not go through `with_session`, so it is
    // also the one that has to throw a dead connection away itself. Leaving it
    // `Open` would keep every later call failing against a socket that is already
    // gone, when reconnecting would have worked.
    if lost {
        *guard = SessionSlot::Empty;
    }
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
    /// Queues the request; the caller sends it. When it arrives the compositor
    /// drops this client's colour transform, so the output goes back to its
    /// default, and the output is free for another client to claim.
    ///
    /// Keyed rather than named, because every caller inside a write has already
    /// resolved the key and a name is the wrong thing to re-resolve after a
    /// dispatch: `wl_output` names are unique among *live* globals, so an
    /// unplug-and-replug between two lookups can hand the same string to a
    /// different output.
    fn release(&mut self, key: u32) {
        if let Some(tracked) = self.entry(key)
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

    /// Bring the output list up to date, including any output that has only just
    /// appeared.
    ///
    /// **One round trip is not enough here, and it is enough in [`open`]** — the
    /// difference is what was queued before the `wl_display.sync`. In `open` the
    /// `bind`s go out first, so the sync is answered only after the events they
    /// generated. Here the `bind` does not exist yet: the first round trip is what
    /// *delivers* the `wl_registry.global` for the new monitor, and
    /// [`State::track`] queues its `bind` from inside that dispatch — after the
    /// sync was already sent, and normally after the compositor has already
    /// answered it. So the first pass reliably learns that an output exists and
    /// just as reliably does not learn its name.
    ///
    /// The second pass is queued behind those binds and closes it. It is skipped
    /// when every tracked output already has a name, so the ordinary case still
    /// costs one round trip; a compositor too old to name its outputs at all pays
    /// the extra one on these two cold paths and nowhere else.
    fn resync(&mut self, context: &str) -> Result<(), Fault> {
        self.roundtrip(context)?;
        if self.state.outputs.iter().any(|t| t.name.is_none()) {
            self.roundtrip(context)?;
        }
        Ok(())
    }

    /// The key of the output with this connector name, asking the compositor
    /// before giving up.
    ///
    /// The retry is the point. This session outlives every call and its output list
    /// only grows when the queue is dispatched, so a monitor plugged in after the
    /// session opened is unknown to [`State::find`] until *something* talks to the
    /// compositor. Without the resynchronisation below, a display that appeared
    /// after startup would answer "no Wayland output is named DP-3" on every call
    /// for the life of the process — and the caller is documented to address
    /// displays by token rather than by enumerating first, so nothing else would
    /// ever heal it.
    fn key_for(&mut self, name: &str) -> Result<u32, Fault> {
        if let Some(key) = self.state.find(name).map(|tracked| tracked.key) {
            return Ok(key);
        }
        self.resync("output lookup")?;
        self.state
            .find(name)
            .map(|tracked| tracked.key)
            .ok_or_else(|| Fault::refused(format!("no Wayland output is named {name}")))
    }

    /// Write one output's gamma table.
    ///
    /// The length comes from the `gamma_size` event, which is sent once when the
    /// control is created and never again, so a control reused across writes
    /// carries a cached length. There is no way to re-read it: the protocol has no
    /// request for the size and no second event.
    ///
    /// On current wlroots that is safe, because the compositor writes against the
    /// `ramp_size` it stored when it created the control — the same number it
    /// advertised. On **0.16 and earlier** it is not quite: that version re-queries
    /// `wlr_output_get_gamma_size(output)` at write time, and LUT size is a
    /// property of the CRTC rather than the output, so an output moved to a CRTC
    /// with a larger table would make this send a short one — which is the
    /// `INVALID_GAMMA` connection kill this whole module is shaped around. Narrow
    /// (it needs a CRTC reassignment *and* a hardware size difference between the
    /// two), unfixable from the client side, and bounded by a decision already
    /// made: this connection carries nothing but gamma, so the blast radius is one
    /// reconnect rather than the layer-shell overlay.
    fn write(&mut self, name: &str, factor: f32) -> Result<(), Fault> {
        let key = self.key_for(name)?;
        let advertised = self.acquire(key, name)?;
        let Some(size) = ramp_size(advertised) else {
            // A compositor whose gamma size does not fit the builder. wlroots
            // cannot reach this — it answers a zero size with `failed` and never
            // sends `gamma_size` — but 1 and anything above `u16::MAX` land here
            // too, so it is not the wlroots-only arm an earlier draft called it.
            self.give_up(key);
            return Err(Fault::refused(format!(
                "{name} reports a gamma table of {advertised} entries, which is not a \
                 table this crate can build"
            )));
        };
        let Some(table) = gamma_table(factor, size) else {
            // Unreachable under `ramp_size`, which is the stricter of the two.
            // Kept as a refusal rather than an `unwrap_or_default`, because a
            // table of the wrong length is the one thing this must never send.
            self.give_up(key);
            return Err(Fault::refused(format!(
                "{name} reports {size} gamma entries, which no table can be built for"
            )));
        };
        let fd = match table_file(&table) {
            Ok(fd) => fd,
            Err(e) => {
                // Through `give_up` like its two neighbours. Staging the table is
                // the one step that can fail *after* the output has been claimed
                // exclusively — `memfd_create` is `ENOSYS` under some sandbox
                // seccomp filters, and both it and the write can fail on a
                // resource limit — and every one of those repeats on the next
                // call, so keeping the claim would strand the output for good.
                self.give_up(key);
                return Err(Fault::refused(format!(
                    "{name}: cannot stage the gamma table: {e}"
                )));
            }
        };
        let Some(held) = self
            .state
            .entry(key)
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
        //
        // A *survivable* failure of this round trip — a `WouldBlock` flush — is the
        // one case where returning `Err` would not be the whole truth, because
        // `set_gamma` is already in the outgoing buffer and `BufferedSocket::flush`
        // keeps what it could not send. Some later call would deliver it, and the
        // caller, having been told the engage failed, would have planned an overlay
        // and would never release the output. So the control is handed back on the
        // way out: the `destroy` is queued behind the `set_gamma`, and the
        // compositor applies the table and drops it again in the same breath.
        if let Err(fault) = self.roundtrip("set_gamma") {
            self.give_up(key);
            return Err(fault);
        }
        // Success is the control still being **there** and still healthy, not
        // merely the absence of a `failed` flag. An earlier draft asked only the
        // second question, through a helper that answered `false` for a control
        // that had gone away — and "gone away" is a real outcome of this very
        // round trip: the output can be unplugged while it runs, at which point
        // `global_remove` reaches `State::forget`, the entry disappears, and the
        // write that was silently dropped by an inert resource would have been
        // reported as a live ramp. The caller does not retry a success, so the
        // display would sit at full brightness with Duja believing it dimmed.
        match self
            .state
            .entry(key)
            .and_then(|tracked| tracked.control.as_ref())
        {
            Some(held) if !held.failed => Ok(()),
            Some(_) => {
                self.give_up(key);
                Err(Fault::refused(format!(
                    "{name} refused the gamma table after accepting the control"
                )))
            }
            None => Err(Fault::refused(format!(
                "{name} went away while its gamma table was being written"
            ))),
        }
    }

    /// Hand an output's control back on a path that is about to return an error.
    ///
    /// The flush is the point, and leaving it out was a real leak rather than an
    /// untidiness: `State::release` only *queues* the `destroy`, and every caller
    /// here returns immediately afterwards, so without this the request would sit
    /// in the output buffer until some later call happened to send something. An
    /// output Duja has decided it cannot drive would stay claimed in the meantime,
    /// and the protocol grants that claim exclusively — so the wait would be at a
    /// colour-temperature tool's expense.
    ///
    /// The flush's own failure is dropped, and the honest reason is narrower than
    /// the one an earlier draft gave. It said "a connection too broken to flush is
    /// one the compositor is about to tear down anyway", which is exactly what
    /// [`survivable`] denies for the case this is most often reached from: a
    /// `WouldBlock` is a full socket buffer, not a dying connection.
    ///
    /// What is true is that there is nothing better to do here. This is already an
    /// error path; the fault being returned is the one worth reporting; and the
    /// `destroy` stays queued, so the next call that reaches the compositor sends
    /// it. The residual is a session where that next call never comes — the write
    /// failed, the caller fell back to the overlay, and nothing touches the gamma
    /// path again — in which the output stays claimed (though not dimmed, since the
    /// `set_gamma` ahead of it is unsent too) until the process exits. `docs/debt.md`
    /// carries it beside the read-side twin, which has the same root: on this
    /// connection only a call drains, and only a call flushes.
    fn give_up(&mut self, key: u32) {
        self.state.release(key);
        let _ = self.flush();
    }

    /// The gamma-table length for an output, taking a control on it if this
    /// session does not already hold one.
    ///
    /// A control that has already answered is reused, so a slider drag is one
    /// `set_gamma` per sample and no round trip beyond the confirming one.
    ///
    /// # A refusal arrives two different ways, because wlroots has done it two ways
    ///
    /// Current wlroots refuses the **newcomer**: `get_gamma_control` finds an
    /// existing control for the output and answers `failed` on the object it just
    /// created, leaving the incumbent untouched. Before `9108717d` (2023-03-06, so
    /// wlroots 0.16 and earlier — which includes the 0.15 that Debian bookworm and
    /// Ubuntu 22.04 LTS ship) it did the opposite: it sent `failed` to the
    /// **incumbent**, destroyed it, and returned without registering or answering
    /// the newcomer at all. On those versions this object receives *neither*
    /// `gamma_size` nor `failed`.
    ///
    /// Both land somewhere sensible below — the first in the `failed` arm, the
    /// second in the arm that finds neither — and both end in a refusal with the
    /// control given back. The second is worth knowing rather than treating as
    /// defensive padding: on an LTS wlroots it is what *every* attempt against an
    /// output another client holds looks like, and the object it leaves behind has
    /// live user data, so a `set_gamma` sent without waiting for `gamma_size`
    /// would be honoured while this client is not the registered controller. That
    /// is why the size is a precondition of writing and not a convenience.
    ///
    /// # Reusing one means acting on a `failed` flag that is one call stale
    ///
    /// A `failed` event is only seen when the queue is dispatched, and the last
    /// dispatch was the previous call's confirming round trip. So a control the
    /// compositor withdrew *between* two calls still looks live here, and the write
    /// that follows goes to an object the compositor has already given up on.
    ///
    /// **A failed control is inert, not dead**, which is what makes that safe, and
    /// it is worth stating because the obvious guess is the opposite one. wlroots
    /// never calls `wl_resource_destroy` on a gamma control from the server side —
    /// the only call site is the client's own `destroy` request. Every server-side
    /// teardown (`failed`, or the output going away) goes through
    /// `gamma_control_destroy`, which nulls the resource's *user data* and leaves
    /// the resource itself alive; a later `set_gamma` then hits
    /// `if (gamma_control == NULL) goto error_fd`, which closes the descriptor and
    /// does nothing else. No protocol error, no killed connection, and no ramp.
    ///
    /// So the stale flag costs one wasted write, and [`Session::write`]'s
    /// confirming round trip is what turns it into an honest failure: that round
    /// trip delivers the pending `failed`, and the check after it releases the
    /// control and returns a refusal. The caller never learns a ramp is live when
    /// it is not, which is the only property that actually has to hold.
    ///
    /// What this does *not* establish is the same behaviour on a compositor that
    /// is not wlroots. One that really destroyed the resource would make the late
    /// `set_gamma` a request on a dead id — though the protocol tells the client to
    /// destroy the object itself on `failed`, which implies it expects the object
    /// to still be there. Either way the blast radius is bounded by a decision made
    /// for a different reason: this connection carries nothing but gamma (module
    /// docs), so the worst case is one error and one reconnect, and what it
    /// explicitly cannot do is take the layer-shell overlay with it.
    fn acquire(&mut self, key: u32, name: &str) -> Result<u32, Fault> {
        if let Some(tracked) = self.state.entry(key)
            && let Some(held) = &tracked.control
        {
            if let Some(size) = held.size.filter(|_| !held.failed) {
                return Ok(size);
            }
            // A control that failed, or that never answered, is dead weight: give
            // it back so the next attempt starts clean. Queued rather than flushed
            // here on purpose — unlike the paths that return, this one goes on to
            // create a replacement, and both requests leave together.
            //
            // Not cached, and the cost of that is real rather than notional. Two
            // of the protocol's four reasons for `failed` are another program's
            // doing and stop being true when it exits; the first one it lists —
            // "the output doesn't support gamma tables" — is permanent hardware.
            // They are indistinguishable, one event with no discriminator, so
            // caching would latch a refusal a user could have fixed by quitting
            // `gammastep`. The price is that an output with no gamma LUT costs one
            // object creation and one round trip on every call, forever. Refusing
            // to latch is the right side of that trade: the latched version is a
            // gamma channel the user cannot get back without restarting Duja.
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
        let Some(tracked) = self.state.entry(key) else {
            // Nothing between the lookup above and here dispatches, so the entry
            // cannot actually have gone in between — but if it ever could, dropping
            // `control` here would be the one leak nothing else can reach. A
            // `wayland-client` proxy sends no destructor on drop, so the compositor
            // would hold that `zwlr_gamma_control_v1` for the life of the
            // connection, invisible to `release`, `release_all` and `forget` alike,
            // with the output claimed exclusively the whole time.
            control.destroy();
            let _ = self.flush();
            return Err(Fault::refused(format!(
                "{name} went away between the lookup for its gamma control and \
                 the request for one"
            )));
        };
        tracked.control = Some(Control {
            object: control,
            size: None,
            failed: false,
        });
        // `gamma_size` is sent the moment the object is created, so one round trip
        // settles it — and so does `failed`, which is what the compositor sends
        // instead when another client already holds this output.
        //
        // Handed back on failure for the same reason the write is: a survivable
        // fault leaves the session open with a control this call is about to stop
        // tracking the meaning of, and an output claimed exclusively for a dim that
        // is not going to happen.
        if let Err(fault) = self.roundtrip("get_gamma_control") {
            self.give_up(key);
            return Err(fault);
        }
        let Some(tracked) = self.state.entry(key) else {
            return Err(Fault::refused(format!(
                "{name} went away while its gamma control was being taken"
            )));
        };
        // Both fields copied out before anything is taken, so the read and the
        // hand-back do not fight over the same borrow.
        let Some((failed, size)) = tracked
            .control
            .as_ref()
            .map(|held| (held.failed, held.size))
        else {
            return Err(Fault::refused(format!(
                "{name} lost its gamma control while it was being taken"
            )));
        };
        if let Some(size) = size.filter(|_| !failed) {
            return Ok(size);
        }
        // Either the compositor refused, or it answered nothing at all. Both mean
        // this session cannot write to that output, and both leave an object that
        // has to be given back — the second one especially, because on wlroots
        // 0.16 and earlier its user data is live, so an object left lying around
        // there is one a later edit could write through without ever having been
        // granted the output.
        //
        // Flushed, not merely queued: this arm returns to the caller, and
        // `give_up`'s own documentation is about exactly this — a `destroy` that
        // sits in the outgoing buffer leaves the output claimed until something
        // else happens to send.
        self.give_up(key);
        Err(Fault::refused(if failed {
            format!(
                "{name} refused a gamma control: another client holds it, or the \
                 output has no gamma table"
            )
        } else {
            format!(
                "{name} answered neither a gamma size nor a refusal, which on \
                 wlroots 0.16 and earlier is what an output another client already \
                 holds looks like"
            )
        }))
    }
}

/// Stage a gamma table in an anonymous file the compositor can read.
///
/// A **memfd**, and it is handed over **rewound to offset 0**. Both halves matter,
/// and the rewind is the one an earlier draft of this function argued *against*.
///
/// wlroots has read this descriptor two different ways, and the version boundary
/// is what makes the rewind load-bearing rather than tidy. Since `15f2f664`
/// (2023-06-05, so wlroots 0.17 onward) it is
/// `pread(fd, table, table_size, 0)`, which ignores the file position entirely.
/// Before that it was a plain `read(fd, table, table_size)`, which does not, and
/// the fix reached neither the 0.15 nor the 0.16 branch — both branch heads still
/// `read`. **Debian bookworm and Ubuntu 22.04 LTS both ship 0.15**, so this is a
/// live configuration rather than history.
///
/// A descriptor sent over `SCM_RIGHTS` is a `dup`, so the compositor **shares this
/// side's open file description and its offset**. An un-rewound memfd is at EOF
/// after the write above, which against a `read()` implementation is a zero-byte
/// read, a length mismatch, and `INVALID_GAMMA` on the whole client connection:
/// the session's first dim would kill it. One `lseek` makes the two behave
/// identically, so the earlier draft's "a no-op dressed as care" was right about
/// exactly one of the two implementations.
///
/// The **anonymity** is not the requirement; the **seekability** is. A pipe serves
/// neither implementation: `pread` refuses one outright with `ESPIPE`, and the
/// compositor sets `O_NONBLOCK` before reading, which turns the other into a short
/// read and so into the same killed connection. A memfd is the cheapest thing that
/// is a real file.
fn table_file(table: &[u8]) -> std::io::Result<OwnedFd> {
    let fd = memfd_create("duja-gamma", MemfdFlags::CLOEXEC)?;
    let mut file = std::fs::File::from(fd);
    // `write_all` rather than a hand-rolled loop: it already retries a short write
    // and an `EINTR`, which is the whole of what the layer backend's own loop does
    // for a file this one has no reason to keep open afterwards.
    file.write_all(table)?;
    file.rewind()?;
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
/// dropped those transforms anyway.
///
/// # Reading the socket is every entry point's job, and one case is left over
///
/// `linux::gamma` drains its X connection with `poll_for_event` after every call,
/// because unsolicited events accumulate in an unbounded queue on a process that
/// may run for weeks. The same pressure exists here and the consequence is worse:
/// a Wayland client that never reads fills its socket buffer, and libwayland's
/// server side answers a client it cannot write to by **disconnecting** it — which
/// would drop every live gamma control at once, silently, with the slot still
/// saying `Open` until the next call found out.
///
/// So all four entry points round-trip rather than flush ([`set_gamma`],
/// [`release`], [`enumerate_gamma_displays`], [`restore_all`]), and a round trip
/// reads and dispatches. What is left uncovered is a session that is **open and
/// idle**: one that engaged a dim and was then left alone while the compositor
/// kept sending `wl_output` reconfiguration events. That is narrower than it
/// sounds, because the events that produce that traffic — hot-plug, mode and
/// layout changes — are the same ones that make the app re-assert its dim, which
/// is a call. It is a residual rather than a hazard, and `docs/debt.md` names it
/// rather than this pretending the X11 sibling's drain has an equivalent here.
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

    /// `restore_all` answers without a session, and says so rather than failing.
    ///
    /// # What this pins, and what it deliberately does not
    ///
    /// It pins the shape of the answer: a clean, empty report, no panic, no block.
    /// That is the whole of what a CI lane can observe, and it is worth having —
    /// `duja --restore` reads an empty clean report as "nothing to restore", so a
    /// panic or a hang here is a broken command.
    ///
    /// It does **not** pin the rule the function is actually built around, which is
    /// that no connection is opened. An earlier version of this test claimed to,
    /// by asserting the session slot was untouched. That assertion cannot fail on
    /// the only lane that runs this module: `with_session` returns at the transport
    /// gate before `SESSION` is ever initialised, so on a headless runner the slot
    /// is `None` however `restore_all` is written — including the version that goes
    /// through `with_session` and opens one. It would have passed the mutation it
    /// existed to catch.
    ///
    /// The no-connection rule is checked by the `WAYLAND_DEBUG` row in
    /// `docs/qa-checklist.md` instead, which is honest about needing a session.
    #[test]
    fn a_restore_with_no_session_answers_rather_than_failing() {
        let report = restore_all();
        assert!(report.is_clean(), "nothing attempted cannot have failed");
        assert!(report.restored.is_empty(), "nothing can have been restored");
    }
}

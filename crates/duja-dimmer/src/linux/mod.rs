//! Everything in this crate that talks to a Linux display server.
//!
//! The split is ADR-0011's, and it is the same one in every submodule here: the
//! rules are pure, run on every CI lane, and name no `wayland-client` or `x11rb`
//! type; these modules connect, fetch what a rule cannot fetch for itself, and
//! carry the answer back to the wire.
//!
//! | pure rule | evidence / effect |
//! |---|---|
//! | [`crate::linux_caps`] | [`x11`], [`wayland`] — two booleans and a list of interface names |
//! | [`crate::linux_outputs`] | [`outputs`] — each output's name, EDID and rectangle |
//! | [`crate::linux_overlay`] | [`overlay`] — the override-redirect ARGB windows |
//! | [`crate::linux_layer`] | [`layer`] — the `zwlr_layer_shell_v1` surfaces |
//! | [`crate::linux_gamma`] | [`gamma`] — the `RandR` CRTC transfer tables |
//! | [`crate::linux_wlr_gamma`] | [`wlr_gamma`] — the `zwlr_gamma_control_v1` tables |
//!
//! Nothing here can run in CI — a GitHub runner has no X server and no
//! compositor — which is precisely why each of these is as small as it is. Every
//! decision the feature makes is on the other side of the boundary.
//!
//! # Two of everything, chosen at runtime
//!
//! This module is where that choice is made, and it is made twice: [`LinuxDimmer`]
//! picks the overlay backend and [`GammaDisplay`] carries which gamma channel a
//! display is addressed on. Windows and macOS need neither, because each has one
//! windowing system; Linux has no single one, and which mechanisms exist is a
//! property of the session rather than of the build.
//!
//! The two choices are deliberately **not** made together. A session's overlay and
//! its gamma channel come from different protocols with different implementors —
//! `KWin` ships layer-shell and no gamma protocol, and `linux_caps` resolves the
//! two arms independently for exactly that reason — so nothing here may infer one
//! from the other.

mod gamma;
mod layer;
mod outputs;
mod overlay;
mod wayland;
mod wlr_gamma;
mod x11;

pub use layer::WaylandDimmer;
pub use outputs::enumerate_outputs;
pub use overlay::X11Dimmer;

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};

use crate::gamma_support::{GammaSupport, RestoreReport, gamma_support_from_hdr};
use crate::linux_caps::{Probe, SessionEnv, SurfaceCaps, Transport, resolve, transport};
use crate::linux_gamma::hdr_active_for;

/// The [`Dimmer`] for a Linux session, chosen at **runtime**.
///
/// Windows and macOS each have one windowing system, so their `PlatformDimmer` is
/// a type alias. Linux does not: whether an overlay is possible, and by what
/// mechanism, is a property of the session rather than the build. So this is a
/// real type that picks when it starts, which is the same answer ADR-0011 gives
/// for the capability report and for the same reason.
#[derive(Debug)]
pub struct LinuxDimmer {
    inner: Box<dyn Dimmer>,
}

impl LinuxDimmer {
    /// Start the backend this session can actually use.
    ///
    /// # Errors
    /// [`DimmerError::Unsupported`] when the session has no overlay mechanism —
    /// no display server, an X11 session with no compositing manager, or a
    /// compositor missing one of the three protocols a layer-shell overlay is
    /// built from. [`DimmerError::Os`] for a session that should have worked and
    /// did not, which the caller logs before disabling software dimming.
    ///
    /// The caller treats both the same way (no dimmer, hardware control intact);
    /// they are distinguished because one is a fault worth a log line naming the
    /// cause and the other is an ordinary session.
    pub fn spawn() -> Result<Self, DimmerError> {
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let display = std::env::var("DISPLAY").ok();
        let env = SessionEnv {
            wayland_display: wayland_display.as_deref(),
            display: display.as_deref(),
        };
        match transport(env) {
            Transport::X11 => X11Dimmer::spawn().map(|dimmer| LinuxDimmer {
                inner: Box::new(dimmer),
            }),
            Transport::Wayland => WaylandDimmer::spawn().map(|dimmer| LinuxDimmer {
                inner: Box::new(dimmer),
            }),
            Transport::None => Err(DimmerError::Unsupported),
        }
    }
}

impl Dimmer for LinuxDimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        self.inner.apply(commands)
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        self.inner.clear()
    }
}

/// A display whose gamma transfer table Duja can drive, on whichever channel this
/// session has.
///
/// The two are not variations on one mechanism, they are different protocols with
/// different owners, and the differences reach the caller:
///
/// | | X11 | Wayland |
/// |---|---|---|
/// | addressed by | a `RandR` **CRTC** id | a `wl_output`'s **connector name** |
/// | granularity | one table per CRTC, so a mirrored pair shares one | one per output |
/// | [`restore_identity`] writes | the identity table | nothing: it hands the output back |
/// | after a crash | the ramp is still on the screen | the output is already back to normal |
///
/// The third row reaches the same *screen* by either route — a compositor with no
/// client transform and an X11 identity table look alike — and differs in what it
/// does to **ownership**. `zwlr_gamma_control_v1` grants one client exclusive
/// access per output, so letting go is the only way to give it back; the X11 LUT
/// is shared and unowned, so there is nothing there to hand over.
#[derive(Debug, Clone)]
pub struct GammaDisplay(Channel);

/// Which gamma protocol a display is addressed through.
#[derive(Debug, Clone)]
enum Channel {
    /// An `XRandR` CRTC, from [`gamma`].
    Crtc(gamma::CrtcDisplay),
    /// A `wl_output`, from [`wlr_gamma`].
    Output(wlr_gamma::OutputDisplay),
}

impl GammaDisplay {
    /// Address a display by its `RandR` CRTC id (X11).
    ///
    /// This and [`Self::from_output`] are what the app's Linux gamma sink will
    /// use, because a `gamma_token` carries the address and nothing else. Which
    /// of the two is right is decided by the session's transport — the same
    /// question [`probe_session`] answers — and the two token formats are not
    /// interchangeable: a CRTC id is decimal and an output name is a connector
    /// string, and [`crate::linux_gamma::crtc_from_token`] refuses the latter
    /// outright rather than parsing a plausible number out of it.
    ///
    /// Nothing calls either yet, and that is stated rather than left to be
    /// discovered: the engage path is the app's gamma sink, which the tray owns,
    /// and the tray is not built on Linux until the ksni wave.
    #[must_use]
    pub fn from_crtc(crtc: u32) -> Self {
        GammaDisplay(Channel::Crtc(gamma::CrtcDisplay::from_crtc(crtc)))
    }

    /// Address a display by its `wl_output` connector name (Wayland).
    ///
    /// See [`Self::from_crtc`] for how the two are told apart.
    #[must_use]
    pub fn from_output(name: &str) -> Self {
        GammaDisplay(Channel::Output(wlr_gamma::OutputDisplay::from_output(name)))
    }

    /// A human-readable name for a report or a log line.
    #[must_use]
    pub fn name(&self) -> &str {
        match &self.0 {
            Channel::Crtc(crtc) => crtc.name(),
            Channel::Output(output) => output.name(),
        }
    }
}

/// Drive `display`'s gamma to scale output brightness by `factor`.
///
/// # Errors
/// [`DimmerError::Os`] when this session has no gamma channel of the kind
/// `display` names, when the connection failed, or when the display server
/// refused the table. The caller falls back to overlay dimming.
///
/// The two channels differ in what an `Err` proves, and the difference is worth
/// knowing. On X11 it does **not** prove the ramp is not live — the write is
/// confirmed with a round trip, and a connection that dies in between reports a
/// failure for a table that is on the screen and **stays** there. On Wayland no
/// residue can outlast the **object**, and every failing path destroys the object
/// on the way out — so the usual worst case is a table applied and dropped again,
/// a flicker, possibly a little after the call returned because a send that blocked
/// leaves the requests queued for whatever flushes next.
///
/// The exception is narrow and worth naming rather than rounding away: a flush that
/// blocks part-way can deliver the table and not the request behind it, and if
/// nothing on this process ever calls the gamma path again, the queued release
/// never goes out. `docs/debt.md` carries it.
pub fn set_gamma(display: &GammaDisplay, factor: f32) -> Result<(), DimmerError> {
    match &display.0 {
        Channel::Crtc(crtc) => gamma::set_gamma(crtc, factor),
        Channel::Output(output) => wlr_gamma::set_gamma(output, factor),
    }
}

/// Undo `display`'s dim.
///
/// Named for what the other two platforms do, because the crate's surface is
/// shared; on Wayland the mechanism is different even though the screen is not.
/// X11 writes the identity transfer table. Wayland destroys the output's
/// `zwlr_gamma_control_v1`, after which the compositor applies no colour transform
/// — the same end state, plus the output released for another client to claim.
///
/// **It is not the baseline composition `linux::gamma`'s docs describe as owed**,
/// and an earlier draft of this paragraph claimed it was "already true on one of
/// the two transports because the protocol does it". That is wrong twice over:
/// wlroots keeps no previous client's table to put back, and this protocol has no
/// request to *read* the current gamma, so a baseline cannot even be sampled here.
/// Composing a dim into a user's curve is owed on X11 and **impossible** on
/// Wayland with this protocol; `docs/debt.md` says so rather than implying Wayland
/// is done.
///
/// # Errors
/// As [`set_gamma`]. A Wayland display that was never dimmed is a silent success:
/// there is no object to destroy and nothing was changed.
pub fn restore_identity(display: &GammaDisplay) -> Result<(), DimmerError> {
    match &display.0 {
        Channel::Crtc(crtc) => gamma::restore_identity(crtc),
        Channel::Output(output) => wlr_gamma::release(output),
    }
}

/// Enumerate the displays whose gamma this session could drive.
///
/// Returns an empty vector rather than an error for a session with no gamma
/// channel or a connection that failed, which is the graceful-degradation
/// contract the other platforms' enumerations keep.
///
/// What the two channels put in it is not the same kind of list, and the
/// asymmetry is deliberate. The X11 walk reads each CRTC's table length first, so
/// it reports only CRTCs that can actually be written. The Wayland one cannot do
/// that without **taking** each output's gamma control, which the protocol grants
/// exclusively — a read-only call must not lock a colour-temperature daemon out
/// of every monitor to answer a question — so it reports every named output and
/// leaves the availability answer to the attempt, which is where ADR-0011 puts it
/// anyway.
#[must_use]
pub fn enumerate_gamma_displays() -> Vec<GammaDisplay> {
    match session_transport() {
        Transport::X11 => gamma::enumerate_gamma_displays()
            .into_iter()
            .map(|crtc| GammaDisplay(Channel::Crtc(crtc)))
            .collect(),
        Transport::Wayland => wlr_gamma::enumerate_gamma_displays()
            .into_iter()
            .map(|output| GammaDisplay(Channel::Output(output)))
            .collect(),
        // Answered here rather than by picking a channel arbitrarily and letting
        // its own refusal produce the same empty list. Both would, and that is the
        // point: with no display server there is no channel to have an opinion.
        Transport::None => Vec::new(),
    }
}

/// Put every gamma table this session is responsible for back.
///
/// Drives `duja --restore` and, once the tray exists on Linux, recovery from a
/// dirty exit. Never fails as a whole: it reports what it reset and what it could
/// not.
///
/// **The two channels are doing genuinely different jobs here**, and only one of
/// them is a rescue:
///
/// - **X11** walks every CRTC on this X screen and writes the identity table to
///   each, including ones driving nothing, because an `XRandR` ramp is server
///   state that outlives the client that set it. A dark screen left by a crashed
///   Duja is exactly what this is for.
/// - **Wayland** destroys the gamma controls *this process* holds and nothing
///   else, because there is nothing else to find: an output's dim lasts only as
///   long as the client's object, and the compositor destroys every object a
///   client holds when the socket closes, so a crash cannot leave a Wayland
///   session dark. It does not even open a connection to discover that.
///
/// So an empty clean report means "nothing to restore" on both, and on Wayland it
/// is the *only* answer a fresh process can honestly give.
///
/// # Both are asked, rather than one being chosen
///
/// Unlike [`enumerate_gamma_displays`], this does not dispatch on the transport,
/// and the difference is not an inconsistency. An enumeration is a question about
/// *this* session, so asking the channel this session does not have would be
/// asking the wrong thing. A restore is not the same question — on Wayland it asks
/// what this **process** is holding, and on X11 it is deliberately wider, a
/// whole-screen rescue that writes identity to CRTCs Duja never touched because an
/// `XRandR` ramp outlives whoever set it. Asking both answers neither wrongly,
/// because each channel refuses cleanly and without a syscall when
/// it is not the one in play: `gamma::restore_all` stops at
/// [`crate::linux_gamma::xrandr_refusal`] before it opens a socket, and
/// `wlr_gamma::restore_all` returns an empty report unless it already has a live
/// session.
///
/// What that buys is one direction of the case where the environment moved under a
/// running process, which `session_transport`'s own documentation is at pains to
/// say can happen. A process that engaged Wayland gamma and then saw
/// `WAYLAND_DISPLAY` disappear would, under a transport switch, never hand those
/// outputs back — and would report a clean rescue for work it did not do.
///
/// **The other direction is not bought, and is worse.** `wlr_gamma::restore_all`
/// works after a drift because it bypasses `with_session` and reads its slot
/// directly; `gamma::restore_all` has no such escape — it stops at
/// [`crate::linux_gamma::xrandr_refusal`], which answers from the *current*
/// environment, so a process that engaged `XRandR` ramps and then acquired a
/// `WAYLAND_DISPLAY` gets an empty clean report over CRTCs it left dark. And an
/// X11 ramp outlives the process, so unlike the Wayland residual this paragraph
/// exists to justify, that one is permanent. Unreachable while nothing on Linux
/// engages a ramp, and not closable without the X11-side guard `docs/debt.md`
/// already owes; recorded there rather than implied away here.
#[must_use]
pub fn restore_all() -> RestoreReport {
    let mut report = wlr_gamma::restore_all();
    let mut x11 = gamma::restore_all();
    report.restored.append(&mut x11.restored);
    report.failed.append(&mut x11.failed);
    report
}

/// Whether HDR is active on this session; see
/// [`hdr_active_for`] for why the answer is decided by transport and what the
/// X11 answer's one documented exception is.
///
/// Read-only; never changes display state.
///
/// # The Wayland answer gates a channel that now exists, which it did not before
///
/// `None` on Wayland is unchanged and still honest — Wayland is where Linux HDR
/// actually happens, and neither `zwlr_gamma_control_v1` nor anything else this
/// crate binds has a query for it. What changed with the `wlr-gamma-control`
/// backend is the *cost*: [`hdr_active_for`]'s own documentation used to argue
/// that `None` costs nothing "because a Wayland session has no `XRandR` gamma
/// channel to use it with in the first place", and that premise is gone. It now
/// costs the Wayland gamma channel, because [`crate::GammaSupport::Unknown`] does
/// not allow gamma and a caller that respects the verdict will plan an overlay.
///
/// That is the safe direction and it is not the finished one. The remedy is a real
/// probe rather than a better guess: the colour-management protocol hands each
/// output an image description whose `tf_named` names the transfer function, so a
/// `st2084_pq` or `hlg` output is knowably HDR.
///
/// Two caveats belong here rather than only in `docs/debt.md`, because this is the
/// doc a maintainer implementing it will read. It does **not** answer for every
/// output: the sibling `tf_power` event describes a pure power curve and names no
/// function, so that case stays `Unknown` as surely as a compositor with no colour
/// management at all. And while the XML ships in the `wayland-protocols` version
/// this workspace already builds against, it is behind the **`staging`** feature,
/// which neither the workspace manifest nor this crate enables — it resolves today
/// only through feature unification with another dependency, which is not
/// something to build on. `docs/debt.md` carries both, owed by the wave that gives
/// Linux a gamma sink to engage from.
#[must_use]
pub fn is_hdr_active() -> Option<bool> {
    hdr_active_for(session_transport())
}

/// Whether gamma dimming is safe on the current session.
///
/// A convenience over [`is_hdr_active`]: HDR ⇒ [`GammaSupport::UnsupportedHdr`],
/// SDR ⇒ [`GammaSupport::Supported`], an indeterminate probe ⇒
/// [`GammaSupport::Unknown`].
#[must_use]
pub fn display_supports_gamma() -> GammaSupport {
    gamma_support_from_hdr(is_hdr_active())
}

/// Which display server this session is on, from `WAYLAND_DISPLAY` and `DISPLAY`.
///
/// Read per call rather than cached: a cached answer is wrong for exactly the
/// session that changed under a running process, and two `getenv`s cost nothing.
pub(super) fn session_transport() -> Transport {
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    transport(SessionEnv {
        wayland_display: wayland_display.as_deref(),
        display: display.as_deref(),
    })
}

/// Resolve what this session can actually do.
///
/// Reads `WAYLAND_DISPLAY` and `DISPLAY`, connects to whichever the rule selects,
/// and returns the report. Never fails: every way this can go wrong is a
/// capability the report already has a reason for.
///
/// Only the **selected** transport is connected to. A Wayland session almost
/// always also has an Xwayland `DISPLAY`, and probing that too would produce a
/// second, contradictory answer for a screen the compositor already owns.
#[must_use]
pub fn probe_session() -> SurfaceCaps {
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    let env = SessionEnv {
        wayland_display: wayland_display.as_deref(),
        display: display.as_deref(),
    };
    match transport(env) {
        Transport::Wayland => {
            let (connected, interfaces) = wayland::probe();
            let borrowed: Vec<&str> = interfaces.iter().map(String::as_str).collect();
            resolve(
                env,
                &Probe {
                    connected,
                    globals: &borrowed,
                    randr: false,
                    // Not asked, and not consulted: a Wayland compositor *is* the
                    // compositing manager, so the rule's Wayland arm ignores this
                    // field. `false` is the honest value for a question that was
                    // never put to a server.
                    compositor: false,
                },
            )
        }
        Transport::X11 => {
            let answered = x11::probe();
            resolve(
                env,
                &Probe {
                    connected: answered.connected,
                    globals: &[],
                    randr: answered.randr,
                    compositor: answered.compositor,
                },
            )
        }
        // No display server: nothing to connect to, and the rule already says so.
        Transport::None => resolve(env, &Probe::default()),
    }
}

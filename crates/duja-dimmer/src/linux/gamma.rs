//! The opt-in gamma path on X11 (`RandR` CRTC ramps), and why Linux needs the
//! crash machinery macOS does not.
//!
//! Like the other two platforms, gamma is **not** on the default dimming path:
//! an overlay reaches true black without touching a transfer table, and gamma is
//! meaningless under HDR. This module is engaged only through the separate,
//! explicit API a caller has to reach for on purpose.
//!
//! Every decision is on the other side of [`crate::linux_gamma`], which is pure
//! and tested on all three CI lanes; this module connects, asks the server how
//! long the table is, and writes it. A GitHub runner has no X server, so what is
//! here is untested by construction and it is kept correspondingly thin.
//!
//! # A ramp here outlives the process, exactly as on Windows
//!
//! This is the property that decides the shape of everything above. The X server
//! holds each CRTC's gamma table as **server state** and does not reset it when
//! the client that wrote it disconnects — which is precisely why `xgamma -gamma
//! .5` and `redshift -O 3000` work as one-shot commands that set a ramp and exit.
//! So Linux sits with Windows, not with macOS: a crash mid-dim leaves the screen
//! dark with nothing running to undo it.
//!
//! What exists today is the manual rescue — [`restore_all`], which `duja
//! --restore` drives. What does not exist yet is the automatic one: the crash
//! marker and RAII guard Windows carries (`ScreenStateGuard`), which write a
//! marker before the first engage so a fresh start can detect a dirty exit. That
//! is deliberate rather than forgotten: **nothing engages a ramp on Linux yet**.
//! The engage path is the app's gamma sink, which the tray owns, and the tray is
//! not built on Linux until the ksni wave. Adding a guard now would be a guard
//! with no caller — the dead-code shape this crate has already been burnt by, and
//! whose tests would pin nothing. `docs/debt.md` carries it as owed to that wave.
//!
//! # Restoring identity clobbers a colour-temperature tool, and that is not new
//!
//! There is one LUT per CRTC and everyone shares it: `gammastep`, `redshift`,
//! GNOME's Night Light on X11, `colord`'s calibration curve, and Duja. Last writer
//! wins. So Duja engaging gamma flattens a user's warm evening tint, and
//! [`restore_identity`] flattens it again on the way out rather than putting it
//! back.
//!
//! The better construction is the one Apple's own `MacGamma` sample uses and that
//! the macOS sink documents as deferred: `GetCrtcGamma` **once**, before the first
//! write, then compose the dim into that baseline (`baseline[i] * factor`, which
//! is exactly right for a linear dim — it preserves the tint *and* darkens) and
//! write the baseline back on restore. It needs one thing this wave has nowhere to
//! put: somewhere to keep the baseline across calls, which is the guard that does
//! not exist yet. Same wave, same reason; `docs/debt.md` carries both together.

use std::sync::{Mutex, OnceLock, PoisonError};

use tracing::debug;
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::Window;
use x11rb::rust_connection::RustConnection;

use duja_core::dimmer::DimmerError;

use crate::gamma_support::{GammaSupport, gamma_support_from_hdr};
use crate::linux_caps::{SessionEnv, Transport, transport};
use crate::linux_gamma::{
    MAX_RAMP_SIZE, MIN_RAMP_SIZE, crtc_label, hdr_active_for, identity_ramp, ramp, xrandr_refusal,
};

use crate::gamma_support::RestoreReport;

/// A display whose gamma transfer table can be driven, identified by its `RandR`
/// **CRTC**.
///
/// The CRTC and not the output: two outputs driven by one CRTC are an X11 mirror,
/// and they share a framebuffer *and* a gamma table, so the CRTC is the
/// granularity the hardware actually has. It is also the token `linux::outputs`
/// stamps on every placed display, so the app's gamma sink can address this
/// without a second enumeration.
///
/// Holds an id and a label, so the value is cheap, [`Send`], and safe to store —
/// there is no handle to open or close, and the connection is shared.
#[derive(Debug, Clone)]
pub struct GammaDisplay {
    crtc: randr::Crtc,
    name: String,
}

impl GammaDisplay {
    /// Wrap a raw `RandR` CRTC id, labelled by the id alone.
    ///
    /// This is the constructor the app's gamma sink uses, because a `gamma_token`
    /// carries the id and nothing else. [`enumerate_gamma_displays`] builds richer
    /// labels naming the connectors, which is what a user reading a report needs.
    #[must_use]
    pub fn from_crtc(crtc: u32) -> Self {
        GammaDisplay {
            crtc,
            name: crtc_label(crtc, &[]),
        }
    }

    /// A human-readable name (`DP-1 (CRTC 63)`, or `CRTC-63` when the connectors
    /// are not known) for reporting.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw `RandR` CRTC id.
    #[must_use]
    pub fn crtc(&self) -> u32 {
        self.crtc
    }
}

/// Drive `display`'s gamma to scale output brightness by `factor`.
///
/// # Errors
/// [`DimmerError::Os`] if this session has no `XRandR` gamma channel at all (a
/// Wayland session is refused here rather than silently writing to Xwayland — see
/// [`xrandr_refusal`]), if the connection failed, if the CRTC reports a table
/// length nothing can be built for, or if the server rejected the write. The
/// caller falls back to overlay dimming.
///
/// **`Err` does not prove the ramp is not live.** The write is confirmed with a
/// round trip, so a connection that dies between the server applying the table
/// and the confirmation arriving reports a failure for a table that **is** on the
/// screen — and an X11 ramp is server state, so it stays there after this client
/// is gone. Narrow (`XKillClient`, a client resource limit, an `ssh -X` tunnel
/// dropping) and deliberately in this direction: the coordinator above does not
/// record a refused engage, so it retries, which recovers a ramp that never
/// landed and rewrites one that did. The residue is a ramp nothing restores if the
/// process then exits, which is the same gap the missing crash guard leaves.
pub fn set_gamma(display: &GammaDisplay, factor: f32) -> Result<(), DimmerError> {
    write_table(display, |size| ramp(factor, size))
}

/// Restore `display` to the identity transfer table (no dimming).
///
/// Identity, not "the user's curve": X11 has no server-side colour profile to
/// reload and Duja keeps no baseline yet, so this is the same end state Windows'
/// restore writes, and it clobbers a colour-temperature tool's ramp — see the
/// module docs for the composition this should eventually be.
///
/// # Errors
/// As [`set_gamma`].
pub fn restore_identity(display: &GammaDisplay) -> Result<(), DimmerError> {
    write_table(display, identity_ramp)
}

/// Write one CRTC's gamma table, sized by whatever the server says that CRTC's
/// table is.
///
/// `build` is the pure table builder — [`ramp`] at a factor, or
/// [`identity_ramp`] — applied to the length the server reports. The length is
/// re-read on **every** write rather than cached: it is one round trip on a local
/// socket, and a CRTC reconfigured since the last write can report a different
/// one, for which the server rejects the request outright rather than rescaling.
fn write_table(
    display: &GammaDisplay,
    build: impl FnOnce(u16) -> Option<Vec<u16>>,
) -> Result<(), DimmerError> {
    with_session(|session| {
        let connection = &session.connection;
        let size = connection
            .randr_get_crtc_gamma_size(display.crtc)
            .map_err(|e| Fault::connection("RandR GetCrtcGammaSize", &e))?
            .reply()
            .map_err(|e| Fault::reply("RandR GetCrtcGammaSize", &e))?
            .size;
        let Some(table) = build(size) else {
            return Err(Fault::refused(format!(
                "{} reports a gamma table of {size} entries, and only {MIN_RAMP_SIZE}..={MAX_RAMP_SIZE} \
                 can be written (0 is a CRTC with no gamma hardware)",
                display.name
            )));
        };
        // `.check()`, not a dropped cookie. `SetCrtcGamma` is a **void** request:
        // x11rb's `VoidCookie` discards its reply on drop, so a protocol error —
        // a BadMatch for a wrong-length table, a BadCrtc for a CRTC that has gone
        // away — would be delivered to the connection's event queue instead, and
        // this function would return `Ok(())` for a ramp that was never applied.
        // Reporting a live ramp that is not live is the exact failure the gamma
        // channel above is built to avoid: the coordinator would record the
        // factor, never retry, and never plan the overlay that would have dimmed
        // the display instead. It costs a round trip per write — `check` inserts
        // a sync and blocks for the answer — which is the price of knowing, and
        // is the second of the two this function makes.
        connection
            .randr_set_crtc_gamma(display.crtc, &table, &table, &table)
            .map_err(|e| Fault::connection("RandR SetCrtcGamma", &e))?
            .check()
            .map_err(|e| Fault::reply("RandR SetCrtcGamma", &e))?;
        Ok(())
    })
}

/// Enumerate the CRTCs currently **driving** something, each labelled by the
/// connectors on it.
///
/// This is the *addressing* surface: what Duja could dim, and what a caller may
/// map a display onto. A CRTC driving no output is skipped, because it shows
/// nothing. [`restore_all`] deliberately uses a different, wider walk — see
/// `restorable_crtcs` (private; a rescue must reach a CRTC that is currently
/// showing nothing, because its table survives being disabled).
///
/// Returns an empty vector (never an error) when this session has no `XRandR`
/// gamma channel or the connection failed — the graceful-degradation contract the
/// other two platforms' enumerations keep. A caller that needs to tell those two
/// apart (`--restore` does, since one is "nothing to do" and the other is "the
/// rescue could not run") must go through [`restore_all`], which reports the
/// failure instead of swallowing it.
#[must_use]
pub fn enumerate_gamma_displays() -> Vec<GammaDisplay> {
    match with_session(|session| collect_crtcs(session, Walk::Driving)) {
        Ok(displays) => displays,
        Err(e) => {
            debug!(error = %e, "no XRandR gamma displays");
            Vec::new()
        }
    }
}

/// Which CRTCs a walk should return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walk {
    /// Only CRTCs with at least one output attached: what can be dimmed.
    Driving,
    /// Every CRTC with a writable gamma table, attached or not: what can hold a
    /// stale ramp. See [`restorable_crtcs`].
    Restorable,
}

/// Every CRTC with a writable gamma table, whether or not it is driving an
/// output — the walk a **rescue** needs.
///
/// The wider walk is not tidiness, it is the module's headline property applied
/// consistently: an X11 gamma table is server state that survives its CRTC being
/// disabled, and the driver reloads it on the next modeset. So a CRTC with no
/// outputs is not "nothing to restore", it is "a ramp nobody can currently see".
///
/// The first draft of this module used the driving-only walk for both jobs, and
/// its review found the case that breaks: Duja dims an external monitor and
/// crashes, the user unplugs it (or closes the lid), and `duja --restore` then
/// reports a clean rescue while silently skipping the CRTC that still holds the
/// dark ramp — which comes back with the monitor, after the user has been told
/// the rescue worked. Telling someone their screen is fixed when it is not is
/// worse than a count that includes idle hardware, and the count is honest about
/// that: a restore report on Linux is measured in CRTCs, not displays, and an
/// idle one is named `CRTC-3` because it has no connector to name it by.
fn restorable_crtcs() -> Result<Vec<GammaDisplay>, DimmerError> {
    with_session(|session| collect_crtcs(session, Walk::Restorable))
}

/// The body of both walks, inside a session.
///
/// A failure of the **screen-resources** request is returned as a [`Fault`]: that
/// is how a dead cached connection is detected, and returning it is what drops
/// the connection so the next call reconnects.
///
/// Per-CRTC failures are treated by kind rather than uniformly swallowed. A
/// protocol error (a CRTC that has gone away mid-walk) skips that CRTC, because
/// one CRTC the server will not describe must not cost the others their restore.
/// A **connection** error is returned, because after the socket dies every
/// remaining CRTC would fail the same way and the walk would otherwise finish
/// `Ok` with a short list and put the dead connection back — deferring the
/// reconnect by a call and reporting a rescue that did nothing.
fn collect_crtcs(session: &Session, walk: Walk) -> Result<Vec<GammaDisplay>, Fault> {
    let connection = &session.connection;
    if !session.screen_resources_current {
        return Err(Fault::refused(format!(
            "this X server's RandR is older than {}.{}, so its CRTCs cannot be listed              (writing a ramp to a known CRTC still works: those requests are RandR 1.2)",
            RANDR_SCREEN_RESOURCES_CURRENT.0, RANDR_SCREEN_RESOURCES_CURRENT.1
        )));
    }
    // `GetScreenResourcesCurrent` reads the server's cached view; the plain
    // `GetScreenResources` re-probes every output over DDC, which costs on the
    // order of a second per connector on some drivers.
    let resources = connection
        .randr_get_screen_resources_current(session.root)
        .map_err(|e| Fault::connection("RandR GetScreenResourcesCurrent", &e))?
        .reply()
        .map_err(|e| Fault::reply("RandR GetScreenResourcesCurrent", &e))?;

    let timestamp = resources.config_timestamp;
    let mut displays = Vec::new();
    for crtc in resources.crtcs {
        let size = match connection
            .randr_get_crtc_gamma_size(crtc)
            .map_err(|e| Fault::connection("RandR GetCrtcGammaSize", &e))?
            .reply()
        {
            Ok(reply) => reply.size,
            Err(e) => {
                let fault = Fault::reply("RandR GetCrtcGammaSize", &e);
                if fault.connection_lost {
                    return Err(fault);
                }
                continue;
            }
        };
        if !(MIN_RAMP_SIZE..=MAX_RAMP_SIZE).contains(&size) {
            continue;
        }
        // A non-`Success` status leaves every other field of the reply undefined
        // per the protocol — `InvalidConfigTime` is what a hot-plug racing this
        // walk looks like — so `outputs` must not be read from it. An info the
        // server will not give up costs the CRTC its label, and on the restorable
        // walk that is all it costs: the ramp is still written.
        let info = connection
            .randr_get_crtc_info(crtc, timestamp)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .filter(|info| info.status == randr::SetConfig::SUCCESS);
        let outputs = info.map(|info| info.outputs).unwrap_or_default();
        if walk == Walk::Driving && outputs.is_empty() {
            continue;
        }
        let names = outputs
            .iter()
            .filter_map(|output| output_name(connection, *output, timestamp))
            .collect::<Vec<_>>();
        displays.push(GammaDisplay {
            crtc,
            name: crtc_label(crtc, &names),
        });
    }
    Ok(displays)
}

/// One output's connector name, for labelling. `None` for an output the server
/// will not describe, which costs the label a name and nothing else.
fn output_name(
    connection: &RustConnection,
    output: randr::Output,
    timestamp: x11rb::protocol::xproto::Timestamp,
) -> Option<String> {
    let info = connection
        .randr_get_output_info(output, timestamp)
        .ok()?
        .reply()
        .ok()
        .filter(|info| info.status == randr::SetConfig::SUCCESS)?;
    // `RandR` output names are ASCII in practice; lossy rather than a failure, so
    // a driver with an odd byte still contributes a readable label.
    Some(String::from_utf8_lossy(&info.name).into_owned())
}

/// The `failed` row name used when the rescue could not run at all, as opposed to
/// a named CRTC that would not take a ramp.
const CHANNEL_ROW: &str = "XRandR gamma channel";

/// Best-effort restore of identity gamma on every CRTC with a writable table.
///
/// Drives both `duja --restore` and, once the tray exists on Linux, recovery from
/// a dirty exit. Never fails as a whole: it reports which CRTCs it reset and which
/// it could not.
///
/// Its blast radius is every CRTC in the session, not only the ones Duja engaged —
/// the same width as the macOS restore and wider than the Windows one. That is
/// what makes it a rescue for a ramp any process left behind, and also what makes
/// it flatten a running `gammastep`'s tint (module docs).
///
/// # An empty clean report means "nothing to restore", and only that
///
/// This is the distinction the review of this module found missing, and it
/// matters because this is the **only** rescue Linux has. A caller reads an empty
/// clean report as "there was nothing here", so it must never also mean "the
/// rescue could not run":
///
/// - A session with no `XRandR` gamma channel — Wayland, or no display server —
///   returns an empty **clean** report. There is genuinely nothing to reset.
/// - Anything else that stops the walk — no `XAUTHORITY` (which is what `sudo
///   duja --restore` looks like, and it is the first thing a user with a dark
///   screen will try), a dead server, `RandR` missing or older than the
///   enumeration needs — is reported as a **failure**, with the reason, so the
///   command says so and exits non-zero.
///
/// Before this, every one of those collapsed into `Vec::new()` behind a `debug!`
/// that is off by default, and `duja --restore` told a user staring at a dark
/// screen that there was nothing to restore, then exited 0.
#[must_use]
pub fn restore_all() -> RestoreReport {
    let mut report = RestoreReport::default();
    // A transport refusal is not a failure: there is no channel here to rescue,
    // which is exactly what an empty clean report says.
    if xrandr_refusal(session_transport()).is_some() {
        return report;
    }
    let crtcs = match restorable_crtcs() {
        Ok(crtcs) => crtcs,
        Err(e) => {
            report.failed.push((CHANNEL_ROW.to_owned(), e.to_string()));
            return report;
        }
    };
    for display in crtcs {
        match restore_identity(&display) {
            Ok(()) => report.restored.push(display.name().to_owned()),
            Err(e) => report
                .failed
                .push((display.name().to_owned(), e.to_string())),
        }
    }
    report
}

/// Whether HDR is active on this session; see
/// [`hdr_active_for`] for why the answer is
/// decided by transport and what the X11 answer's one documented exception is.
///
/// Read-only; never changes display state.
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
fn session_transport() -> Transport {
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    transport(SessionEnv {
        wayland_display: wayland_display.as_deref(),
        display: display.as_deref(),
    })
}

/// Why one gamma request failed, and whether the connection survived it.
///
/// The second field is the whole reason this type exists rather than a bare
/// `String`: a protocol error (`BadCrtc` for a monitor that has just been
/// unplugged) is a per-request failure the next call should retry over the same
/// connection, while an I/O error means the socket is gone and every later call
/// would fail identically until something reconnects. Conflating them wedges the
/// gamma channel for the rest of the session after one server restart.
struct Fault {
    message: String,
    connection_lost: bool,
}

impl Fault {
    /// A failure to even queue the request.
    ///
    /// Only an `IoError` means the socket is gone. The other `ConnectionError`
    /// variants — an extension this build asked for and the server does not have,
    /// a request too large to encode, a reply this client could not parse, an
    /// allocation failure — all describe *this request* over a connection that is
    /// still perfectly usable, and throwing it away for one of them buys a
    /// needless connect, `.Xauthority` read and setup handshake on the next call,
    /// on the UI thread mid-drag.
    fn connection(context: &str, error: &ConnectionError) -> Self {
        Fault {
            message: format!("{context} failed: {error}"),
            connection_lost: matches!(error, ConnectionError::IoError(_)),
        }
    }

    /// A failure waiting for the answer. An `X11Error` is the server refusing
    /// this one request and leaves the connection usable; anything else is the
    /// connection itself.
    fn reply(context: &str, error: &ReplyError) -> Self {
        Fault {
            message: format!("{context} failed: {error}"),
            connection_lost: !matches!(error, ReplyError::X11Error(_)),
        }
    }

    /// A refusal decided here rather than by the server; the connection is fine.
    fn refused(message: String) -> Self {
        Fault {
            message,
            connection_lost: false,
        }
    }
}

/// The X connection every gamma call shares, the root its `RandR` requests are
/// addressed to, and what the server said it can do.
struct Session {
    connection: RustConnection,
    root: Window,
    /// Whether the negotiated `RandR` is at least 1.3, which is what
    /// `GetScreenResourcesCurrent` needs. The gamma requests themselves are 1.2,
    /// so a 1.2 server can still be **written** to; only the walk is unavailable,
    /// and it says so rather than answering an empty list forever.
    screen_resources_current: bool,
}

/// The process-wide gamma connection, opened on first use.
///
/// Shared rather than per-call because the caller is a slider drag: the app's
/// gamma coordinator re-engages whenever the factor changes, so a connection per
/// write would be a socket connect, an `.Xauthority` read and a setup handshake on
/// every frame, on the UI thread. The overlay backend's connection is deliberately
/// *not* reused — it lives on its own worker thread and is not reachable from
/// here — and a second client connection costs one file descriptor.
///
/// # It is drained, because "nothing selects events on it" is not enough
///
/// The first draft of this comment claimed the event queue stays empty because
/// this connection selects no events. That is false, and its review named the
/// counter-example: X11 `MappingNotify` is sent to **every** client and there is
/// no event mask that expresses disinterest in it, so every keyboard remap,
/// `setxkbmap`, or USB-keyboard hotplug pushes an entry into x11rb's `pending_events`,
/// which is an unbounded `VecDeque` drained only by a caller that polls. Tens of
/// bytes a time and this process may run for weeks, so [`with_session`] polls the
/// queue dry after every call rather than asserting an impossibility.
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

/// Run `f` against the shared connection, opening it if needed.
///
/// The session is **taken** for the duration and put back afterwards — unless the
/// call reported the connection lost, in which case it is dropped and the next
/// call reconnects. That is what lets the gamma channel survive an X server
/// restart, a session switch, or a `SIGHUP`ed display manager instead of failing
/// identically forever.
fn with_session<T>(f: impl FnOnce(&Session) -> Result<T, Fault>) -> Result<T, DimmerError> {
    // The transport gate comes first, before anything opens a socket: on a
    // Wayland session `DISPLAY` points at Xwayland and every request below would
    // succeed against CRTCs that are not on the path to any monitor.
    if let Some(reason) = xrandr_refusal(session_transport()) {
        return Err(DimmerError::Os(reason.to_owned()));
    }
    let cell = SESSION.get_or_init(|| Mutex::new(None));
    // A poisoned lock means some earlier call panicked while holding it. The
    // guarded value is an `Option<Session>` and every path through this function
    // leaves it consistent, so recovering the guard is right and refusing every
    // later gamma call for the life of the process is not.
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    let session = match guard.take() {
        Some(session) => session,
        None => open()?,
    };
    let outcome = f(&session);
    // Unsolicited events land here whether or not anything was selected (see
    // `SESSION`), and nothing else will ever read them. Errors are ignored on
    // purpose: a connection that cannot be polled is one whose real failure the
    // call above has already reported, and losing a drain is not worth masking it.
    while matches!(session.connection.poll_for_event(), Ok(Some(_))) {}
    match outcome {
        Ok(value) => {
            *guard = Some(session);
            Ok(value)
        }
        Err(fault) => {
            if !fault.connection_lost {
                *guard = Some(session);
            }
            Err(DimmerError::Os(fault.message))
        }
    }
}

/// The extension X.Org added so a client can tell Xwayland from an X server that
/// owns real outputs.
///
/// `xwaylandproto`: *"The XWAYLAND extension allows clients to reliably identify
/// whether an X server is Xwayland. Only Xwayland initializes this extension.
/// Thus, if the extension is present, the X server is Xwayland. Clients should
/// not need the protocol detailed in this document, a `QueryExtension` or
/// `ListExtensions` request is sufficient."* Presence is the whole answer, so no
/// request from the extension itself is ever issued.
const XWAYLAND_EXTENSION: &str = "XWAYLAND";

/// The `RandR` version `GetScreenResourcesCurrent` was added in.
const RANDR_SCREEN_RESOURCES_CURRENT: (u32, u32) = (1, 3);

/// Open the gamma connection, refuse Xwayland, and negotiate `RandR`.
fn open() -> Result<Session, DimmerError> {
    let (connection, screen) =
        x11rb::connect(None).map_err(|e| DimmerError::Os(format!("X11 connect failed: {e}")))?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .map(|screen| screen.root)
        .ok_or_else(|| DimmerError::Os(format!("X11 screen {screen} has no root window")))?;

    // The authoritative half of the Xwayland gate. `with_session` has already
    // asked the *environment*, which is cheap and skips this connect entirely —
    // but `Transport::X11`'s own documentation records that the environment
    // misfires (a systemd user unit, a sanitised environment, `sudo`, `ssh` with
    // `DISPLAY` exported, a `tmux` server older than the session), and a misfire
    // here is not a visible error: it is a ramp written to a virtual CRTC, an
    // `Ok(())`, and a screen that never changed. So the server is asked too, by
    // the query X.Org added for exactly this purpose.
    if connection
        .extension_information(XWAYLAND_EXTENSION)
        .map_err(|e| DimmerError::Os(format!("X11 QueryExtension failed: {e}")))?
        .is_some()
    {
        return Err(DimmerError::Os(
            "this X server is Xwayland: an XRandR ramp would land on a virtual \
             CRTC that is not on the path to any monitor"
                .to_owned(),
        ));
    }

    // Negotiate the extension version before issuing any of its requests; the
    // protocol leaves a client's behaviour undefined otherwise. `QueryVersion`
    // answers with the lower of what the client asked for and what the server
    // has, so the reply has to be *read* rather than merely awaited: a server
    // that tops out at 1.2 accepts every gamma request (they are all 1.2) and
    // refuses `GetScreenResourcesCurrent` (1.3), which would otherwise be an
    // enumeration that answers empty forever on a session whose writes work.
    let version = connection
        .randr_query_version(
            RANDR_SCREEN_RESOURCES_CURRENT.0,
            RANDR_SCREEN_RESOURCES_CURRENT.1,
        )
        .map_err(|e| DimmerError::Os(format!("this X server has no RandR extension: {e}")))?
        .reply()
        .map_err(|e| DimmerError::Os(format!("RandR QueryVersion failed: {e}")))?;
    Ok(Session {
        connection,
        root,
        screen_resources_current: (version.major_version, version.minor_version)
            >= RANDR_SCREEN_RESOURCES_CURRENT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The label a token-built display carries, which is all the app's gamma sink
    /// can give it. Runs on the Linux CI lane with no X server: it touches no
    /// connection.
    #[test]
    fn a_display_built_from_a_token_is_labelled_by_its_crtc() {
        let display = GammaDisplay::from_crtc(63);
        assert_eq!(display.crtc(), 63);
        assert_eq!(display.name(), "CRTC-63");
    }

    /// A session with no display server must degrade rather than block, panic, or
    /// claim success — which is the state every CI lane runs in, and the only
    /// state in which this module's real entry points can be exercised at all.
    ///
    /// # Why it returns instead of asserting the environment
    ///
    /// A developer runs this suite inside their own X session, where these calls
    /// reach a live server. Asserting "there is no display server" would red for
    /// them, and — far worse — [`restore_all`] would **write identity gamma to
    /// every CRTC on their machine**, flattening a running `gammastep`'s tint from
    /// a `cargo test`. A test must not change the screen of the person running
    /// it, so everything below the guard is skipped rather than adapted.
    ///
    /// What that costs is worth naming: on a developer's box this test pins
    /// nothing at all. Its coverage is the CI lanes, which is where it matters,
    /// because the headless refusals are the only behaviour of this module a
    /// runner can observe.
    #[test]
    fn a_session_with_no_display_server_degrades_rather_than_failing_loudly() {
        if session_transport() != Transport::None {
            return;
        }
        assert_eq!(is_hdr_active(), None, "no session, nothing to know");
        assert!(!display_supports_gamma().allows_gamma());
        assert!(
            enumerate_gamma_displays().is_empty(),
            "there is no server to enumerate CRTCs from"
        );
        let report = restore_all();
        assert!(report.restored.is_empty(), "nothing can have been restored");
        assert!(report.is_clean(), "nothing attempted cannot have failed");
        assert!(
            set_gamma(&GammaDisplay::from_crtc(1), 0.5).is_err(),
            "a ramp must never report success with no server to accept it"
        );
        assert!(restore_identity(&GammaDisplay::from_crtc(1)).is_err());
    }
}

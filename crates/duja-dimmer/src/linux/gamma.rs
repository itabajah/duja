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
use x11rb::connection::Connection as _;
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
    with_session(|connection, _root| {
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

/// Enumerate the CRTCs currently driving something, each labelled by the
/// connectors on it.
///
/// Returns an empty vector (never an error) when this session has no `XRandR` gamma
/// channel or the connection failed — the graceful-degradation contract the other
/// two platforms' enumerations keep.
///
/// A CRTC driving **no** output is skipped. That is the disabled CRTC every
/// multi-head GPU has spare: it shows nothing, so there is nothing on it to dim
/// and nothing to restore, and including it would inflate `duja --restore`'s
/// count past the number of monitors the user can see.
///
/// # Where that skip has a boundary
///
/// A CRTC keeps its table while it is disabled, so a ramp Duja engaged on a
/// monitor that was then **unplugged** is skipped by a restore run while it is
/// away, and comes back with the monitor. The window is narrow — it needs the
/// unplug to happen between the engage and the restore, and the next restore
/// after the replug catches it — but it is a window, and the alternative trades
/// it for a report that names CRTCs the user has no monitor for. Named here
/// rather than fixed, because the fix that closes it properly is the per-CRTC
/// baseline the module docs describe, which knows what Duja actually touched
/// instead of sweeping everything.
#[must_use]
pub fn enumerate_gamma_displays() -> Vec<GammaDisplay> {
    match with_session(collect_crtcs) {
        Ok(displays) => displays,
        Err(e) => {
            debug!(error = %e, "no `XRandR` gamma displays");
            Vec::new()
        }
    }
}

/// The body of [`enumerate_gamma_displays`], inside a session.
///
/// Only the first request can report a [`Fault`]: a failure there is how a dead
/// cached connection is detected, and returning it is what drops the connection
/// so the next call reconnects. Per-CRTC failures `continue` instead — one CRTC
/// the server will not describe must not cost the others their restore.
fn collect_crtcs(connection: &RustConnection, root: Window) -> Result<Vec<GammaDisplay>, Fault> {
    // `GetScreenResourcesCurrent` reads the server's cached view; the plain
    // `GetScreenResources` re-probes every output over DDC, which costs on the
    // order of a second per connector on some drivers.
    let resources = connection
        .randr_get_screen_resources_current(root)
        .map_err(|e| Fault::connection("RandR GetScreenResourcesCurrent", &e))?
        .reply()
        .map_err(|e| Fault::reply("RandR GetScreenResourcesCurrent", &e))?;

    let timestamp = resources.config_timestamp;
    let mut displays = Vec::new();
    for crtc in resources.crtcs {
        let Some(size) = connection
            .randr_get_crtc_gamma_size(crtc)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.size)
        else {
            continue;
        };
        if !(MIN_RAMP_SIZE..=MAX_RAMP_SIZE).contains(&size) {
            continue;
        }
        let Some(info) = connection
            .randr_get_crtc_info(crtc, timestamp)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            // A non-`Success` status leaves every other field of the reply
            // undefined per the protocol — `InvalidConfigTime` is what a hot-plug
            // racing this walk looks like — so `outputs` must not be read from it.
            .filter(|info| info.status == randr::SetConfig::SUCCESS)
        else {
            continue;
        };
        if info.outputs.is_empty() {
            continue;
        }
        let names = info
            .outputs
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

/// Best-effort restore of identity gamma on every CRTC that is driving something.
///
/// Drives both `duja --restore` and, once the tray exists on Linux, recovery from
/// a dirty exit. Never fails as a whole: it reports which displays it reset and
/// which it could not.
///
/// Its blast radius is every CRTC in the session, not only the ones Duja engaged —
/// the same width as the macOS restore and wider than the Windows one. That is
/// what makes it a rescue for a ramp any process left behind, and also what makes
/// it flatten a running `gammastep`'s tint (module docs).
#[must_use]
pub fn restore_all() -> RestoreReport {
    let mut report = RestoreReport::default();
    for display in enumerate_gamma_displays() {
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
    /// A failure to even queue the request: the connection is gone.
    fn connection(context: &str, error: &ConnectionError) -> Self {
        Fault {
            message: format!("{context} failed: {error}"),
            connection_lost: true,
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

/// The X connection every gamma call shares, and the root its `RandR` requests
/// are addressed to.
struct Session {
    connection: RustConnection,
    root: Window,
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
/// Nothing selects for events on it, so its event queue stays empty and cannot
/// grow behind an owner that never polls it.
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

/// Run `f` against the shared connection, opening it if needed.
///
/// The session is **taken** for the duration and put back afterwards — unless the
/// call reported the connection lost, in which case it is dropped and the next
/// call reconnects. That is what lets the gamma channel survive an X server
/// restart, a session switch, or a `SIGHUP`ed display manager instead of failing
/// identically forever.
fn with_session<T>(
    f: impl FnOnce(&RustConnection, Window) -> Result<T, Fault>,
) -> Result<T, DimmerError> {
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
    match f(&session.connection, session.root) {
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

/// Open the gamma connection and negotiate `RandR`.
fn open() -> Result<Session, DimmerError> {
    let (connection, screen) =
        x11rb::connect(None).map_err(|e| DimmerError::Os(format!("X11 connect failed: {e}")))?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .map(|screen| screen.root)
        .ok_or_else(|| DimmerError::Os(format!("X11 screen {screen} has no root window")))?;
    // Negotiate the extension version before issuing any of its requests; the
    // protocol leaves a client's behaviour undefined otherwise. The gamma
    // requests themselves are `RandR` 1.2, but `GetScreenResourcesCurrent` —
    // which the enumeration needs, and which is the only alternative to
    // re-probing every connector over DDC — is 1.3.
    connection
        .randr_query_version(1, 3)
        .map_err(|e| DimmerError::Os(format!("RandR QueryVersion failed: {e}")))?
        .reply()
        .map_err(|e| DimmerError::Os(format!("this X server has no RandR extension: {e}")))?;
    Ok(Session { connection, root })
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

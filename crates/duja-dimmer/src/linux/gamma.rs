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
//! the client that wrote it disconnects — which is precisely why
//! `xrandr --output DP-1 --gamma 1:1:0.5` works as a one-shot command that sets a
//! ramp and exits. (That one and only that one: `xgamma` drives
//! XFree86-VidModeExtension, and `redshift` picks between `randr`, `drm` and
//! `vidmode` backends at runtime, so neither is evidence about this API.) So Linux sits with Windows, not with macOS: a
//! crash mid-dim leaves the screen dark with nothing running to undo it.
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
    ChannelRefusal, ConnectionFault, MAX_RAMP_SIZE, RESCUE_WALK, SendFailure, Walk,
    connection_survives, crtc_label, hdr_active_for, identity_ramp, ramp, ramp_size_is_absent,
    randr_lists_crtcs, rescue_refusal_report, send_failure_is_absent_channel, walk_includes,
    writable_ramp_size, xrandr_refusal,
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
    // Both kinds of unavailability collapse here: a caller of `set_gamma` has one
    // response to either (no ramp, fall back to the overlay). Only `restore_all`
    // needs them apart, and it goes through `with_session` directly.
    with_session(|session| {
        let connection = &session.connection;
        let size = connection
            .randr_get_crtc_gamma_size(display.crtc)
            .map_err(|e| Fault::connection("RandR GetCrtcGammaSize", &e))?
            .reply()
            .map_err(|e| Fault::reply("RandR GetCrtcGammaSize", &e))?
            .size;
        let Some(table) = build(size) else {
            let why = if ramp_size_is_absent(size) {
                "has no gamma table to write"
            } else {
                "reports a table larger than one X11 request can carry"
            };
            return Err(Fault::refused(format!(
                "{} {why}: {size} entries is outside the writable range",
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
        //
        // What it buys is the *protocol* half and not the whole. `Success` here
        // means the server stored the table: `ProcRRSetCrtcGamma` discards
        // `RRCrtcGammaSet`'s return, which is the driver hook's own result, so a
        // write KMS refused reads as accepted. See `crate::gamma_is_advisory`,
        // which is `true` on Linux for exactly this.
        connection
            .randr_set_crtc_gamma(display.crtc, &table, &table, &table)
            .map_err(|e| Fault::connection("RandR SetCrtcGamma", &e))?
            .check()
            .map_err(|e| Fault::reply("RandR SetCrtcGamma", &e))?;
        Ok(())
    })
    .map_err(Into::into)
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
        Ok(walk) => walk.displays,
        Err(e) => {
            debug!(error = %e.reason(), "no XRandR gamma displays");
            Vec::new()
        }
    }
}

/// Every CRTC a rescue must reach, plus the ones it could not describe.
///
/// The walk is [`RESCUE_WALK`] rather than a literal, because using the narrower
/// one here is a defect with no symptom at the time — see that constant.
fn rescue_crtcs() -> Result<CrtcWalk, Unavailable> {
    with_session(|session| collect_crtcs(session, RESCUE_WALK))
}

/// What one walk of the server found: the CRTCs it can name, and the ones it
/// could not reach at all.
///
/// The second list exists because a CRTC the server refuses to describe is
/// **not** nothing. Round two of this module's review found the gap: a dock or
/// eGPU going away between `GetScreenResourcesCurrent` and the per-CRTC queries
/// leaves some ids answering `BadCrtc`, those were skipped silently, and a rescue
/// that reached one CRTC out of four reported itself clean with exit 0 while
/// three still held a dark ramp. A CRTC that could not be asked is a CRTC the
/// rescue did not run on, and it says so.
#[derive(Default)]
struct CrtcWalk {
    /// CRTCs the walk can address.
    displays: Vec<GammaDisplay>,
    /// `(name, reason)` for each CRTC the server would not describe. Ignored by
    /// the addressing walk, reported by the rescue.
    unreachable: Vec<(String, String)>,
}

/// The body of both walks, inside a session.
///
/// A failure of the **screen-resources** request ends the walk: that is how a
/// dead cached connection is detected, and returning it is what drops the
/// connection so the next call reconnects.
///
/// Per-CRTC failures are treated by kind rather than uniformly swallowed:
///
/// - a **connection** error ends the walk, because after the socket dies every
///   remaining CRTC fails the same way and finishing `Ok` with a short list would
///   put the dead connection back — deferring the reconnect by a call and
///   reporting a rescue that did nothing;
/// - a **protocol** error (a CRTC that went away mid-walk) skips that CRTC but is
///   recorded in [`CrtcWalk::unreachable`], so a rescue names it instead of
///   quietly reporting itself clean;
/// - a gamma size below the writable range is genuinely nothing — a CRTC with no
///   gamma table holds no ramp — and is skipped silently, while one *above* it is
///   recorded, because that CRTC has a table this crate cannot address.
///
/// Every CRTC is named before anything can reject it, which costs a `GetCrtcInfo`
/// on CRTCs that then turn out to have no gamma table. That is one round trip on
/// a local socket, on a path that runs per display event rather than per frame,
/// and it buys a report whose *failure* rows carry connector names — which are the
/// rows a user has to match against something on their desk.
fn collect_crtcs(session: &Session, walk: Walk) -> Result<CrtcWalk, Fault> {
    let connection = &session.connection;
    if !session.screen_resources_current {
        let (major, minor) = RANDR_SCREEN_RESOURCES_CURRENT;
        return Err(Fault::refused(format!(
            "this X server's RandR is older than {major}.{minor}, so its CRTCs cannot \
             be listed (writing a ramp to a known CRTC still works: those requests \
             are RandR 1.2)"
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
    let mut found = CrtcWalk::default();
    for crtc in resources.crtcs {
        // The label is built **first**, before anything can reject this CRTC.
        // Every row that reaches the user is one they have to find on their desk,
        // and the rows most worth naming are the failures; building the name only
        // on the success path left the oversized-table row reading `CRTC-63` with
        // no connector on it. A non-`Success` status leaves every other field of
        // the reply undefined per the protocol — `InvalidConfigTime` is what a
        // hot-plug racing this walk looks like — so `outputs` must not be read
        // from one. An info the server will not give up costs the CRTC its name
        // and nothing else: the ramp is still written.
        let info = match connection
            .randr_get_crtc_info(crtc, timestamp)
            .map_err(|e| Fault::connection("RandR GetCrtcInfo", &e))?
            .reply()
        {
            Ok(info) => Some(info).filter(|info| info.status == randr::SetConfig::SUCCESS),
            Err(e) => {
                let fault = Fault::reply("RandR GetCrtcInfo", &e);
                if fault.connection_lost {
                    return Err(fault);
                }
                None
            }
        };
        let outputs = info.map(|info| info.outputs).unwrap_or_default();
        let mut names = Vec::new();
        for output in &outputs {
            names.extend(output_name(connection, *output, timestamp)?);
        }
        let label = crtc_label(crtc, &names);

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
                found.unreachable.push((label, fault.message));
                continue;
            }
        };
        if !writable_ramp_size(size) {
            // Two different things wear one refusal, and only one of them is
            // nothing. A size below the minimum is a CRTC with no gamma table at
            // all: there is no ramp on it to reset, so skipping in silence is
            // honest. A size *above* the maximum is a CRTC that has a table this
            // crate cannot address, so it may be holding a dark ramp — dropping
            // that one silently is a rescue reporting itself clean over a
            // still-dimmed screen, which is this module's recurring defect in
            // miniature.
            if !ramp_size_is_absent(size) {
                found.unreachable.push((
                    label,
                    format!(
                        "reports a gamma table of {size} entries, larger than the {MAX_RAMP_SIZE}                          one X11 request can carry"
                    ),
                ));
            }
            continue;
        }
        if !walk_includes(walk, !outputs.is_empty()) {
            continue;
        }
        found.displays.push(GammaDisplay { crtc, name: label });
    }
    Ok(found)
}

/// One output's connector name, for labelling.
///
/// `Ok(None)` for an output the server will not describe, which costs the label a
/// name and nothing else; `Err` only when the connection itself is gone, which
/// ends the walk rather than silently shortening it.
fn output_name(
    connection: &RustConnection,
    output: randr::Output,
    timestamp: x11rb::protocol::xproto::Timestamp,
) -> Result<Option<String>, Fault> {
    let info = match connection
        .randr_get_output_info(output, timestamp)
        .map_err(|e| Fault::connection("RandR GetOutputInfo", &e))?
        .reply()
    {
        Ok(info) => info,
        Err(e) => {
            let fault = Fault::reply("RandR GetOutputInfo", &e);
            return if fault.connection_lost {
                Err(fault)
            } else {
                Ok(None)
            };
        }
    };
    if info.status != randr::SetConfig::SUCCESS {
        return Ok(None);
    }
    // `RandR` output names are ASCII in practice; lossy rather than a failure, so
    // a driver with an odd byte still contributes a readable label.
    Ok(Some(String::from_utf8_lossy(&info.name).into_owned()))
}

/// Best-effort restore of identity gamma on every CRTC with a writable table.
///
/// Drives both `duja --restore` and, once the tray exists on Linux, recovery from
/// a dirty exit. Never fails as a whole: it reports which CRTCs it reset and which
/// it could not.
///
/// Its blast radius is every CRTC on **this X screen**, not only the ones Duja
/// engaged — the same width as the macOS restore and wider than the Windows one.
/// That is what makes it a rescue for a ramp any process left behind, and also
/// what makes it flatten a running `gammastep`'s tint (module docs).
///
/// "This X screen" and not "the session" is exact: the connection is opened
/// against the default screen and `GetScreenResourcesCurrent` is per screen, so a
/// Zaphod-mode server (`:0.0` / `:0.1`, dual-GPU without Xinerama) has a second
/// screen this never walks — and would report a clean rescue with that screen
/// still dimmed. `linux::outputs` has the identical single-root limit, so the two
/// agree; `docs/debt.md` carries it rather than this quietly claiming otherwise.
///
/// # An empty clean report means "nothing to restore", and only that
///
/// This is the distinction the reviews of this module kept landing on, and it
/// matters because this is the **only** rescue Linux has. A caller reads an empty
/// clean report as "there was nothing here", so it must never also mean "the
/// rescue could not run" — and it must not swing the other way either, reporting
/// a *failure* for a session that simply has no gamma channel. The branch is
/// [`rescue_refusal_report`], which is pure and tested on every lane precisely
/// because this module got it wrong in both directions on consecutive rounds:
///
/// - A session with **no channel at all** — Wayland, an Xwayland server, an X
///   server with no `RandR` extension (so no per-CRTC gamma table exists), or no
///   display server — returns an empty **clean** report. Nothing here was dimmed
///   by this mechanism, so there is nothing to put back.
/// - A channel that **could not be reached** — no `XAUTHORITY` (which is what
///   `sudo duja --restore` looks like, and it is the first thing a user with a
///   dark screen tries), a dead server, or a `RandR` too **old** to list the
///   CRTCs — is a failure row with the reason, so the command says so and exits
///   non-zero. That last one is deliberately not in the bullet above: its gamma
///   writes are `RandR` 1.2 and work, so a ramp may well be live and only the
///   walk that would find it is missing.
/// - A CRTC the server would not describe is named too, rather than silently
///   dropped: a rescue that reached one CRTC of four must not report itself clean.
#[must_use]
pub fn restore_all() -> RestoreReport {
    // The cheap gate first: no connect at all for a session the environment
    // already rules out.
    if let Some(reason) = xrandr_refusal(session_transport()) {
        return rescue_refusal_report(ChannelRefusal::Absent(reason));
    }
    let walk = match rescue_crtcs() {
        Ok(walk) => walk,
        Err(unavailable) => {
            return rescue_refusal_report(match &unavailable {
                // The server answered and it is not one that owns any monitor.
                // Nothing to rescue, exactly as for a Wayland session — the same
                // answer by a different route, and the route must not change it.
                Unavailable::NoChannel(reason) => ChannelRefusal::Absent(reason),
                Unavailable::Failed(reason) => ChannelRefusal::Unreachable(reason),
            });
        }
    };
    let mut report = RestoreReport {
        restored: Vec::new(),
        failed: walk.unreachable,
    };
    for display in walk.displays {
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

/// Name what an `x11rb` connection error *is*, so the rule that acts on it can
/// live in the pure module.
///
/// This mapping and its sibling [`classify_send_error`] are the two places where
/// a wrong answer is not a wrong rule but a wrong input, and they are the reason
/// this module has a test at all: they are total functions over a constructible
/// enum, so the ubuntu lane can pin them with no X server. The decision they feed
/// has been wrong twice in this PR's history.
fn classify_connection_error(error: &ConnectionError) -> ConnectionFault {
    match error {
        ConnectionError::IoError(_) => ConnectionFault::Io,
        // `x11rb` answers this **only** from a `CheckState::Error` entry, which a
        // failed `QueryExtension` sets for the life of the connection — so it is
        // never a one-off. See `ConnectionFault::ExtensionLookupPoisoned`.
        ConnectionError::UnknownError => ConnectionFault::ExtensionLookupPoisoned,
        _ => ConnectionFault::PerRequest,
    }
}

/// Name what a failure to *send* a request is, for [`send_failure_is_absent_channel`].
///
/// `UnsupportedExtension` is the only variant that means the feature is absent:
/// `x11rb` produces it solely from `extension_information` answering `Ok(None)`,
/// which comes solely from a `QueryExtension` reply with `present == false`.
/// Everything else — including an I/O error *during* that lookup, which reaches
/// the same call site — is transport.
fn classify_send_error(error: &ConnectionError) -> SendFailure {
    match error {
        ConnectionError::UnsupportedExtension => SendFailure::ExtensionAbsent,
        _ => SendFailure::Transport,
    }
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
    /// Only an `IoError` means the socket is gone. The others describe *this
    /// request* over a connection that is still usable: `UnsupportedExtension`
    /// (the server lacks something this build asked for), `MaximumRequestLengthExceeded`,
    /// `ParseError` (a reply this client could not decode — it arrives here via
    /// `ReplyError`, not from the send side), and `UnknownError`, which is what
    /// x11rb answers for a `QueryExtension` that came back an X11 error. Throwing
    /// the connection away for any of them buys a needless connect, `.Xauthority`
    /// read and setup handshake on the next call, on the UI thread mid-drag.
    ///
    /// `ConnectionError` is `#[non_exhaustive]`, so a future variant lands in the
    /// survivable bucket. That is the right default here — the failure mode of
    /// guessing wrong is a slow reconnect, not a wedge — but it is a guess, and
    /// saying so is the point of this paragraph.
    fn connection(context: &str, error: &ConnectionError) -> Self {
        // One `matches!` to name the fault, then the rule is the pure one. The
        // `UnknownError` arm is not caution: `x11rb`'s extension manager caches a
        // failed `QueryExtension` as `CheckState::Error` and answers every later
        // lookup for that name with the same error **for the life of the
        // connection** — and every request in this module resolves the `RandR`
        // opcode first, so such a connection can never serve another one. Keeping
        // it would wedge the gamma channel until the process restarts.
        Fault {
            message: format!("{context} failed: {error}"),
            connection_lost: !connection_survives(classify_connection_error(error)),
        }
    }

    /// A failure waiting for the answer.
    ///
    /// An `X11Error` is the server refusing this one request and leaves the
    /// connection usable. Everything else **is** a `ConnectionError`, so it is
    /// classified by exactly the same rule rather than a second one: round two of
    /// this module's review found the two had drifted apart, so an unparseable
    /// reply (`ReplyError::ConnectionError(ParseError)`) tore down and rebuilt the
    /// session on every frame of a slider drag — through the path that actually
    /// fires, since almost every request here carries a reply.
    fn reply(context: &str, error: &ReplyError) -> Self {
        match error {
            // The server refusing one request. Routed through the same rule as
            // everything else rather than writing `false` here, so the module has
            // one place that decides whether a connection survives.
            ReplyError::X11Error(_) => Fault {
                message: format!("{context} failed: {error}"),
                connection_lost: !connection_survives(ConnectionFault::PerRequest),
            },
            ReplyError::ConnectionError(e) => Fault::connection(context, e),
        }
    }

    /// A refusal decided here rather than by the server; the connection is fine.
    fn refused(message: String) -> Self {
        Fault {
            message,
            connection_lost: !connection_survives(ConnectionFault::PerRequest),
        }
    }
}

/// Why a gamma call could not reach the channel: the distinction `--restore` has
/// to make, carried out of [`with_session`] rather than flattened to a string.
enum Unavailable {
    /// There is no `XRandR` gamma channel in this session at all. Nothing here
    /// was ever dimmed by this mechanism.
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
    /// Every public entry point but [`restore_all`] treats the two the same way —
    /// no ramp, fall back to the overlay — so they collapse to one error there.
    fn from(unavailable: Unavailable) -> Self {
        DimmerError::Os(match unavailable {
            Unavailable::NoChannel(reason) | Unavailable::Failed(reason) => reason,
        })
    }
}

/// The X connection every gamma call shares, the root its `RandR` requests are
/// addressed to, and what the server said it can do.
struct Session {
    connection: RustConnection,
    root: Window,
    /// Whether the negotiated `RandR` is at least 1.3, which is what
    /// `GetScreenResourcesCurrent` needs. The gamma requests themselves are 1.2,
    /// so a 1.2 server can still be **written** to; only the walk is unavailable.
    ///
    /// [`restore_all`] says so, with a failure row and a non-zero exit.
    /// [`enumerate_gamma_displays`] does **not** — it keeps the
    /// graceful-degradation contract its two sibling platforms have and answers an
    /// empty list with a `debug!`, forever, on such a server. That asymmetry is
    /// deliberate (a rescue must never look clean when it did not run, while an
    /// enumeration returning nothing is the honest answer for a caller that only
    /// wanted to know what it could dim) but it is an asymmetry, so it is written
    /// down rather than left to be discovered.
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
static SESSION: OnceLock<Mutex<SessionSlot>> = OnceLock::new();

/// What the process knows about its gamma connection.
#[derive(Default)]
enum SessionSlot {
    /// Not opened yet, or dropped after a connection failure so the next call
    /// reconnects.
    #[default]
    Empty,
    /// Live, and ready to lend to the next call.
    ///
    /// Boxed because a `RustConnection` is an order of magnitude larger than the
    /// other variants and this enum is moved in and out of the slot on every
    /// call; there is exactly one of these per process, so the indirection costs
    /// one allocation for the life of the program.
    Open(Box<Session>),
    /// This display server has no gamma channel and never will — it is Xwayland.
    ///
    /// Cached, and that is not an optimisation. `SESSION` exists because "the
    /// caller is a slider drag", and the gamma coordinator above deliberately
    /// retries a refused engage on every batch (that is the only way a display
    /// recovers). So re-deriving this would mean a socket connect, an
    /// `.Xauthority` read, a setup handshake and a `QueryExtension` **per frame**,
    /// on the UI thread, for as long as the user holds the slider — round two of
    /// this module's review called that a connect storm and it is one.
    ///
    /// Sticky for the life of the process because it is a property of the server
    /// this `DISPLAY` reaches, not of the moment.
    ///
    /// The limit of that, stated rather than implied: the per-call gate ahead of
    /// the cache is `xrandr_refusal(session_transport())`, which only tells X11
    /// from Wayland from nothing. A process whose `DISPLAY` moved from an Xwayland
    /// `:0` to a real X server `:1` stays `Transport::X11`, hits this cache, and
    /// is refused for the rest of its life. Nothing Duja does moves `DISPLAY`
    /// under itself, so this is a caveat rather than a defect — but it is a real
    /// one, and keying the cache on the `DISPLAY` string is what would close it.
    NoChannel(String),
}

/// Run `f` against the shared connection, opening it if needed.
///
/// The session is **taken** for the duration and put back afterwards — unless the
/// call reported the connection lost, in which case it is dropped and the next
/// call reconnects. That is what lets the gamma channel survive an X server
/// restart, a session switch, or a `SIGHUP`ed display manager instead of failing
/// identically forever.
fn with_session<T>(f: impl FnOnce(&Session) -> Result<T, Fault>) -> Result<T, Unavailable> {
    // The cheap gate first, before anything opens a socket: on a Wayland session
    // `DISPLAY` points at Xwayland, whose CRTCs are not on the path to any
    // monitor. Read per call, so a changed environment is caught immediately.
    if let Some(reason) = xrandr_refusal(session_transport()) {
        return Err(Unavailable::NoChannel(reason.to_owned()));
    }
    let cell = SESSION.get_or_init(|| Mutex::new(SessionSlot::default()));
    // A poisoned lock means some earlier call panicked while holding it. Every
    // path through this function leaves the slot consistent, so recovering the
    // guard is right and refusing every later gamma call for the life of the
    // process is not.
    let mut guard = cell.lock().unwrap_or_else(PoisonError::into_inner);
    let session = match std::mem::take(&mut *guard) {
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
            // Not cached: a connect that failed can succeed later (the server was
            // still starting, the display manager was restarting), and it is the
            // one failure a user might fix and retry without restarting Duja.
            Err(failed) => return Err(failed),
        },
    };
    let outcome = f(&session);
    // Unsolicited events land here whether or not anything was selected (see
    // `SESSION`), and nothing else will ever read them. Errors are ignored on
    // purpose: a connection that cannot be polled is one whose real failure the
    // call above has already reported, and losing a drain is not worth masking it.
    while matches!(session.connection.poll_for_event(), Ok(Some(_))) {}
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

/// The extension X.Org added so a client can tell Xwayland from an X server that
/// owns real outputs.
///
/// `xwaylandproto` 1.0: *"The XWAYLAND extension allows clients to reliably
/// identify whether an X server is Xwayland. Only Xwayland initializes this
/// extension. Thus, if the extension is present, the X server is Xwayland.
/// Clients should not need the protocol detailed in this document, a
/// `QueryExtension` or `ListExtensions` request is sufficient."* Presence is the
/// whole answer, so no request from the extension itself is ever issued.
///
/// # It is a peer of the environment check, not a replacement for it
///
/// The Xwayland on Ubuntu 22.04 LTS (supported into 2027) and Debian bookworm is
/// the 22.1 branch, whose source registers no such extension and whose
/// `hw/xwayland/meson.build` carries no `xwaylandproto` dependency — 24.1 does. So
/// on those distributions this query answers "not Xwayland" for a server that is.
///
/// The dates are consistent with that (the spec is 2022-07-29, 22.1.0 was February
/// 2022) but they do **not** establish it, and an earlier draft of this comment
/// argued from them alone: point releases backport features routinely, and 22.1.9
/// postdates the spec by more than a year. The source tree is the evidence.
///
/// So the two gates cover each other rather than one superseding the other: the
/// environment catches an old Xwayland with `WAYLAND_DISPLAY` set, and this catches
/// a new one where the environment was stripped. What is left uncovered is an old
/// Xwayland reached from a stripped environment, and nothing available to a client
/// closes that.
const XWAYLAND_EXTENSION: &str = "XWAYLAND";

/// The `RandR` version `GetScreenResourcesCurrent` was added in.
const RANDR_SCREEN_RESOURCES_CURRENT: (u32, u32) = (1, 3);

/// Open the gamma connection, refuse Xwayland, and negotiate `RandR`.
///
/// The two failure kinds are kept apart all the way out: an Xwayland server is a
/// session with **no channel** (nothing to rescue, and cacheable, because a server
/// does not stop being Xwayland), while a connect that failed is a session whose
/// channel could not be reached this time.
fn open() -> Result<Session, Unavailable> {
    let (connection, screen) = x11rb::connect(None)
        .map_err(|e| Unavailable::Failed(format!("X11 connect failed: {e}")))?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .map(|screen| screen.root)
        .ok_or_else(|| Unavailable::Failed(format!("X11 screen {screen} has no root window")))?;

    // The server-side half of the Xwayland gate. `with_session` has already asked
    // the *environment*, which is cheap and skips this connect entirely — but
    // `Transport::X11`'s own documentation records that the environment misfires
    // (a systemd user unit, a sanitised environment, `sudo`, `ssh` with `DISPLAY`
    // exported, a `tmux` server older than the session), and a misfire here is not
    // a visible error: it is a ramp written to a virtual CRTC, an `Ok(())`, and a
    // screen that never changed. See `XWAYLAND_EXTENSION` for what this covers
    // and, just as importantly, what it does not.
    if connection
        .extension_information(XWAYLAND_EXTENSION)
        .map_err(|e| Unavailable::Failed(format!("X11 QueryExtension failed: {e}")))?
        .is_some()
    {
        return Err(Unavailable::NoChannel(
            "this X server is Xwayland: an XRandR ramp would land on a virtual CRTC \
             that is not on the path to any monitor"
                .to_owned(),
        ));
    }

    // Negotiate the extension version before issuing any of its requests; the
    // protocol leaves a client's behaviour undefined otherwise. The reply has to
    // be *read* rather than merely awaited: a server that tops out at `RandR` 1.2
    // accepts every gamma request (they are all 1.2) and refuses
    // `GetScreenResourcesCurrent` (1.3), so without this the walk would answer an
    // empty list forever on a session whose writes work perfectly.
    //
    // `ProcRRQueryVersion` compares only the **major** version, then answers with
    // either the client's pair verbatim or the server's — so asking 1.3 of a 1.6
    // server yields 1.6, not 1.3. The `>=` below is what makes that harmless.
    // The two halves of this one call are different kinds of unavailable, and the
    // difference is exactly what `Unavailable` exists for. x11rb resolves an
    // extension before it can send the request, so a server with **no RandR at
    // all** fails on the send with `UnsupportedExtension` — and such a server has
    // no per-CRTC gamma mechanism, so Duja can never have dimmed anything through
    // it and "nothing to restore" is literally true. That is `NoChannel`.
    //
    // Compare a server whose RandR is merely older than 1.3 (handled at the walk,
    // not here): its gamma *writes* are 1.2 and work perfectly, so a ramp may well
    // be live and only the walk that would find it is missing. That one is a
    // failure the user has to see. Collapsing the two would report a rescue as
    // failed on a server that never had a channel to rescue.
    let version = connection
        .randr_query_version(
            RANDR_SCREEN_RESOURCES_CURRENT.0,
            RANDR_SCREEN_RESOURCES_CURRENT.1,
        )
        .map_err(|e| {
            // `x11rb` resolves an extension's opcode before it can encode the
            // request, and `major_opcode` propagates whatever
            // `extension_information` returns — so an I/O error during that lookup
            // arrives at this exact `map_err` too. Matching the variant rather
            // than trusting the call site is what keeps a dead connection out of
            // the "nothing to rescue" bucket; the rule itself is pure.
            if send_failure_is_absent_channel(classify_send_error(&e)) {
                Unavailable::NoChannel(
                    "this X server has no RandR extension, so it has no per-CRTC gamma table"
                        .to_owned(),
                )
            } else {
                Unavailable::Failed(format!("RandR QueryVersion could not be sent: {e}"))
            }
        })?
        .reply()
        .map_err(|e| Unavailable::Failed(format!("RandR QueryVersion failed: {e}")))?;
    Ok(Session {
        connection,
        root,
        screen_resources_current: randr_lists_crtcs(version.major_version, version.minor_version),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two `x11rb` mappings, pinned on the ubuntu lane.
    ///
    /// These are the highest-risk lines in the module — a correct rule fed a
    /// wrong input is still a wrong answer, and this is the classification that
    /// has been wrong in both directions inside one PR. They are total functions
    /// over a constructible enum, so unlike everything else here they cost
    /// nothing to test: no X server, no connection, no display.
    ///
    /// Reds a swap of the two `SendFailure` arms, which would report "nothing to
    /// restore" and exit 0 for a dead connection, and a swap of the
    /// `ConnectionFault` ones, which would keep a permanently poisoned connection
    /// and wedge the gamma channel for the life of the process.
    #[test]
    fn an_x11rb_error_is_named_for_what_it_actually_means() {
        assert_eq!(
            classify_send_error(&ConnectionError::UnsupportedExtension),
            SendFailure::ExtensionAbsent
        );
        for error in [
            ConnectionError::UnknownError,
            ConnectionError::MaximumRequestLengthExceeded,
            ConnectionError::IoError(std::io::Error::other("socket gone")),
        ] {
            assert_eq!(
                classify_send_error(&error),
                SendFailure::Transport,
                "{error:?} is not the absence of an extension"
            );
        }

        assert_eq!(
            classify_connection_error(&ConnectionError::IoError(std::io::Error::other("gone"))),
            ConnectionFault::Io
        );
        assert_eq!(
            classify_connection_error(&ConnectionError::UnknownError),
            ConnectionFault::ExtensionLookupPoisoned,
            "x11rb answers this only from a cached QueryExtension failure, which              is permanent for that connection"
        );
        assert_eq!(
            classify_connection_error(&ConnectionError::MaximumRequestLengthExceeded),
            ConnectionFault::PerRequest
        );
        assert_eq!(
            classify_connection_error(&ConnectionError::UnsupportedExtension),
            ConnectionFault::PerRequest,
            "a missing extension says nothing about the socket"
        );
    }

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

//! The decisions an `XRandR` gamma ramp makes that are arithmetic rather than X11.
//!
//! The fourth pure Linux module, and the same split as [`crate::linux_caps`],
//! [`crate::linux_outputs`] and [`crate::linux_overlay`]: everything decidable
//! without an X server lives here and is tested on all three CI lanes, and the
//! Linux-only `linux::gamma` module does nothing but carry the answers to the
//! wire. A GitHub runner has no X server, so anything left on the other side of
//! that boundary is untested code.
//!
//! Four decisions live here, and each of them fails **silently** if it is wrong:
//!
//! - [`xrandr_refusal`] — whether this session has an `XRandR` gamma channel at
//!   all. The dangerous answer is not "no": it is a Wayland session, where
//!   `DISPLAY` almost always points at Xwayland, the connection and the extension
//!   are both there, and the table would land on a virtual CRTC that is not on
//!   the path to any monitor.
//! - [`crtc_from_token`] — recovering the CRTC id from the display-surface token,
//!   where the failure is dimming *a different monitor*.
//! - [`ramp`] — the ramp itself, where a wrong curve is a wrongly-lit screen and
//!   a wrong **length** is a request the server rejects.
//! - [`hdr_active_for`] — whether a ramp means anything on this session at all.
//!
//! # An `XRandR` ramp outlives the process that set it
//!
//! Worth stating here rather than only in the backend, because it is the property
//! that decides whether a crash guard is needed — and it separates the two Linux
//! transports from each other rather than Linux from anything. **X11** is with
//! Windows. **Wayland** is not: a `zwlr_gamma_control_v1` dim lives only as long as
//! the client's object, and destroying that object — which the compositor does when
//! the socket closes — drops the client's colour transform, so the output is back
//! to its default with nothing left to rescue ([`crate::linux_wlr_gamma`]). Not
//! "the compositor puts the original back": it keeps no earlier client's table, and
//! saying otherwise is the misreading `#131` had to retract from six files. The X
//! server holds each CRTC's gamma table as server state and does **not** reset it
//! when the client that wrote it disconnects — which is exactly why
//! `xrandr --output DP-1 --gamma 1:1:0.5` works as a one-shot command that exits
//! immediately. (Not `xgamma`, which drives XFree86-VidModeExtension, and not
//! `redshift`, which chooses its backend at runtime — neither is evidence here.) So a Duja that
//! crashes mid-dim leaves the screen dark with nothing left running to undo it.
//!
//! `duja --restore` is the manual rescue and exists today. The automatic one — a
//! crash marker and an RAII guard, the machinery Windows carries — is owed to the
//! wave that gives Linux a tray to engage gamma from; nothing engages a ramp on
//! Linux until then. `docs/debt.md` carries it, scoped to this transport.

use duja_core::dimmer::clamp_gamma;

use crate::linux_caps::Transport;

/// The smallest gamma table that can express anything.
///
/// A `RandR` ramp of `size` entries maps input `i / (size - 1)` to `table[i]`, so a
/// one-entry table has no input axis at all and a zero-entry one is a CRTC that
/// reports no gamma hardware. Both are refused rather than written, because the
/// alternative is inventing a meaning for a table the protocol does not define.
pub const MIN_RAMP_SIZE: u16 = 2;

/// The largest gamma table `SetCrtcGamma` can carry over a core X11 connection.
///
/// **A property of the X11 transport, not of gamma tables**, which is why it is
/// named for it and why [`ramp`] no longer enforces it. `zwlr_gamma_control_v1`
/// sends its table over a **file descriptor** rather than in a request, so nothing
/// on the Wayland side is bounded by a request length; a compositor that reports a
/// larger `gamma_size` is asking for a table this crate must be able to build.
/// Enforcing it in the pure builder would refuse a Wayland ramp for an X11 reason.
/// [`writable_ramp_size`] is where the X11 writer and the X11 rescue walk both
/// read it.
///
/// Derived, not chosen, and the derivation is checked against x11rb's own
/// serialiser rather than read off the spec alone. The core protocol caps a
/// request at the server's `maximum_request_length`, which is `65535` four-byte
/// units — `262_140` bytes — on every X server in use. `SetCrtcGamma` spends 12
/// of those on its header (major and minor opcode, length, CRTC, size, two pad
/// bytes) and `6 * size` on the three `CARD16` channels, with a single trailing
/// pad to a four-byte boundary. So `6 * size <= 262_128`, i.e. `size <= 43_688`,
/// and `43_688` is even, so it needs no padding and lands exactly on the ceiling.
///
/// This is a sanity bound and not a hardware one: real CRTCs report 256, 1024 or
/// 4096, so nothing that exists is anywhere near it. Its job is to turn a garbage
/// `GetCrtcGammaSize` reply into a named refusal instead of a 393 KB allocation
/// and an opaque serialisation error — and to keep [`ramp`] total, since a caller
/// that trusted a `u16` blindly would allocate whatever the server said.
///
/// A server advertising a *smaller* maximum still rejects a large-but-legal ramp;
/// that surfaces as an ordinary connection error from the write, which is honest.
/// BIG-REQUESTS would raise the ceiling, and deliberately is not relied on: it is
/// an extension a server may not offer, and no hardware needs it.
pub const MAX_RAMP_SIZE: u16 = 43_688;

/// The value an identity ramp's last entry holds; `RandR` tables are `CARD16`.
const RAMP_MAX: f64 = 65_535.0;

/// Why this session has no `XRandR` gamma channel, or `None` when it has one.
///
/// # This is the cheap half of the Xwayland gate, and it is not the reliable one
///
/// "No display server" is the boring answer. The answer that matters is
/// **Wayland**, because on a Wayland session `DISPLAY` is almost always set — to
/// Xwayland — and the gamma path then runs against the wrong server. What is
/// certain is the half that makes it dangerous: `x11rb::connect` connects,
/// `RandR` is present, and Xwayland does not own the outputs. It renders into a
/// `wl_surface`, so whatever it does with a CRTC gamma table, that table is not
/// on the path to any monitor.
///
/// This function decides that from the **environment**, and the environment is a
/// heuristic this crate has already written down as fallible:
/// [`Transport::X11`]'s own documentation names the misfire — "a systemd user
/// unit, a sanitised environment" — a Wayland session that reaches the X11 arm
/// because `WAYLAND_DISPLAY` never made it into the process. `ssh` with
/// `DISPLAY` exported, `sudo`, and a `tmux` server older than the session are the
/// same shape. So this must not be the only gate, and it is not: `linux::gamma`
/// also asks the server, with the `XWAYLAND` extension query X.Org added for
/// exactly this purpose (*"Only Xwayland initializes this extension. Thus, if the
/// extension is present, the X server is Xwayland"*).
///
/// The two are **peers**, and it would be comfortable but wrong to call the
/// protocol one authoritative: only Xwayland **23.1 and later** register that
/// extension — the 22.1 branch that Ubuntu 22.04 LTS and Debian bookworm ship
/// carries no `xwaylandproto` dependency at all — so on those it answers "not
/// Xwayland" for a server that is. (Argue that from the source tree and not from
/// release dates: point releases backport, and 22.1.9 postdates the spec by over
/// a year.) Each gate covers the other's blind spot — this one catches an old Xwayland with `WAYLAND_DISPLAY` set, that one
/// catches a new Xwayland reached from a stripped environment — and an old
/// Xwayland from a stripped environment is covered by neither. See
/// `XWAYLAND_EXTENSION` in `src/linux/gamma.rs`.
///
/// What neither of them depends on is the thing this project cannot verify:
/// whether Xwayland *accepts* a gamma write or refuses it, which turns on the
/// gamma size it reports for its virtual CRTCs and needs a Wayland session to
/// read. Both branches are possible and only one is quiet — a refusal surfaces as
/// an error the caller can act on, while an acceptance is an `Ok(())` behind a
/// screen that never changed, and Duja would then record a ramp as live and later
/// "restore" it. Refusing before the write is correct under either branch.
///
/// A Wayland session's gamma channel is `wlr-gamma-control`, which is a different
/// backend and lives in [`crate::linux_wlr_gamma`] and `linux::wlr_gamma`.
/// [`crate::linux_wlr_gamma::wlr_gamma_refusal`] is this function's mirror image,
/// and `every_session_has_at_most_one_gamma_channel_and_a_desktop_has_one` pins
/// the pair against each other: no session may claim both, and every session with
/// a display server must claim one.
#[must_use]
pub const fn xrandr_refusal(transport: Transport) -> Option<&'static str> {
    match transport {
        Transport::X11 => None,
        Transport::Wayland => Some(
            "this session is Wayland: an XRandR ramp would land on Xwayland's \
             virtual CRTCs, which are not on the path to any monitor",
        ),
        Transport::None => Some("this session has no display server"),
    }
}

/// Recover an `XRandR` CRTC id from a display-surface token.
///
/// The token is `backend::DisplayGeom`'s `gamma_token`, which on Linux is the
/// decimal `RandR` CRTC id stamped by `linux::outputs` — the CRTC and not the
/// output, because two outputs sharing a CRTC are an X11 mirror and share one
/// gamma table, so the CRTC is the granularity the hardware actually has.
///
/// Returns `None` for anything else, and the caller treats that exactly as "this
/// display has no gamma device": it refuses the engage rather than dimming
/// something else. Four ways that matters, and none of them is hygiene:
///
/// - **`0` is `x11rb::NONE`**, which is what an output with no CRTC reports — a
///   disconnected monitor, or one the user disabled in their display settings.
///   `linux::outputs` already skips those, so a `0` reaching here means the token
///   was built from something that never had a rectangle.
/// - **A Wayland token is an output *name*** (`DP-1`), because that is the only
///   address Wayland grants. It must fail closed rather than parse to a plausible
///   number, and it is the token a session that got past [`xrandr_refusal`] by
///   some other route would carry.
/// - **A lenient parse of `"1abc"` would silently address CRTC 1**, a real CRTC
///   that is almost certainly a different monitor.
/// - **The other platforms' tokens** are a GDI device name and a
///   `CGDirectDisplayID`; the second *would* parse, which is exactly why the
///   platform gate above it is a transport check and not a build-time `cfg`.
///
/// # Its consumer is the app's Linux gamma sink, which lands with the tray
///
/// Nothing calls this yet, and that is stated rather than left to be discovered.
/// It is here, beside the backend, because this crate is also what *stamps* the
/// token — and the two halves are now the same pair of functions rather than two
/// prose descriptions of one format. Its shape is the macOS sink's
/// `display_id_from_token`, which is the fourth bullet above: the two tokens are
/// both decimal integers, so only the platform gate keeps them apart.
#[must_use]
pub fn crtc_from_token(token: &str) -> Option<u32> {
    match token.parse::<u32>() {
        // `x11rb::NONE`: never a CRTC, and what "no CRTC" is spelled as.
        Ok(0) | Err(_) => None,
        Ok(crtc) => Some(crtc),
    }
}

/// Stamp an `XRandR` CRTC id as a display-surface token.
///
/// The inverse of [`crtc_from_token`], and it exists as a function rather than an
/// inlined `to_string` because the review of this module found how quietly the
/// pair can drift apart. `linux::outputs` is what stamps the real token; **every
/// fixture in this crate and in `duja-app` writes it as `"crtc-63"`**, a shape
/// [`crtc_from_token`] rejects outright. A perfectly reasonable tidy-up making
/// the production stamp match what every test says a token looks like would have
/// made the gamma channel refuse *every* display, silently, with a fully green
/// suite — because nothing joined the two ends.
///
/// Now something does: one function each way, and `crtc_token_round_trips`
/// between them. The fixtures stay as they are, deliberately — they exercise the
/// join, which treats the token as opaque, and their being a *different* shape is
/// what makes the round-trip test the only thing holding the format.
#[must_use]
pub fn crtc_token(crtc: u32) -> String {
    crtc.to_string()
}

/// Build the gamma table that scales output brightness by `factor`, for a CRTC
/// whose table holds `size` entries.
///
/// `factor` is clamped into [`GAMMA_FLOOR`](duja_core::dimmer::GAMMA_FLOOR)`..=1.0`
/// first, so a ramp is never blacker than the crate's safety floor — which on
/// Linux is the *only* floor there is, because unlike Windows the X server
/// applies no anti-lockout validation of its own and will happily accept a table
/// of zeroes.
///
/// Entry `i` is the identity value `i * 65535 / (size - 1)` scaled by `factor` and
/// rounded, so `factor == 1.0` is the exact identity table and smaller factors
/// darken linearly. One channel is returned rather than three: the dim is neutral
/// by construction, and the caller sends the same slice as red, green and blue.
///
/// Returns `None` below [`MIN_RAMP_SIZE`] rather than guessing: a one-entry table
/// has no input axis and a zero-entry one is a CRTC reporting no gamma hardware,
/// and neither is a table this could scale.
///
/// **There is no upper bound here.** [`MAX_RAMP_SIZE`] is what one *X11 request*
/// can carry, which is a fact about that transport and not about gamma; the
/// Wayland channel hands its table over a file descriptor and has no such
/// ceiling. The X11 writer applies it, and [`writable_ramp_size`] is the shared
/// predicate. Total and never-panicking either way — `size` is a `u16`, so the
/// largest allocation this can be asked for is 64 Ki entries.
#[must_use]
pub fn ramp(factor: f32, size: u16) -> Option<Vec<u16>> {
    if size < MIN_RAMP_SIZE {
        return None;
    }
    let f = f64::from(clamp_gamma(factor));
    // `size >= MIN_RAMP_SIZE` (2), so the last index is at least 1 and this is
    // never a division by zero. `saturating_sub` rather than `- 1` for the
    // crate's arithmetic policy, not because it can saturate here.
    let last = f64::from(u32::from(size).saturating_sub(1));
    let mut table = Vec::with_capacity(usize::from(size));
    for i in 0..u32::from(size) {
        let identity = f64::from(i) * RAMP_MAX / last;
        let scaled = (identity * f).round().clamp(0.0, RAMP_MAX);
        // RATIONALE (clippy::cast_possible_truncation / cast_sign_loss):
        // `scaled` is rounded and clamped into [0.0, 65535.0], so the cast is
        // exact and can neither truncate nor lose a sign.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            table.push(scaled as u16);
        }
    }
    Some(table)
}

/// The identity gamma table for a CRTC of `size` entries (no dimming); what a
/// restore writes back. `None` for a size [`ramp`] refuses.
#[must_use]
pub fn identity_ramp(size: u16) -> Option<Vec<u16>> {
    ramp(1.0, size)
}

/// A human-readable name for a CRTC, given the outputs currently driven by it.
///
/// Only ever shown to a person — `duja --restore`'s report, and the log line for a
/// ramp the server refused. A bare CRTC id is a number the user has no way to map
/// to a monitor without `xrandr --verbose`, so the connector names lead and the id
/// follows for the case where two CRTCs drive identically-named outputs across
/// GPUs.
///
/// A CRTC with several outputs is an X11 mirror — one framebuffer, one gamma
/// table, several monitors — so every name is listed rather than the first one
/// picked: on a mirrored pair, "restored `DP-1`" would name one of the two
/// monitors that changed and quietly omit the other.
#[must_use]
pub fn crtc_label(crtc: u32, outputs: &[String]) -> String {
    if outputs.is_empty() {
        format!("CRTC-{crtc}")
    } else {
        format!("{} (CRTC {crtc})", outputs.join("+"))
    }
}

/// Whether HDR is active on this session, as far as the transport can say.
///
/// The Windows and macOS answers come from a real probe (DXGI colour space,
/// `NSScreen` EDR headroom). Linux has no equivalent query, so this is decided by
/// transport, and the two answers are deliberately asymmetric:
///
/// - **X11 ⇒ `Some(false)`.** The X protocol has no HDR path: there is no
///   pixel format, no colour-space negotiation and no metadata request in the core
///   protocol or in `RandR`, and no X11 desktop drives an HDR pipeline. An X11
///   desktop is SDR, and its CRTC LUT is the SDR pipeline's, which is exactly the
///   condition a gamma ramp needs.
/// - **Wayland or no display server ⇒ `None`** (⇒
///   [`GammaSupport::Unknown`](crate::GammaSupport::Unknown) ⇒ the caller plans an
///   overlay). Wayland is where Linux HDR actually happens — gamescope, `KWin`,
///   and `wp_color_management_v1` — and there is no way to ask from here. Unknown
///   is the honest answer, and it is the **safe** one rather than the free one.
///
///   An earlier draft of this paragraph said it "costs nothing, because a Wayland
///   session has no `XRandR` gamma channel to use it with in the first place". The
///   premise was true and is not any more: that session has a
///   `zwlr_gamma_control_v1` channel now ([`crate::linux_wlr_gamma`]), so this
///   verdict is what refuses it. Refusing is still right — a ramp under HDR is at
///   best ignored and at worst a display Duja believes it has dimmed and has not —
///   but it is now a cost rather than a freebie, and `docs/debt.md` carries the
///   remedy: `wp_color_management_v1`'s per-output `tf_named`, which answers the
///   question instead of guessing at it.
///
/// # The X11 answer has one documented exception, and it does not change it
///
/// A fullscreen Vulkan application on the NVIDIA proprietary driver can put an X11
/// display into HDR mode for as long as it is in the foreground. That does not
/// make `Some(false)` wrong for Duja's purposes: it is transient and
/// per-application rather than a property of the session, the desktop underneath
/// is still SDR, and a fullscreen-exclusive window is already a documented limit
/// for the overlay path too — Duja cannot dim what it cannot cover. Reporting
/// `Unknown` for every X11 session to model a state no ordinary desktop is ever
/// in would refuse the gamma channel to everybody.
#[must_use]
pub const fn hdr_active_for(transport: Transport) -> Option<bool> {
    match transport {
        Transport::X11 => Some(false),
        Transport::Wayland | Transport::None => None,
    }
}

/// Which CRTCs a walk of the display server should return.
///
/// Two jobs, two answers, and conflating them is a defect in each direction — so
/// the predicate is [`walk_includes`], here, rather than an `if` in the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// Only CRTCs with an output attached: what Duja could *dim*. A CRTC showing
    /// nothing cannot be given an overlay or addressed by a display.
    Driving,
    /// Every CRTC with a writable table, attached or not: what could be holding a
    /// *stale ramp*. A gamma table survives its CRTC being disabled, so a monitor
    /// unplugged after a crash is on a CRTC that is driving nothing and is
    /// nonetheless exactly what a rescue exists to reach.
    Restorable,
}

/// The walk a rescue pass must use.
///
/// Named rather than written inline at the call site, because using the narrower
/// walk there is a defect with **no visible symptom at the time**: `duja
/// --restore` reports a clean rescue and the dark ramp comes back with the
/// monitor, after the user has been told it worked. That is the shape the first
/// review of this module found, and a constant with a test on it is what stops
/// it coming back.
pub const RESCUE_WALK: Walk = Walk::Restorable;

/// Whether a walk includes a CRTC, given whether anything is attached to it.
#[must_use]
pub const fn walk_includes(walk: Walk, has_outputs: bool) -> bool {
    match walk {
        Walk::Driving => has_outputs,
        Walk::Restorable => true,
    }
}

/// Why an X11 request could not be **sent**, in the terms this crate decides on.
///
/// The backend maps one `x11rb` variant onto this and the rule lives here, which
/// is the split the rest of the module already uses. It is here specifically
/// because this is the decision that has been wrong twice in one PR — once by
/// putting a missing extension on the failure side, and once by putting a dead
/// connection on the *nothing to rescue* side, which is far worse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailure {
    /// The server does not have the extension at all. `x11rb` answers this only
    /// when `QueryExtension` came back with `present == false`.
    ExtensionAbsent,
    /// Anything else on the send path: the socket, the encoding, or a
    /// `QueryExtension` that itself failed. **Never** the absence of a feature.
    Transport,
}

/// Whether a send failure means this session never had a gamma channel.
///
/// Only a genuinely absent extension does. Everything else is a channel that
/// might exist and might be holding a ramp, so it has to be reported rather than
/// dismissed — `duja --restore` printing "nothing to restore" and exiting 0 at a
/// dark screen is the failure this whole distinction exists to prevent.
#[must_use]
pub const fn send_failure_is_absent_channel(failure: SendFailure) -> bool {
    matches!(failure, SendFailure::ExtensionAbsent)
}

/// What went wrong on a connection, in the terms this crate decides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFault {
    /// The socket is gone.
    Io,
    /// The connection's extension lookup is permanently poisoned: once a
    /// `QueryExtension` fails, `x11rb` caches the failure and answers every later
    /// lookup for that name with the same error **for the life of the
    /// connection**. Since every request in the gamma backend resolves the
    /// `RandR` opcode first, such a connection can never serve another one.
    ExtensionLookupPoisoned,
    /// A per-request failure that leaves the connection usable: a request too
    /// large to encode, a feature the server does not have, or a reply this client
    /// could not parse.
    ///
    /// That last one has an exception worth knowing rather than hiding: a parse
    /// failure on a `QueryExtension` **reply** also poisons the lookup, so the
    /// connection is in fact finished. It is not special-cased because it
    /// self-heals in one call — the next request's lookup returns
    /// [`ExtensionLookupPoisoned`](Self::ExtensionLookupPoisoned) and the
    /// connection is dropped then — so the cost is one wasted round trip rather
    /// than a wedge.
    PerRequest,
}

/// Whether the connection survives this fault and can be reused.
///
/// The failure directions are deliberately asymmetric. Discarding a healthy
/// connection costs one reconnect; **keeping a dead one costs the gamma channel
/// for the life of the process**, because nothing else ever replaces it. So a
/// fault that might be permanent is treated as permanent.
#[must_use]
pub const fn connection_survives(fault: ConnectionFault) -> bool {
    matches!(fault, ConnectionFault::PerRequest)
}

/// Whether a `RandR` of this version can list a screen's CRTCs.
///
/// `GetScreenResourcesCurrent` is `RandR` 1.3. Every gamma request is 1.2, so a
/// 1.2 server can still be written to — which is why this is a separate question
/// from "is there a channel" and lands on the *failure* side of a rescue rather
/// than the *nothing to do* side.
#[must_use]
pub const fn randr_lists_crtcs(major: u32, minor: u32) -> bool {
    major > 1 || (major == 1 && minor >= 3)
}

/// Whether a CRTC's reported gamma-table length is one **X11** can write.
///
/// Both bounds, and it is now the only place the upper one is applied: [`ramp`]
/// enforces the floor alone, because that is the half that is a property of gamma
/// tables rather than of a transport. So this is what the X11 writer and the X11
/// rescue walk share, and what stops them drifting apart.
///
/// The walk also has to tell the two failures apart, because they are not the same
/// thing to a rescue: [`ramp_size_is_absent`] says which.
#[must_use]
pub const fn writable_ramp_size(size: u16) -> bool {
    MIN_RAMP_SIZE <= size && size <= MAX_RAMP_SIZE
}

/// Whether a size this crate cannot write means the CRTC holds **no ramp at all**
/// (so a rescue may skip it in silence) rather than one it cannot reach.
///
/// A size below [`MIN_RAMP_SIZE`] is a CRTC with no gamma hardware: there is
/// nothing on it to reset. A size *above* [`MAX_RAMP_SIZE`] is the opposite — that
/// CRTC has a table, may well be holding a dark ramp, and this crate simply cannot
/// address it. Skipping the second in silence is a rescue reporting itself clean
/// with a screen still dimmed, which is this module's recurring defect in
/// miniature.
#[must_use]
pub const fn ramp_size_is_absent(size: u16) -> bool {
    size < MIN_RAMP_SIZE
}

/// Why a rescue pass is over before it has touched a CRTC.
///
/// Deliberately has **no** "the channel is open" variant: that case is the caller
/// not calling this at all, so there is no arm here that cannot fire and no
/// `unwrap_or_default` guarding a branch that never happens. The distinction it
/// *does* carry is the whole point — one of these is "there is nothing here to
/// rescue" and the other is "the rescue could not run", and a user staring at a
/// dark screen needs them told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRefusal<'a> {
    /// There is no `XRandR` gamma channel in this session **at all** — a Wayland
    /// compositor, an Xwayland server, no display server, or an X server with no
    /// `RandR` extension (which therefore has no per-CRTC gamma table). Nothing
    /// here was ever dimmed by this mechanism, so there is nothing to put back.
    Absent(&'a str),
    /// There should be a channel and it could not be reached: no `XAUTHORITY`, a
    /// dead server, or a `RandR` too **old** to list the CRTCs — note that one is
    /// not the bullet above: its gamma *writes* are 1.2 and work, so a ramp may
    /// well be live and only the walk that would find it is missing. Something may
    /// well be dimmed and
    /// this pass could not touch it.
    Unreachable(&'a str),
}

/// What a rescue pass reports when it never got as far as a CRTC.
///
/// Pure because it is a *decision* and it was got wrong once in each direction.
/// The first version of this module collapsed every refusal into an empty report,
/// so `sudo duja --restore` — sudo drops `XAUTHORITY`, and sudo is the first thing
/// anyone with a dark screen tries — printed "nothing to restore" and exited 0.
/// The fix for that then over-corrected: an Xwayland session, which genuinely has
/// nothing to rescue, came back as a *failure* with a non-zero exit, contradicting
/// this module's own documentation and the QA checklist written beside it.
///
/// Both mistakes are this one branch, so it lives here with a test on each arm
/// rather than in the backend where no CI lane can reach it.
#[must_use]
pub fn rescue_refusal_report(refusal: ChannelRefusal<'_>) -> crate::RestoreReport {
    match refusal {
        // Nothing to reset, and saying so is the truth rather than a shrug.
        ChannelRefusal::Absent(_) => crate::RestoreReport::default(),
        // A failure the user has to see, with the reason and a non-zero exit.
        ChannelRefusal::Unreachable(reason) => crate::RestoreReport {
            restored: Vec::new(),
            failed: vec![(CHANNEL_ROW.to_owned(), reason.to_owned())],
        },
    }
}

/// The `failed` row name for a rescue that could not run at all, as opposed to a
/// named CRTC that would not take a ramp.
pub const CHANNEL_ROW: &str = "XRandR gamma channel";

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::dimmer::GAMMA_FLOOR;

    #[test]
    fn only_an_x11_session_has_an_xrandr_gamma_channel() {
        assert_eq!(xrandr_refusal(Transport::X11), None);
        assert!(xrandr_refusal(Transport::None).is_some());
    }

    /// The refusal that earns this function: a Wayland session reaches every
    /// `XRandR` request through Xwayland and every one of them succeeds, so nothing
    /// downstream can detect the no-op. Reds a gate that only checks "can I open
    /// an X connection".
    #[test]
    fn a_wayland_session_is_refused_and_says_why() {
        let reason = xrandr_refusal(Transport::Wayland).expect("Wayland has no `XRandR` channel");
        assert!(
            reason.contains("Xwayland"),
            "the reason must name the trap, not just say no: {reason}"
        );
    }

    #[test]
    fn a_crtc_token_parses() {
        assert_eq!(crtc_from_token("63"), Some(63));
        assert_eq!(crtc_from_token("1"), Some(1));
    }

    /// The only thing holding the token format across the two crates that use it.
    ///
    /// Reds the tidy-up the review of this module identified as reachable and
    /// silent: making the stamp `format!("crtc-{crtc}")`, to match what every
    /// fixture in `linux_outputs` and `backend` writes, would leave the whole
    /// suite green while the gamma channel refused every display on Linux.
    ///
    /// One level of the gap stays open and is worth naming rather than implying
    /// away: this pins the two *functions* against each other, not the call site.
    /// The production stamp is in `linux::outputs`, which is `cfg(linux)`-only and
    /// needs an X server, so an edit that bypasses [`crtc_token`] there is caught
    /// by nothing here. What closes that is a reader noticing the comment at the
    /// call site, which is why one is there.
    #[test]
    fn crtc_token_round_trips() {
        for crtc in [1u32, 63, 4096, u32::MAX] {
            assert_eq!(
                crtc_from_token(&crtc_token(crtc)),
                Some(crtc),
                "CRTC {crtc} does not survive its own token"
            );
        }
    }

    /// Every token shape that must fail closed rather than address a CRTC.
    #[test]
    fn a_token_that_is_not_a_crtc_is_refused() {
        for token in [
            // `x11rb::NONE` — what "this output has no CRTC" is spelled as.
            "0",
            // A Wayland token: an output name is the only address Wayland grants.
            "DP-1",
            "eDP-1",
            // The Windows token.
            r"\\.\DISPLAY1",
            // A lenient parse would address CRTC 1 — a real, different monitor.
            "1abc",
            " 63",
            "63 ",
            "-63",
            "",
        ] {
            assert_eq!(crtc_from_token(token), None, "token {token:?} was accepted");
        }
    }

    /// A 256-entry identity table must be exactly the Windows one: entry `i` is
    /// `i * 257`, because `65535 / 255` is 257 with no remainder.
    ///
    /// The literal is deliberate and so is its limit. It pins this ramp against
    /// the constant the Windows ramp is *documented* to produce, which is the
    /// figure `MIN_ACCEPTED_GAMMA`'s whole derivation rests on — not against
    /// `win::gamma::gamma_ramp` itself, which does not compile on the lanes this
    /// test runs on. So a change to the Windows ramp would not red this; a change
    /// to *this* ramp that broke the correspondence would.
    #[test]
    fn a_256_entry_identity_matches_the_windows_ramp() {
        let table = identity_ramp(256).expect("256 is a legal size");
        assert_eq!(table.len(), 256);
        for (i, &value) in table.iter().enumerate() {
            let expected = u16::try_from(i).expect("i < 256") * 257;
            assert_eq!(value, expected, "entry {i}");
        }
    }

    /// The identity spans the full `CARD16` range whatever the table length —
    /// 1024 and 4096 are both real hardware, and a ramp built for 256 and sent to
    /// a 1024-entry CRTC is a rejected request, not a dimmer screen.
    #[test]
    fn every_identity_table_spans_the_full_range() {
        for size in [MIN_RAMP_SIZE, 256, 1024, 4096, MAX_RAMP_SIZE] {
            let table = identity_ramp(size).expect("a legal size");
            assert_eq!(table.len(), usize::from(size), "size {size}");
            assert_eq!(table.first(), Some(&0), "size {size} must start at black");
            assert_eq!(
                table.last(),
                Some(&65535),
                "size {size} must end at full scale"
            );
        }
    }

    #[test]
    fn a_dimmed_table_is_the_identity_scaled_and_stays_monotone() {
        let table = ramp(0.5, 256).expect("256 is a legal size");
        assert_eq!(table.first(), Some(&0));
        // 65535 * 0.5 = 32767.5, rounded away from zero.
        assert_eq!(table.last(), Some(&32768));
        for pair in table.windows(2) {
            let (previous, next) = (pair.first(), pair.last());
            assert!(previous <= next, "a gamma table must not go backwards");
        }
    }

    /// The factor is clamped before it reaches the arithmetic, so no table this
    /// builds is ever blacker than the crate's floor — the only floor Linux has,
    /// since the X server validates nothing.
    #[test]
    fn the_factor_is_clamped_to_the_safety_floor() {
        let floored = ramp(0.0, 256).expect("256 is a legal size");
        let at_floor = ramp(GAMMA_FLOOR, 256).expect("256 is a legal size");
        assert_eq!(floored, at_floor, "0.0 must clamp up to the floor");
        assert!(
            floored.last() > Some(&0),
            "the floor must still light the screen"
        );

        let nan = ramp(f32::NAN, 256).expect("256 is a legal size");
        assert_eq!(
            nan,
            identity_ramp(256).expect("256 is a legal size"),
            "NaN must map to identity, never to a darker screen"
        );

        let above = ramp(2.0, 256).expect("256 is a legal size");
        assert_eq!(above, identity_ramp(256).expect("256 is a legal size"));
    }

    /// A size the protocol cannot express is refused rather than guessed at. The
    /// zero case is a CRTC reporting no gamma hardware and the one case has no
    /// input axis; both would otherwise be a division by zero or an invented
    /// meaning.
    #[test]
    fn an_impossible_table_size_is_refused() {
        assert_eq!(ramp(0.5, 0), None, "a CRTC with no gamma table");
        assert_eq!(ramp(0.5, 1), None, "a table with no input axis");
        // And nothing above: a size only one transport cannot carry is still a
        // table, which `a_table_too_large_for_an_x11_request_is_still_a_table`
        // pins from the other side.
        assert!(ramp(0.5, MIN_RAMP_SIZE).is_some(), "the floor is inclusive");
        assert!(ramp(0.5, u16::MAX).is_some());
        assert!(ramp(0.5, MAX_RAMP_SIZE).is_some());
    }

    /// **The bound `ramp` carried is X11's *request length*, not a property of
    /// gamma tables.** `SetCrtcGamma` is a core request and so is capped by
    /// `maximum_request_length`; a `zwlr_gamma_control_v1` ramp travels over a
    /// **file descriptor**, which has no such ceiling. A compositor reporting a
    /// `gamma_size` above it is asking for a table this module must be able to
    /// build, and refusing it here would be refusing a Wayland ramp for an X11
    /// reason. The transport's bound belongs to the transport.
    #[test]
    fn a_table_too_large_for_an_x11_request_is_still_a_table() {
        let size = MAX_RAMP_SIZE.saturating_add(1);
        let table = ramp(0.5, size).expect("a size only X11 cannot carry is still a table");
        assert_eq!(table.len(), usize::from(size));
        assert!(identity_ramp(size).is_some());
        // ...and the X11 writer is what has to refuse it.
        assert!(!writable_ramp_size(size));
    }

    /// The derivation in [`MAX_RAMP_SIZE`]'s docs, asserted rather than trusted:
    /// the header plus three `CARD16` channels must fit the core protocol's
    /// 262_140-byte request ceiling, and one more entry must not.
    #[test]
    fn the_ramp_ceiling_is_the_largest_request_that_fits() {
        const HEADER: usize = 12;
        const LIMIT: usize = 262_140;
        let bytes = |size: u16| HEADER.saturating_add(usize::from(size).saturating_mul(6));
        assert!(bytes(MAX_RAMP_SIZE) <= LIMIT, "the ceiling must fit");
        assert!(
            bytes(MAX_RAMP_SIZE.saturating_add(1)) > LIMIT,
            "the ceiling must be the largest that fits"
        );
    }

    #[test]
    fn a_crtc_with_no_outputs_is_labelled_by_id() {
        assert_eq!(crtc_label(63, &[]), "CRTC-63");
    }

    #[test]
    fn a_labelled_crtc_leads_with_its_connector() {
        assert_eq!(crtc_label(63, &["DP-1".to_owned()]), "DP-1 (CRTC 63)");
    }

    /// A mirrored pair shares one CRTC and one gamma table, so both monitors
    /// change and both must be named. Reds a label that takes the first output.
    #[test]
    fn a_mirrored_crtc_names_every_output_it_drives() {
        let outputs = ["DP-1".to_owned(), "HDMI-A-1".to_owned()];
        assert_eq!(crtc_label(7, &outputs), "DP-1+HDMI-A-1 (CRTC 7)");
    }

    /// The rescue walk must reach a CRTC that is driving nothing, and the
    /// addressing walk must not.
    ///
    /// Reds `RESCUE_WALK = Walk::Driving`, which is the round-1 defect: a monitor
    /// unplugged between a crash and the rescue sits on a CRTC with no outputs,
    /// its ramp is skipped, `--restore` reports clean, and the dark table comes
    /// back with the monitor.
    #[test]
    fn a_rescue_reaches_a_crtc_that_is_driving_nothing() {
        assert!(
            walk_includes(RESCUE_WALK, false),
            "a rescue that skips an idle CRTC misses the ramp on an unplugged monitor"
        );
        assert!(walk_includes(RESCUE_WALK, true));
        assert!(walk_includes(Walk::Driving, true));
        assert!(
            !walk_includes(Walk::Driving, false),
            "an idle CRTC cannot be dimmed or addressed"
        );
    }

    /// Only a genuinely absent extension is "nothing to rescue". Reds the defect
    /// this PR shipped and reverted within one commit: routing every send failure
    /// there meant a dead connection printed "nothing to restore" and exited 0 for
    /// a session that may well be dimmed.
    #[test]
    fn only_a_missing_extension_means_a_missing_channel() {
        assert!(send_failure_is_absent_channel(SendFailure::ExtensionAbsent));
        assert!(
            !send_failure_is_absent_channel(SendFailure::Transport),
            "a transport failure may be hiding a live ramp and must be reported"
        );
    }

    /// A fault that might be permanent is treated as permanent, because the two
    /// failure directions are not symmetric: a needless reconnect costs one
    /// handshake, a kept-but-dead connection costs the channel for the life of the
    /// process.
    #[test]
    fn a_connection_survives_only_a_per_request_fault() {
        assert!(connection_survives(ConnectionFault::PerRequest));
        assert!(!connection_survives(ConnectionFault::Io));
        assert!(
            !connection_survives(ConnectionFault::ExtensionLookupPoisoned),
            "x11rb caches a failed QueryExtension for the life of the connection, \
             and every request here resolves RandR first"
        );
    }

    #[test]
    fn only_randr_1_3_and_later_can_list_crtcs() {
        assert!(!randr_lists_crtcs(1, 2), "GetScreenResourcesCurrent is 1.3");
        assert!(randr_lists_crtcs(1, 3));
        assert!(randr_lists_crtcs(1, 6), "a newer server still qualifies");
        assert!(randr_lists_crtcs(2, 0), "so does a newer major");
        assert!(!randr_lists_crtcs(0, 9));
    }

    /// The X11 walk and the X11 writer must classify every size identically, or
    /// the rescue reports a CRTC unreachable that the writer would have written,
    /// or worse the other way round. They share [`writable_ramp_size`] for exactly
    /// that reason.
    ///
    /// `ramp` is deliberately **not** the third party here: it is stricter than
    /// nothing and looser than X11, because the ceiling is the transport's. Where
    /// the two differ is asserted below rather than left implicit, so collapsing
    /// them back together reds.
    #[test]
    fn the_walk_and_the_writer_agree_on_which_sizes_are_writable() {
        let sizes = [
            0u16,
            1,
            MIN_RAMP_SIZE,
            2,
            256,
            4096,
            MAX_RAMP_SIZE,
            MAX_RAMP_SIZE.saturating_add(1),
            u16::MAX,
        ];
        for size in sizes {
            // The walk's predicate is the writer's, by construction and by name.
            assert_eq!(
                writable_ramp_size(size),
                (MIN_RAMP_SIZE..=MAX_RAMP_SIZE).contains(&size),
                "size {size} is classified differently by the walk and the writer"
            );
            // And the builder answers the narrower question: is this a table at
            // all, transport aside.
            assert_eq!(
                ramp(0.5, size).is_some(),
                size >= MIN_RAMP_SIZE,
                "size {size}: the builder must bound only the floor"
            );
        }

        // The gap is the whole point of the split, so name it.
        let beyond_x11 = MAX_RAMP_SIZE.saturating_add(1);
        assert!(ramp(0.5, beyond_x11).is_some());
        assert!(!writable_ramp_size(beyond_x11));
    }

    /// A CRTC with no gamma hardware holds nothing; one whose table is too large
    /// to write holds something this crate cannot reach. A rescue may skip the
    /// first silently and must not skip the second silently.
    #[test]
    fn an_unwritable_size_is_only_nothing_when_there_is_no_table() {
        assert!(ramp_size_is_absent(0), "no gamma hardware");
        assert!(ramp_size_is_absent(1), "no input axis, so no ramp to hold");
        assert!(
            !ramp_size_is_absent(MAX_RAMP_SIZE.saturating_add(1)),
            "an oversized table is a ramp this crate cannot reach, not an absent one"
        );
        assert!(!ramp_size_is_absent(u16::MAX));
    }

    /// A session with no channel has nothing to rescue; a channel that could not
    /// be reached is a failure the user must see. Reds either of the two mistakes
    /// this branch has already made, in either direction.
    #[test]
    fn a_rescue_tells_nothing_to_do_apart_from_could_not_run() {
        let absent = rescue_refusal_report(ChannelRefusal::Absent("this session is Wayland"));
        assert!(absent.restored.is_empty());
        assert!(
            absent.is_clean(),
            "a session with no gamma channel has nothing to fail at"
        );

        let unreachable =
            rescue_refusal_report(ChannelRefusal::Unreachable("X11 connect failed: no auth"));
        assert!(!unreachable.is_clean(), "the user must see this one fail");
        assert_eq!(
            unreachable.failed.first().map(|(row, _)| row.as_str()),
            Some(CHANNEL_ROW),
            "the failure is the channel itself, not a CRTC"
        );
        assert!(
            unreachable
                .failed
                .first()
                .is_some_and(|(_, reason)| reason.contains("no auth")),
            "the reason has to reach the user: it is the whole diagnostic"
        );
    }

    #[test]
    fn hdr_is_known_absent_on_x11_and_unknown_elsewhere() {
        assert_eq!(hdr_active_for(Transport::X11), Some(false));
        assert_eq!(hdr_active_for(Transport::Wayland), None);
        assert_eq!(hdr_active_for(Transport::None), None);
    }

    /// The mapping the caller actually consumes: only X11 ends up allowed to
    /// drive a ramp, and the two sessions with no `XRandR` channel are also the two
    /// that report `Unknown`. Reds a Wayland arm that answered `Some(false)` —
    /// which would advertise a gamma channel that silently writes to Xwayland.
    ///
    /// The **`XRandR`** channel, which is what the name means and is worth spelling
    /// out now that it is no longer the only one. Since `#131` a Wayland session
    /// has `wlr-gamma-control`, so "the one with a channel" would be two of them if
    /// read literally — and this test would still be right, because that channel is
    /// gated by the same `Unknown` verdict for a reason `docs/debt.md` carries.
    #[test]
    fn the_only_session_allowed_to_drive_a_ramp_is_the_one_with_a_channel() {
        for transport in [Transport::X11, Transport::Wayland, Transport::None] {
            let allowed = crate::gamma_support_from_hdr(hdr_active_for(transport)).allows_gamma();
            assert_eq!(
                allowed,
                xrandr_refusal(transport).is_none(),
                "{transport:?} disagrees with itself about gamma"
            );
        }
    }
}

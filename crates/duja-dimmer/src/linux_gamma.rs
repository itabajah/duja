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
//!   `DISPLAY` almost always points at Xwayland, every request below would
//!   succeed, and the ramp would land on Xwayland's virtual CRTCs where nothing
//!   ever shows it.
//! - [`crtc_from_token`] — recovering the CRTC id from the display-surface token,
//!   where the failure is dimming *a different monitor*.
//! - [`ramp`] — the ramp itself, where a wrong curve is a wrongly-lit screen and
//!   a wrong **length** is a request the server rejects.
//! - [`hdr_active_for`] — whether a ramp means anything on this session at all.
//!
//! # An `XRandR` ramp outlives the process that set it
//!
//! Worth stating here rather than only in the backend, because it is the one
//! property that separates Linux from macOS and puts it with Windows. The X
//! server holds each CRTC's gamma table as server state and does **not** reset it
//! when the client that wrote it disconnects — which is exactly why `xgamma` and
//! `redshift -O` work as one-shot commands that exit immediately. So a Duja that
//! crashes mid-dim leaves the screen dark with nothing left running to undo it.
//!
//! `duja --restore` is the manual rescue and exists today. The automatic one — a
//! crash marker and an RAII guard, the machinery Windows carries — is owed to the
//! wave that gives Linux a tray to engage gamma from; nothing engages a ramp on
//! Linux until then. `docs/debt.md` carries it.

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
/// Derived, not chosen. The core protocol caps a request at the server's
/// `maximum_request_length`, which is `65535` four-byte units — `262_140` bytes —
/// on every X server in use. `SetCrtcGamma` spends 12 of those on its header
/// (opcode, length, CRTC, size, pad) and `6 * size` on the three `CARD16`
/// channels, so `6 * size <= 262_128`, i.e. `size <= 43_688`.
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
/// # The Wayland arm is the whole point of this function
///
/// "No display server" is the boring answer. The answer that matters is
/// **Wayland**, because on a Wayland session `DISPLAY` is almost always set — to
/// Xwayland — and every step of the gamma path below would then *succeed*:
/// `x11rb::connect` connects, `RandR` is present, `GetCrtcGammaSize` answers, and
/// `SetCrtcGamma` writes a ramp into Xwayland's own virtual CRTC. Xwayland
/// renders into a `wl_surface`; it does not own the outputs and its gamma tables
/// are not on the path to any monitor. The user would get a silent no-op with an
/// `Ok(())` behind it, and Duja would record a ramp as live and later "restore"
/// it — the failure this crate rates worst, an OS call that reports success while
/// nothing on screen changes.
///
/// So the refusal is by **transport**, decided by [`crate::linux_caps::transport`]
/// from the environment, and not by whether an X connection can be opened. A
/// Wayland session's gamma channel is `wlr-gamma-control` (or the compositor's own
/// night-light), which is a different backend and a later wave.
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
/// token (`linux::outputs`), so the two halves of that contract are one file
/// apart and one test away — rather than the parse being written from scratch in
/// another crate, months later, with nothing pinning it. Its shape is the macOS
/// sink's `display_id_from_token`, which is the fourth bullet above: the two
/// tokens are both decimal integers, so only the platform gate keeps them apart.
#[must_use]
pub fn crtc_from_token(token: &str) -> Option<u32> {
    match token.parse::<u32>() {
        // `x11rb::NONE`: never a CRTC, and what "no CRTC" is spelled as.
        Ok(0) | Err(_) => None,
        Ok(crtc) => Some(crtc),
    }
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
/// Returns `None` for a size outside [`MIN_RAMP_SIZE`]`..=`[`MAX_RAMP_SIZE`]
/// rather than guessing — see both constants for why each bound exists. Total and
/// never-panicking otherwise.
#[must_use]
pub fn ramp(factor: f32, size: u16) -> Option<Vec<u16>> {
    if !(MIN_RAMP_SIZE..=MAX_RAMP_SIZE).contains(&size) {
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
///   is the honest answer and it costs nothing, because a Wayland session has no
///   `XRandR` gamma channel to use it with in the first place ([`xrandr_refusal`]).
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
    fn a_crtc_token_round_trips() {
        assert_eq!(crtc_from_token("63"), Some(63));
        assert_eq!(crtc_from_token("1"), Some(1));
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
    /// `i * 257`, because `65535 / 255` is 257 with no remainder. Pins the two
    /// platforms' ramp arithmetic against each other rather than against itself.
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
        assert_eq!(
            ramp(0.5, MAX_RAMP_SIZE.saturating_add(1)),
            None,
            "a table larger than one request can carry"
        );
        assert_eq!(ramp(0.5, u16::MAX), None);
        assert!(ramp(0.5, MAX_RAMP_SIZE).is_some(), "the bound is inclusive");
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

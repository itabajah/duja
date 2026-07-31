//! Software dimming: the fallback layer of Duja's brightness continuum.
//!
//! Primary mechanism (ADR-0003): a per-monitor, borderless, always-on-top,
//! **click-through** overlay window with variable-alpha black fill — the only
//! technique that reaches true black on every OS, survives HDR, and does not
//! disturb the gamma ramp. An opt-in gamma-ramp backend exists only where
//! verified safe (never under HDR) and is engaged through a separate, explicit
//! API so a routine [`Dimmer::apply`] never touches gamma.
//!
//! # Architecture
//!
//! - [`plan`] is the pure diffing kernel: it turns a desired [`DimCommand`] set
//!   into the minimal [`plan::OverlayOp`] list, given the current overlays. It
//!   is OS-free and exhaustively unit-tested on every target.
//! - On Windows, `WindowsDimmer` owns a dedicated thread that holds every
//!   overlay window and its message loop (spawn → HWND-ready handshake,
//!   shutdown → destroy windows → join; `Drop` shuts down). `apply` diffs with
//!   [`plan`] and executes the ops on that thread.
//! - On macOS, `MacDimmer` cannot own a thread: `AppKit` windows may only be
//!   touched on the main thread, which in `duja-app` runs Slint's (winit's)
//!   `NSApplication` loop. `apply` diffs with [`plan`] on the calling thread,
//!   then marshals the resulting overlay ops onto the **main dispatch queue**
//!   (`dispatch_async`); the windows live in a main-thread store. See the `mac`
//!   module docs for the observable-contract difference (non-blocking vs the
//!   Windows blocking `apply`) and the running-run-loop requirement.
//! - On other Unix targets, `StubDimmer` records-and-succeeds so higher layers
//!   can run their logic unchanged (documented no-op; the Linux backend lands
//!   in P7).
//!
//! # Security invariant
//!
//! Overlays must **never** intercept input. On Windows every overlay carries
//! `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE` and answers `WM_NCHITTEST` with
//! `HTTRANSPARENT`; on macOS every overlay sets `ignoresMouseEvents = true`.
//! Fullscreen-exclusive apps and the OS secure/login screens are documented
//! known-limits on both platforms (an overlay cannot cover them).
//!
//! # Crash safety
//!
//! Overlay windows die with the process. A Windows gamma ramp **persists** after
//! death, so `ScreenStateGuard` restores identity gamma on drop (including panic
//! unwind) and a marker file lets a fresh start detect a dirty exit and call
//! `restore_all`. macOS is different: the window server restores each process's
//! gamma automatically when the process exits, so the macOS backend needs **no**
//! marker machinery and its `restore_all` is a single
//! `CGDisplayRestoreColorSyncSettings` call.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod plan;

// Pure geometry for the macOS overlay backend. OS-free, so it compiles and is
// tested on every target (like `plan`); only the `mac` backend calls it.
mod mac_geom;

// Re-export the cross-platform vocabulary so callers can depend on this crate
// alone for the dimming surface.
pub use duja_core::dimmer::{
    DimCommand, Dimmer, DimmerError, DisplayBounds, GAMMA_FLOOR, clamp_alpha, clamp_gamma,
};

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{
    GammaDisplay, GammaRamp, GammaSupport, RestoreReport, ScreenStateGuard, WindowsDimmer,
    clear_marker, display_supports_gamma, enumerate_gamma_displays, gamma_support_from_hdr,
    is_hdr_active, mark_dirty, marker_present, restore_all, restore_identity, set_gamma,
};

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
pub use mac::{
    GammaDisplay, GammaSupport, MacDimmer, RestoreReport, display_supports_gamma,
    enumerate_gamma_displays, gamma_support_from_hdr, is_hdr_active, restore_all, restore_identity,
    set_gamma,
};

#[cfg(not(any(windows, target_os = "macos")))]
mod stub;

#[cfg(not(any(windows, target_os = "macos")))]
pub use stub::StubDimmer;

/// The concrete [`Dimmer`] for the current platform: `WindowsDimmer` on Windows,
/// `MacDimmer` on macOS, `StubDimmer` elsewhere. Callers that want the native
/// backend without a `cfg` write `PlatformDimmer`.
#[cfg(windows)]
pub type PlatformDimmer = WindowsDimmer;

/// The concrete [`Dimmer`] for the current platform (macOS overlay backend).
#[cfg(target_os = "macos")]
pub type PlatformDimmer = MacDimmer;

/// The concrete [`Dimmer`] for the current platform (non-Windows/macOS stub).
#[cfg(not(any(windows, target_os = "macos")))]
pub type PlatformDimmer = StubDimmer;

/// The lowest gamma factor **this platform's OS** will actually accept.
///
/// A *platform* limit, distinct from [`GAMMA_FLOOR`] — Duja's own cross-platform
/// safety floor on how dark a ramp it is willing to ask for. A caller that wants a
/// factor below this will get a refusal rather than a darker screen, so it must
/// ask **before** attempting the write and realise that part of the range some
/// other way (ADR-0003's overlay is the answer, and `duja-app`'s `dimming::plan`
/// does exactly that).
///
/// On Windows this is `MIN_ACCEPTED_GAMMA` (`0.5`). Microsoft's
/// [Using gamma correction](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/using-gamma-correction)
/// states the rule: *"any entry in the ramp must be within 32768 of the identity
/// value"*, so no application can blank the display. See that constant for the
/// derivation and the hardware measurement that confirms it. `GAMMA_FLOOR` is
/// therefore **unreachable** on Windows.
///
/// Staying at or above this bound is also the only *reliable* protection, because a
/// refusal is not always reported: the same API is documented as able to *"fail
/// silently (that is, it returns TRUE, but it doesn't set your ramp)"*. See
/// `docs/debt.md`.
#[cfg(windows)]
#[must_use]
pub fn min_gamma_factor() -> f32 {
    win::MIN_ACCEPTED_GAMMA
}

/// The lowest gamma factor **this platform's OS** will actually accept.
///
/// A *platform* limit, distinct from [`GAMMA_FLOOR`] — Duja's own cross-platform
/// safety floor on how dark a ramp it is willing to ask for. Off Windows there is
/// no additional OS restriction to report, so this is `GAMMA_FLOOR` itself and no
/// caller ever needs to substitute an overlay for a refused ramp:
///
/// - **macOS**: `CGSetDisplayTransferByFormula` takes an arbitrary
///   `(min, max, gamma)` triple — it will happily accept `max = 0` and black the
///   display out — so it imposes no anti-lockout clamp of its own. (The reason
///   that is safe on macOS and not on Windows is crash behaviour, not validation:
///   the window server restores a process's transfer tables when it exits, so a
///   crashed ramp self-heals.)
/// - **Other targets**: there is no gamma backend at all, so the value is inert.
#[cfg(not(windows))]
#[must_use]
pub fn min_gamma_factor() -> f32 {
    GAMMA_FLOOR
}

/// Whether this platform's gamma write can report success without applying the
/// ramp, in a way the caller **cannot pre-empt**.
///
/// Not "can a gamma write ever silently fail here" — both supported platforms
/// document or evidence some such mode. The question is whether there is a rule the
/// caller can satisfy to stay out of it, because that is what decides whether Duja
/// can engineer the hazard away or has to disclose it.
///
/// On **Windows** this is `false`. The failure is documented with a trigger:
/// [`SetDeviceGammaRamp`](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-setdevicegammaramp)
/// *"implements heuristics to check whether a provided ramp will result in an
/// unreadable screen. **If a ramp violates those heuristics**, then the function
/// fails silently"* — and the heuristic is quantified elsewhere as "any entry in
/// the ramp must be within 32768 of the identity value". `MIN_ACCEPTED_GAMMA` is
/// derived from exactly that bound and pinned *at* it by `win/gamma.rs`'s tests,
/// and `duja-app`'s planner substitutes an overlay below it, so every ramp Duja
/// sends complies by construction. That is what its own doc means by "staying at or
/// above it is the only reliable protection". A residual remains — a driver or GPU
/// with a tighter, undocumented rule (`docs/debt.md`) — but it is a hardware
/// unknown, not a mode the platform has.
///
/// On **macOS** this is `true`, and the difference is mechanism rather than
/// likelihood. `CGSetDisplayTransferByFormula` is reported (DTS-acknowledged) to
/// return `kCGErrorSuccess` while leaving the display's curve unchanged, on
/// *valid* triples, when "Automatically adjust brightness" is on — the default on
/// Apple Silicon laptops. There is no rule to comply with, and readback cannot
/// detect it either: `CGGetDisplayTransferByTable` returns the values just written
/// while the screen is unchanged. See `docs/debt.md`.
#[cfg(windows)]
#[must_use]
pub fn gamma_is_advisory() -> bool {
    false
}

/// Whether this platform's gamma write can report success without applying the
/// ramp, in a way the caller **cannot pre-empt**. See the Windows arm for the full
/// contract; the short version is that this asks whether the hazard can be
/// engineered away, not whether it exists.
///
/// - **macOS**: `true`. `CGSetDisplayTransferByFormula` is reported
///   (DTS-acknowledged) to return `kCGErrorSuccess` while leaving the curve
///   unchanged, on *valid* triples, when "Automatically adjust brightness" is on —
///   the default on Apple Silicon laptops. No rule exists to comply with, and
///   `CGGetDisplayTransferByTable` returns the written values while the screen is
///   unchanged, so verification by readback does not detect it either.
/// - **Other targets**: `false` — there is no gamma backend at all, so nothing can
///   silently fail.
#[cfg(not(windows))]
#[must_use]
pub fn gamma_is_advisory() -> bool {
    cfg!(target_os = "macos")
}

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_against_core() {
        assert_eq!(version(), duja_core::version());
    }

    #[test]
    fn min_gamma_factor_is_never_below_the_safety_floor() {
        // A platform whose OS accepted *less* than `GAMMA_FLOOR` would still not
        // get it: `clamp_gamma` bounds every factor at the floor first, so the
        // effective minimum can only ever be at or above it.
        assert!(min_gamma_factor() >= GAMMA_FLOOR);
    }

    #[cfg(windows)]
    #[test]
    fn windows_cannot_reach_the_cross_platform_gamma_floor() {
        // The whole reason the app needs an overlay fallback: on Windows the OS
        // refuses the bottom of the gamma range outright, so `GAMMA_FLOOR` is not
        // a level the ramp can deliver.
        assert!(
            min_gamma_factor() > GAMMA_FLOOR,
            "Windows caps the ramp above GAMMA_FLOOR; the planner must substitute an overlay"
        );
    }

    #[test]
    fn a_platform_with_a_pre_emptable_failure_mode_is_not_advisory() {
        // The two verdicts are not "does gamma ever silently fail" — Windows
        // documents such a mode too. They are "is there a rule the caller can
        // satisfy", because only that decides whether Duja engineers the hazard
        // away or discloses it. Windows has one and `min_gamma_factor()` *is* it;
        // macOS has none.
        //
        // The pairing is asserted rather than the two constants separately, so
        // flipping either verdict without the other reds: a Windows `true` would
        // put a hazard caption in front of users on a path this crate's own
        // `MIN_ACCEPTED_GAMMA` tests prove compliant, and a macOS `false` would
        // ship the reported silent no-op undisclosed.
        #[cfg(windows)]
        {
            assert!(!gamma_is_advisory());
            assert!(
                min_gamma_factor() > GAMMA_FLOOR,
                "Windows is non-advisory *because* it has a satisfiable bound; if that \
                 bound ever stopped existing, the verdict would have to change with it"
            );
        }
        #[cfg(target_os = "macos")]
        {
            assert!(
                gamma_is_advisory(),
                "macOS accepts the whole range and can still not apply it — the settings \
                 window has to say so"
            );
            assert!(
                (min_gamma_factor() - GAMMA_FLOOR).abs() < f32::EPSILON,
                "no OS bound to comply with is exactly why the verdict is advisory"
            );
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(!gamma_is_advisory(), "no gamma backend, nothing to fail");
    }
}

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
}

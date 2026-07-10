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
//! - On other targets, `StubDimmer` records-and-succeeds so higher layers can
//!   run their logic unchanged (documented no-op; real backends land in P6/P7).
//!
//! # Security invariant
//!
//! Overlays must **never** intercept input. Every overlay carries
//! `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE` and answers `WM_NCHITTEST` with
//! `HTTRANSPARENT`; fullscreen-exclusive apps and the secure desktop are
//! documented known-limits (an overlay cannot cover them).
//!
//! # Crash safety
//!
//! Overlay windows die with the process, but a Windows gamma ramp **persists**
//! after death. `ScreenStateGuard` is the app's RAII owner that restores
//! identity gamma on drop (including panic unwind); a marker file written before
//! the first gamma engage lets a fresh start detect a dirty exit and call
//! `restore_all` to recover.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod plan;

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

#[cfg(not(windows))]
mod stub;

#[cfg(not(windows))]
pub use stub::StubDimmer;

/// The concrete [`Dimmer`] for the current platform: `WindowsDimmer` on
/// Windows, `StubDimmer` elsewhere. Callers that want the native backend
/// without a `cfg` write `PlatformDimmer`.
#[cfg(windows)]
pub type PlatformDimmer = WindowsDimmer;

/// The concrete [`Dimmer`] for the current platform (non-Windows stub).
#[cfg(not(windows))]
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

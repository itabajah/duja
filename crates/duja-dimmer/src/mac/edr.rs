//! The HDR/EDR guard: force overlay dimming on displays that can do HDR.
//!
//! A gamma ramp is meaningless (and ignored) on an HDR/EDR display, so Duja must
//! never offer the gamma path there. The macOS analogue of the Windows DXGI
//! colour-space probe is `NSScreen`'s extended-dynamic-range headroom: a display
//! reports `maximumPotentialExtendedDynamicRangeColorComponentValue > 1.0` iff it
//! can present EDR/HDR content. We treat *capability* (not just currently-active
//! HDR) as unsafe-for-gamma — the conservative choice.
//!
//! `NSScreen` is a main-thread API. Probed from a background thread we cannot
//! read it safely, so [`is_hdr_active`] returns `None` (⇒ [`GammaSupport::Unknown`]
//! ⇒ gamma withheld) unless called on the main thread. This matches the Windows
//! "uncertain probe ⇒ default to overlay" safety posture while keeping the exact
//! same public surface.

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;

// The verdict this probe feeds is the same on every platform — only the probe
// itself is per-platform — so it lives in one unconditional module and is tested
// on all three CI lanes rather than once per backend. It was byte-identical to
// the Windows copy before Linux would have made it three. See
// `crate::gamma_support`.
use crate::gamma_support::{GammaSupport, gamma_support_from_hdr};

/// Whether any attached display can present HDR/EDR content.
///
/// Returns `Some(true)` if at least one `NSScreen` reports EDR headroom above
/// `1.0`, `Some(false)` if every screen is SDR, and `None` if the probe could
/// not run — which, on macOS, includes being called off the main thread (see
/// the module docs). Read-only; never changes display state.
#[must_use]
pub fn is_hdr_active() -> Option<bool> {
    // `NSScreen` must be read on the main thread; without that proof we cannot
    // determine the state and report `Unknown` (the safe default).
    let mtm = MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    let count = screens.count();
    if count == 0 {
        return None;
    }
    for i in 0..count {
        let screen = screens.objectAtIndex(i);
        if screen.maximumPotentialExtendedDynamicRangeColorComponentValue() > 1.0 {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether gamma dimming is safe on the current display configuration.
///
/// A convenience over [`is_hdr_active`]: HDR-capable ⇒ [`GammaSupport::UnsupportedHdr`],
/// SDR ⇒ [`GammaSupport::Supported`], an indeterminate probe ⇒
/// [`GammaSupport::Unknown`].
#[must_use]
pub fn display_supports_gamma() -> GammaSupport {
    gamma_support_from_hdr(is_hdr_active())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `gamma_support_from_hdr` mapping is pinned in `crate::gamma_support`,
    // where it now lives; what is macOS-specific — and all this module still owns
    // — is the `NSScreen` EDR probe and its off-main-thread refusal.

    #[test]
    fn probe_runs_without_panicking() {
        // Read-only and safe anywhere; off the main thread it returns `None`.
        let _ = is_hdr_active();
        let _ = display_supports_gamma();
    }
}

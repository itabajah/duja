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

/// Whether a display can safely use the gamma dimming path. Mirrors the Windows
/// enum exactly so cross-platform callers share one type shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GammaSupport {
    /// Gamma is safe here (SDR display, probe succeeded).
    Supported,
    /// The display can present HDR/EDR; gamma must not be used (force overlay).
    UnsupportedHdr,
    /// The probe could not determine HDR state; the caller should default to
    /// overlay dimming (the safe choice).
    Unknown,
}

impl GammaSupport {
    /// Whether the gamma path may be used. Only [`Supported`](Self::Supported)
    /// returns `true`; [`Unknown`](Self::Unknown) is treated as "no".
    #[must_use]
    pub fn allows_gamma(self) -> bool {
        matches!(self, GammaSupport::Supported)
    }
}

/// Map the raw HDR probe (`Some(true)` = HDR-capable, `Some(false)` = SDR,
/// `None` = unknown) to a [`GammaSupport`]. Pure, so it is unit-tested directly.
/// Identical to the Windows mapper for cross-platform symmetry.
#[must_use]
pub fn gamma_support_from_hdr(hdr_active: Option<bool>) -> GammaSupport {
    match hdr_active {
        Some(true) => GammaSupport::UnsupportedHdr,
        Some(false) => GammaSupport::Supported,
        None => GammaSupport::Unknown,
    }
}

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

    #[test]
    fn hdr_maps_to_unsupported() {
        assert_eq!(
            gamma_support_from_hdr(Some(true)),
            GammaSupport::UnsupportedHdr
        );
        assert!(!gamma_support_from_hdr(Some(true)).allows_gamma());
    }

    #[test]
    fn sdr_maps_to_supported() {
        assert_eq!(gamma_support_from_hdr(Some(false)), GammaSupport::Supported);
        assert!(gamma_support_from_hdr(Some(false)).allows_gamma());
    }

    #[test]
    fn unknown_defaults_to_no_gamma() {
        assert_eq!(gamma_support_from_hdr(None), GammaSupport::Unknown);
        assert!(!gamma_support_from_hdr(None).allows_gamma());
    }

    #[test]
    fn probe_runs_without_panicking() {
        // Read-only and safe anywhere; off the main thread it returns `None`.
        let _ = is_hdr_active();
        let _ = display_supports_gamma();
    }
}

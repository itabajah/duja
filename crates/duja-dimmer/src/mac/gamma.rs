//! The opt-in gamma path (Core Graphics), and why macOS needs no crash marker.
//!
//! Like Windows, gamma is **not** on the default dimming path: an overlay
//! reaches true black without touching the transfer tables, and gamma is
//! meaningless under HDR. Unlike Windows, a macOS gamma table does **not**
//! outlive the process: the window server tracks each connection's transfer
//! tables and restores them automatically when that process exits. A crash
//! therefore self-heals, so this module carries **none** of the Windows
//! crash-marker / `ScreenStateGuard` machinery — [`restore_all`] is a single
//! `CGDisplayRestoreColorSyncSettings` call for an explicit, in-process restore
//! (e.g. `duja-app --restore`).
//!
//! Gamma calls do not require the main thread (Quartz display services are not
//! `NSWindow`), so they run inline on the caller's thread — off the main-queue
//! marshalling path, exactly as the Windows backend keeps gamma off the overlay
//! `apply` path.

use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayRestoreColorSyncSettings, CGError, CGGetActiveDisplayList,
    CGMainDisplayID, CGSetDisplayTransferByFormula,
};

use duja_core::dimmer::{DimmerError, clamp_gamma};

/// The largest number of displays [`enumerate_gamma_displays`] will report.
const MAX_DISPLAYS: u32 = 16;

/// A display whose gamma transfer function can be driven, identified by its
/// `CGDirectDisplayID`.
///
/// Holds only the id (a `u32`), so the value is cheap, [`Send`], and safe to
/// store — the FFI takes the id directly, with no handle to open or close.
#[derive(Debug, Clone)]
pub struct GammaDisplay {
    id: CGDirectDisplayID,
    name: String,
}

impl GammaDisplay {
    /// Wrap a raw `CGDirectDisplayID`. Mainly for tests; production code obtains
    /// displays via [`enumerate_gamma_displays`].
    #[must_use]
    pub fn from_display_id(id: CGDirectDisplayID) -> Self {
        GammaDisplay {
            id,
            name: format!("CGDisplay-{id}"),
        }
    }

    /// A friendly name (e.g. `CGDisplay-1`) for reporting.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw Core Graphics display id.
    #[must_use]
    pub fn id(&self) -> CGDirectDisplayID {
        self.id
    }
}

/// The Core-Graphics transfer-formula coefficients `(min, max, gamma)` that
/// scale output brightness linearly by `factor`.
///
/// `factor` is clamped into [`GAMMA_FLOOR`](duja_core::dimmer::GAMMA_FLOOR)`..=1.0`
/// first (so a driven ramp is never blacker than the safety floor). The formula
/// `out = min + (max - min) * in^gamma` with `min = 0`, `max = factor`,
/// `gamma = 1` yields `out = factor * in` — a neutral linear dim, identical in
/// intent to the Windows `gamma_ramp`. Pure, so it is unit-tested directly.
#[must_use]
pub fn transfer_formula(factor: f32) -> (f32, f32, f32) {
    (0.0, clamp_gamma(factor), 1.0)
}

/// Drive `display`'s gamma to scale output brightness by `factor`.
///
/// # Errors
/// [`DimmerError::Os`] if Core Graphics rejects the transfer function (some
/// displays/configurations refuse gamma changes — the caller should fall back
/// to overlay dimming).
pub fn set_gamma(display: &GammaDisplay, factor: f32) -> Result<(), DimmerError> {
    let (min, max, gamma) = transfer_formula(factor);
    write_formula(display.id, min, max, gamma)
}

/// Restore `display` to the identity transfer function (no dimming).
///
/// # Errors
/// [`DimmerError::Os`] if Core Graphics rejects the transfer function.
pub fn restore_identity(display: &GammaDisplay) -> Result<(), DimmerError> {
    write_formula(display.id, 0.0, 1.0, 1.0)
}

/// Apply a transfer formula to one display via Core Graphics.
fn write_formula(id: CGDirectDisplayID, min: f32, max: f32, gamma: f32) -> Result<(), DimmerError> {
    // Pure by-value call (objc2 marks it safe): the nine `CGGammaValue` (f32)
    // coefficients are passed by value, no pointers.
    let err = CGSetDisplayTransferByFormula(id, min, max, gamma, min, max, gamma, min, max, gamma);
    if err == CGError::Success {
        Ok(())
    } else {
        Err(DimmerError::Os(format!(
            "CGSetDisplayTransferByFormula failed for CGDisplay-{id}: {err:?}"
        )))
    }
}

/// Enumerate the displays currently active on the desktop.
///
/// Returns an empty vector (never an error) if Core Graphics cannot enumerate —
/// the graceful-degradation contract.
#[must_use]
pub fn enumerate_gamma_displays() -> Vec<GammaDisplay> {
    let mut ids = [0u32; MAX_DISPLAYS as usize];
    let mut count: u32 = 0;
    // SAFETY: `ids` is a `MAX_DISPLAYS`-long buffer matching the cap argument;
    // `count` receives the number written. A non-success return leaves us with
    // `count == 0`, which yields an empty vector.
    let err = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if err != CGError::Success {
        return Vec::new();
    }
    let n = (count as usize).min(ids.len());
    ids.get(..n)
        .unwrap_or(&[])
        .iter()
        .map(|&id| GammaDisplay::from_display_id(id))
        .collect()
}

/// Restore the user's `ColorSync` gamma on every display now (an explicit,
/// in-process restore; the window server also does this automatically on exit).
///
/// Never fails as a whole: `CGDisplayRestoreColorSyncSettings` restores all
/// displays at once, so the report lists every enumerated display as restored.
#[must_use]
pub fn restore_all() -> RestoreReport {
    // Pure call (objc2 marks it safe): resets every display to its ColorSync
    // profile gamma, no arguments.
    CGDisplayRestoreColorSyncSettings();
    let restored = {
        let displays = enumerate_gamma_displays();
        if displays.is_empty() {
            // Enumeration came back empty (headless/CI): still report the main
            // display id we know exists (pure scalar query, objc2-safe).
            let main = CGMainDisplayID();
            vec![GammaDisplay::from_display_id(main).name().to_owned()]
        } else {
            displays.iter().map(|d| d.name().to_owned()).collect()
        }
    };
    RestoreReport {
        restored,
        failed: Vec::new(),
    }
}

/// What a [`restore_all`] pass did: the displays it reset and any it could not.
///
/// Mirrors the Windows report shape. On macOS the restore is a single global
/// call, so `failed` is always empty; the field exists for surface parity.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// Names of displays whose gamma was restored.
    pub restored: Vec<String>,
    /// `(name, error)` for each display that could not be restored (always empty
    /// on macOS).
    pub failed: Vec<(String, String)>,
}

impl RestoreReport {
    /// Whether every attempted display was restored (no failures).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::dimmer::GAMMA_FLOOR;

    #[test]
    fn identity_formula_is_unit_scale() {
        assert_eq!(transfer_formula(1.0), (0.0, 1.0, 1.0));
    }

    #[test]
    fn factor_is_clamped_to_floor() {
        let (_, max, _) = transfer_formula(0.0);
        assert!((max - GAMMA_FLOOR).abs() < f32::EPSILON);
        let (_, nan_max, _) = transfer_formula(f32::NAN);
        assert!((nan_max - 1.0).abs() < f32::EPSILON, "NaN maps to identity");
    }

    #[test]
    fn mid_factor_passes_through() {
        let (min, max, gamma) = transfer_formula(0.5);
        assert!((min - 0.0).abs() < f32::EPSILON);
        assert!((max - 0.5).abs() < f32::EPSILON);
        assert!((gamma - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn gamma_display_name_includes_id() {
        let d = GammaDisplay::from_display_id(7);
        assert_eq!(d.id(), 7);
        assert_eq!(d.name(), "CGDisplay-7");
    }

    #[test]
    fn restore_report_cleanliness() {
        let mut r = RestoreReport::default();
        assert!(r.is_clean());
        r.failed.push(("X".to_owned(), "boom".to_owned()));
        assert!(!r.is_clean());
    }
}

//! The opt-in gamma-ramp path and its crash-safety machinery.
//!
//! Gamma is **not** on the default dimming path: an overlay reaches true black
//! without touching the GPU's gamma ramp, and — unlike a window — a gamma ramp
//! **persists after the process dies**. A crash mid-dim would otherwise leave
//! the user staring at a too-dark desktop with no obvious cure. This module is
//! therefore engaged only explicitly, through [`ScreenStateGuard`], which:
//!
//! - writes a **marker file** (atomic create) before the first gamma engage, so
//!   a fresh start can detect a dirty exit ([`crate::marker_present`]) and call
//!   [`restore_all`] to recover;
//! - restores identity gamma on every touched display on drop, **including a
//!   panic unwind**;
//! - clears the marker on clean teardown.
//!
//! The ramp maths ([`gamma_ramp`]) is pure and unit-tested on every target; the
//! Win32 calls are Windows-only and covered by the hardware-gated live tests.

use std::path::PathBuf;

use duja_core::dimmer::{DimmerError, clamp_gamma};
// The marker file itself is three `std::fs` calls with nothing Windows about
// them, so it lives in `crate::marker` and Linux uses the same three. This module
// is still the only thing that writes one *automatically*, through the guard.
use crate::marker::{clear_marker, mark_dirty};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW, DeleteDC, EnumDisplayDevicesW,
    HDC,
};
use windows::Win32::UI::ColorSystem::SetDeviceGammaRamp;
use windows::core::PCWSTR;

/// A 16-bit gamma ramp: three identical channels of 256 entries, in the layout
/// `SetDeviceGammaRamp` expects.
pub type GammaRamp = [[u16; 256]; 3];

/// The largest deviation from the identity ramp Windows will accept in a
/// `SetDeviceGammaRamp` write.
///
/// # The rule is documented, not inferred
///
/// Microsoft states it verbatim in
/// [Using gamma correction](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/using-gamma-correction):
/// *"any entry in the ramp must be within 32768 of the identity value. This
/// restriction means that no app can turn the display completely black or to some
/// other unreadable color."* It is enforced by GDI, not by an individual driver,
/// which is what makes the constant a property of **Windows** rather than of the
/// one display it was first observed on.
///
/// # And it is confirmed by measurement
///
/// The hardware sweep `report_which_gamma_factors_the_driver_accepts`
/// (`tests/windows_live.rs`, `#[ignore]`d and `DUJA_HW_TESTS`-gated) walked
/// factors 1.00 → 0.30 in 0.05 steps on an MSI MP273QP and reported **every step
/// down to and including 0.50 accepted, and every step from 0.45 down refused**.
/// Since [`gamma_ramp`] sets entry `i` to `i * 257 * f`, the largest deviation
/// from the identity is at `i = 255`, i.e. `65535 * (1 - f)`: entry 255 is 32768
/// at `f = 0.50`, a deviation of **32767** (accepted), and 29491 at `f = 0.45`, a
/// deviation of **36044** (refused). Two adjacent 0.05 steps therefore bracket the
/// cut-off in `32767 <= limit <= 36043` — an interval of 3277 integers, so the
/// measurement alone does not pin a value; 32768 is the documented rule, sits
/// inside that interval, and is the tightest figure consistent with both. The
/// arithmetic is pinned by `measured_gamma_boundary_matches_the_deviation_limit`.
///
/// Note that 32767 is one unit inside the limit under either a `<` or a `<=`
/// reading of "within", so the fix does not depend on which Microsoft meant.
///
/// This is a **platform** limit. It is unrelated to
/// [`GAMMA_FLOOR`](duja_core::dimmer::GAMMA_FLOOR), which is Duja's own
/// cross-platform *safety* floor on how dark a ramp it is willing to ask for.
const MAX_RAMP_DEVIATION: u32 = 32_768;

/// The lowest gamma factor a Windows ramp write can actually succeed with.
///
/// Derived from [`MAX_RAMP_DEVIATION`]: [`gamma_ramp`]'s worst deviation is
/// `65535 * (1 - f)`, so an accepted ramp needs `65535 * (1 - f) <= 32768`, i.e.
/// `f >= 0.499_992_4`. `0.5` is the smallest `f32` that satisfies that under every
/// reading of the rule, and it is exactly the lowest step the hardware sweep
/// measured as accepted.
///
/// What a caller must know before relying on this:
///
/// - [`GAMMA_FLOOR`](duja_core::dimmer::GAMMA_FLOOR) (`0.3`) is **unreachable on
///   Windows**. Asking for anything below `0.5` yields a refusal, not a darker
///   screen, so the caller has to realise that part of the range some other way —
///   `duja-app`'s `dimming::plan` plans an overlay instead.
/// - A refusal is **not** always observable. Microsoft documents the same call as
///   able to *"fail silently (that is, it returns TRUE, but it doesn't set your
///   ramp)"*, so asking below this bound can also produce a ramp that is not live
///   with no error at all. Staying at or above it is the only reliable protection;
///   see [`ramp_failure_message`] and `docs/debt.md`.
pub const MIN_ACCEPTED_GAMMA: f32 = 0.5;

/// Build a linear gamma ramp that scales output brightness by `factor`.
///
/// `factor` is clamped into [`GAMMA_FLOOR`](duja_core::dimmer::GAMMA_FLOOR)`..=1.0`
/// first (so a crashed ramp is never blacker than the floor). Entry `i` is the
/// identity value `i * 257` (which maps `0..=255` onto the full `0..=65535`
/// range) scaled by `factor` and clamped — so `factor == 1.0` yields the exact
/// identity ramp and smaller factors darken linearly. All three channels are
/// equal (a neutral, non-tinting dim). Total and never-panicking.
#[must_use]
pub fn gamma_ramp(factor: f32) -> GammaRamp {
    let f = f64::from(clamp_gamma(factor));
    let mut row = [0u16; 256];
    for (i, slot) in row.iter_mut().enumerate() {
        // i is a loop index in 0..256, so the conversion is exact.
        let step = f64::from(u16::try_from(i).unwrap_or(0));
        let identity = step * 257.0; // 0.0..=65535.0
        let scaled = (identity * f).round().clamp(0.0, 65535.0);
        // RATIONALE (clippy::cast_possible_truncation / cast_sign_loss):
        // `scaled` is a rounded, clamped value in [0.0, 65535.0], so the cast to
        // u16 is exact and cannot truncate or lose a sign.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            *slot = scaled as u16;
        }
    }
    [row, row, row]
}

/// The identity gamma ramp (no dimming); what a restore writes back.
#[must_use]
pub fn identity_ramp() -> GammaRamp {
    gamma_ramp(1.0)
}

/// A display whose gamma ramp can be driven, identified by its GDI device name
/// (e.g. `\\.\DISPLAY1`).
///
/// Holds no OS handle — a device context is created and destroyed per call — so
/// the value is cheap to keep, [`Send`], and safe to store in a guard.
#[derive(Debug, Clone)]
pub struct GammaDisplay {
    /// NUL-terminated wide device name for `CreateDCW`.
    name_wide: Vec<u16>,
    /// Friendly (lossy UTF-8) device name for reporting.
    name: String,
}

impl GammaDisplay {
    /// The device name (e.g. `\\.\DISPLAY1`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Build from a GDI device name string. Mainly for tests; production code
    /// obtains displays via [`enumerate_gamma_displays`].
    #[must_use]
    pub fn from_device_name(name: &str) -> Self {
        let name_wide = name.encode_utf16().chain(std::iter::once(0)).collect();
        GammaDisplay {
            name_wide,
            name: name.to_owned(),
        }
    }

    /// Open a device context for this display, run `f`, then always delete it.
    fn with_dc<T>(&self, f: impl FnOnce(HDC) -> T) -> Result<T, DimmerError> {
        // SAFETY: `name_wide` is a valid NUL-terminated device name; CreateDCW
        // returns a DC we delete below (null on failure).
        let hdc = unsafe {
            CreateDCW(
                PCWSTR::null(),
                PCWSTR(self.name_wide.as_ptr()),
                PCWSTR::null(),
                None,
            )
        };
        if hdc.is_invalid() {
            return Err(DimmerError::Os(format!(
                "CreateDCW failed for {}",
                self.name
            )));
        }
        let out = f(hdc);
        // SAFETY: `hdc` was created by CreateDCW above and is used only here.
        unsafe {
            let _ = DeleteDC(hdc);
        }
        Ok(out)
    }
}

/// Write a gamma ramp scaled by `factor` to `display`.
///
/// `factor` is clamped to the safe floor. Returns the OS error if the device
/// context or `SetDeviceGammaRamp` fails (some displays/drivers reject gamma
/// changes — the caller should fall back to overlay dimming).
///
/// # Errors
/// [`DimmerError::Os`] if the device context could not be opened or the ramp
/// was rejected by the driver.
pub fn set_gamma(display: &GammaDisplay, factor: f32) -> Result<(), DimmerError> {
    let ramp = gamma_ramp(factor);
    write_ramp(display, &ramp)
}

/// Restore the identity (no-dimming) ramp on `display`.
///
/// # Errors
/// [`DimmerError::Os`] if the device context could not be opened or the ramp
/// was rejected.
pub fn restore_identity(display: &GammaDisplay) -> Result<(), DimmerError> {
    write_ramp(display, &identity_ramp())
}

/// Write a fully-formed ramp to a display's device context.
fn write_ramp(display: &GammaDisplay, ramp: &GammaRamp) -> Result<(), DimmerError> {
    display.with_dc(|hdc| {
        let ptr: *const core::ffi::c_void = std::ptr::from_ref(ramp).cast();
        // SAFETY: `hdc` is a live DC for this display; `ptr` points at a
        // 3×256×u16 ramp with exactly the layout SetDeviceGammaRamp reads.
        unsafe { SetDeviceGammaRamp(hdc, ptr) }.ok().map_err(|e| {
            DimmerError::Os(ramp_failure_message(
                e.code().0,
                &e.to_string(),
                max_identity_deviation(ramp),
            ))
        })
    })?
}

/// The largest absolute deviation from the identity ramp anywhere in `ramp`.
///
/// The identity value for entry `i` is `i * 257` (see [`gamma_ramp`]), so this is
/// the quantity Windows' ramp validation caps at [`MAX_RAMP_DEVIATION`]. Pure and
/// total: every product fits in `u32` (`255 * 257 == 65535`).
fn max_identity_deviation(ramp: &GammaRamp) -> u32 {
    let mut worst = 0u32;
    for row in ramp {
        for (i, &value) in row.iter().enumerate() {
            // `i` is a loop index in 0..256, so the conversion is exact and the
            // product is at most 65535 (saturating only to keep the lint wall's
            // arithmetic policy).
            let identity = u32::try_from(i).unwrap_or(0).saturating_mul(257);
            worst = worst.max(u32::from(value).abs_diff(identity));
        }
    }
    worst
}

/// Describe a failed `SetDeviceGammaRamp` honestly.
///
/// The call returns `FALSE` **without** calling `SetLastError` when Windows' own
/// validation refuses the ramp. `windows-rs` builds the error `.ok()` yields from
/// `GetLastError`, so the code is `0` and the text renders as *"The operation
/// completed successfully. (0x00000000)"* — a message that both contradicts
/// itself and hides the real cause. That shipped: one user's log holds 349 lines
/// of it, spanning the whole life of the feature.
///
/// So: when the OS reported no error code there is no OS error to relay, and this
/// says what actually happened and names the limit that explains it. When there
/// *is* a real code, the OS's own text is passed through unchanged.
///
/// # This covers only the failure shape that *reports* itself
///
/// Microsoft documents a second shape on the same API: a ramp violating the
/// heuristics can make the call *"fail silently (that is, it returns TRUE, but it
/// doesn't set your ramp)"*. Nothing here — and nothing reachable from the write
/// alone — sees that case; it needs a `GetDeviceGammaRamp` read-back comparison
/// (ADR-0002's verify-by-readback idiom). Recorded in `docs/debt.md`, because the
/// comparison tolerance has to be tuned against real hardware: the same page notes
/// the hardware LUT can be lower precision than the 16-bit values written.
fn ramp_failure_message(os_code: i32, os_error: &str, deviation: u32) -> String {
    if os_code == 0 {
        format!(
            "SetDeviceGammaRamp refused the ramp (it set no OS error code, which is how \
             Windows' own ramp validation rejects a ramp straying too far from linear so an \
             application cannot blank the screen): this ramp deviates {deviation} from the \
             identity, the limit is {MAX_RAMP_DEVIATION}, and the lowest factor Windows \
             accepts is {MIN_ACCEPTED_GAMMA}"
        )
    } else {
        format!("SetDeviceGammaRamp failed: {os_error}")
    }
}

/// Enumerate the displays currently attached to the desktop.
#[must_use]
pub fn enumerate_gamma_displays() -> Vec<GammaDisplay> {
    let mut out = Vec::new();
    let mut index = 0u32;
    loop {
        let mut device = DISPLAY_DEVICEW {
            cb: u32::try_from(size_of::<DISPLAY_DEVICEW>()).unwrap_or(0),
            ..Default::default()
        };
        // SAFETY: `device` is a fully-initialized DISPLAY_DEVICEW with cb set;
        // `None` enumerates display adapters by index.
        let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &raw mut device, 0) };
        if !ok.as_bool() {
            break;
        }
        index = index.saturating_add(1);
        if device.StateFlags.0 & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP.0 == 0 {
            continue;
        }
        let name = wide_to_string(&device.DeviceName);
        if name.is_empty() {
            continue;
        }
        out.push(GammaDisplay::from_device_name(&name));
    }
    out
}

/// Best-effort restore of identity gamma on every attached display.
///
/// Used both by `duja-app --restore` and by startup recovery when
/// [`crate::marker_present`] reports a dirty exit. Never fails as a whole: it reports
/// which displays it restored and which it could not.
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

// What a [`restore_all`] pass did. The shape is the same on all three platforms,
// so it lives in one unconditional module and the crate root exports it from
// there; here a row is a GDI device name and identity gamma, and `failed` really
// can be non-empty. See `crate::gamma_support`.
use crate::gamma_support::RestoreReport;

/// RAII owner of the screen's software-dimming state.
///
/// Created and held by the app for as long as gamma dimming might be engaged.
/// [`engage_gamma`](Self::engage_gamma) writes the crash marker (once) and drives
/// a display's gamma; [`Drop`] restores identity gamma on every display it
/// touched — **even on a panic unwind** — and clears the marker.
///
/// Overlay windows are *not* owned here: they die with the process, so the
/// `WindowsDimmer`'s own teardown covers them. This guard exists for the one
/// piece of screen state that outlives the process — the gamma ramp.
#[derive(Debug)]
pub struct ScreenStateGuard {
    marker: Option<PathBuf>,
    marked: bool,
    touched: Vec<GammaDisplay>,
}

impl ScreenStateGuard {
    /// A guard that writes/clears its crash marker at `marker_path` (pass `None`
    /// to skip the marker, e.g. in tests).
    #[must_use]
    pub fn new(marker_path: Option<PathBuf>) -> Self {
        ScreenStateGuard {
            marker: marker_path,
            marked: false,
            touched: Vec::new(),
        }
    }

    /// Engage gamma dimming on `display` at `factor`, recording it for restore.
    ///
    /// Writes the crash marker before the *first* successful or attempted engage.
    ///
    /// # Errors
    /// Propagates [`set_gamma`]'s [`DimmerError`] if the driver rejects the ramp;
    /// the display is not recorded as touched in that case.
    pub fn engage_gamma(&mut self, display: GammaDisplay, factor: f32) -> Result<(), DimmerError> {
        self.mark_if_needed();
        set_gamma(&display, factor)?;
        if !self.touched.iter().any(|d| d.name == display.name) {
            self.touched.push(display);
        }
        Ok(())
    }

    /// Restore identity gamma on one touched display by GDI device name, drop it
    /// from the touched set, and — if that empties the set — clear the crash
    /// marker (no gamma remains engaged). A name that was never touched is a
    /// no-op success.
    ///
    /// Used to reconcile a per-display change: when a display leaves the gamma
    /// path (its slider rose above the sub-floor zone, or it was unplugged) while
    /// others stay engaged.
    ///
    /// # Errors
    /// [`DimmerError::Os`] if the identity ramp could not be written for the
    /// named display.
    pub fn restore_display(&mut self, name: &str) -> Result<(), DimmerError> {
        let Some(pos) = self.touched.iter().position(|d| d.name == name) else {
            return Ok(());
        };
        let display = self.touched.remove(pos);
        // Restore first; only *keep* it forgotten (and maybe clear the marker) on
        // success. A failed restore puts the display back and leaves the marker,
        // so the ramp is retried on Drop and recovered on the next launch.
        if let Err(e) = restore_identity(&display) {
            self.touched.insert(pos, display);
            return Err(e);
        }
        if self.touched.is_empty() {
            self.clear_marker_now();
        }
        Ok(())
    }

    /// Restore identity gamma on every touched display now and return what was
    /// restored.
    ///
    /// The crash marker is cleared **only if every restore succeeded**. Any
    /// display whose restore failed is kept in the touched set — so [`Drop`]
    /// retries it and `touched` keeps reflecting the still-engaged ramps — and
    /// the marker is left in place, so a persistent unrestored ramp still
    /// triggers [`restore_all`] recovery on the next launch.
    pub fn restore_now(&mut self) -> RestoreReport {
        let mut report = RestoreReport::default();
        let mut still_touched = Vec::new();
        for display in self.touched.drain(..) {
            match restore_identity(&display) {
                Ok(()) => report.restored.push(display.name().to_owned()),
                Err(e) => {
                    report
                        .failed
                        .push((display.name().to_owned(), e.to_string()));
                    still_touched.push(display);
                }
            }
        }
        self.touched = still_touched;
        if report.is_clean() {
            self.clear_marker_now();
        }
        report
    }

    /// Displays this guard has engaged gamma on and not yet restored.
    #[must_use]
    pub fn touched(&self) -> &[GammaDisplay] {
        &self.touched
    }

    fn mark_if_needed(&mut self) {
        if self.marked {
            return;
        }
        if let Some(path) = &self.marker {
            let _ = mark_dirty(path);
        }
        self.marked = true;
    }

    fn clear_marker_now(&mut self) {
        if let Some(path) = &self.marker {
            let _ = clear_marker(path);
        }
        self.marked = false;
    }
}

impl Drop for ScreenStateGuard {
    fn drop(&mut self) {
        // Best-effort: restore every touched display. Runs during a panic unwind
        // too, so it must never itself panic — every call here swallows its error.
        // The marker is cleared ONLY if every restore succeeded; if any failed it
        // is kept, so the next launch's `marker_present` triggers `restore_all`
        // (the never-brick net for a persistent, unrestored gamma ramp).
        let mut all_restored = true;
        for display in self.touched.drain(..) {
            if restore_identity(&display).is_err() {
                all_restored = false;
            }
        }
        if all_restored {
            self.clear_marker_now();
        }
    }
}

/// Decode a fixed-size wide `DeviceName` buffer up to its first NUL.
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(buf.get(..end).unwrap_or(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Not re-exported from this module any more (see `crate::marker`), but the
    // guard's own tests still assert on what it did to the file.
    use crate::marker::marker_present;
    use duja_core::dimmer::GAMMA_FLOOR;

    #[test]
    fn identity_ramp_is_exact_scaled_index() {
        let r = identity_ramp();
        for (i, &v) in r[0].iter().enumerate() {
            let expected = u16::try_from(i).unwrap() * 257;
            assert_eq!(v, expected, "identity entry {i}");
        }
    }

    #[test]
    fn all_three_channels_equal() {
        let r = gamma_ramp(0.6);
        assert_eq!(r[0], r[1]);
        assert_eq!(r[1], r[2]);
    }

    #[test]
    fn ramp_is_monotonic_nondecreasing() {
        for &f in &[GAMMA_FLOOR, 0.5, 0.75, 1.0] {
            let r = gamma_ramp(f);
            for w in r[0].windows(2) {
                if let [lo, hi] = w {
                    assert!(hi >= lo, "non-monotonic at factor {f}");
                }
            }
        }
    }

    #[test]
    fn ramp_endpoints() {
        let r = gamma_ramp(0.5);
        assert_eq!(r[0][0], 0);
        // top entry = round(65535 * 0.5) = 32768 (255*257=65535).
        assert_eq!(r[0][255], 32768);
    }

    // --- The OS-imposed minimum factor, and why it is 0.5 -------------------
    //
    // These are pure arithmetic over `gamma_ramp`, so they need no display: they
    // pin the derivation that `MAX_RAMP_DEVIATION` / `MIN_ACCEPTED_GAMMA` rest on
    // against the two empirical facts the hardware sweep produced (0.50 accepted,
    // 0.45 refused). If the ramp maths ever changes shape, the constants stop
    // matching the measurement here rather than silently on a user's desk.

    #[test]
    fn measured_gamma_boundary_matches_the_deviation_limit() {
        // The sweep's two neighbouring outcomes, in deviation units. `65535·(1−f)`
        // at i = 255: 0.50 ⇒ 32767 (accepted), 0.45 ⇒ 36044 (refused).
        assert_eq!(max_identity_deviation(&gamma_ramp(0.50)), 32_767);
        assert_eq!(max_identity_deviation(&gamma_ramp(0.45)), 36_044);
        // ...which is consistent with the documented limit of 32768: the accepted
        // ramp is within it and the refused one is not. (32767 is one unit inside
        // under either a `<` or a `<=` reading of Microsoft's "within 32768".)
        assert!(
            max_identity_deviation(&gamma_ramp(0.50)) <= MAX_RAMP_DEVIATION,
            "the measured lowest ACCEPTED factor must be within the limit"
        );
        assert!(
            max_identity_deviation(&gamma_ramp(0.45)) > MAX_RAMP_DEVIATION,
            "the measured highest REFUSED factor must exceed the limit"
        );
    }

    #[test]
    fn min_accepted_gamma_is_the_lowest_factor_within_the_limit() {
        assert!(
            max_identity_deviation(&gamma_ramp(MIN_ACCEPTED_GAMMA)) <= MAX_RAMP_DEVIATION,
            "MIN_ACCEPTED_GAMMA must itself be acceptable"
        );
        // And it is the boundary, not merely somewhere inside: a hair below it the
        // deviation already exceeds the limit. Written relative to the constant, so
        // raising MIN_ACCEPTED_GAMMA (which would needlessly shrink the reachable
        // gamma range) reds here instead of passing silently.
        assert!(
            max_identity_deviation(&gamma_ramp(MIN_ACCEPTED_GAMMA - 0.001)) > MAX_RAMP_DEVIATION,
            "MIN_ACCEPTED_GAMMA must be at the boundary, not comfortably above it"
        );
        // The consequence the app has to live with: Duja's own cross-platform
        // safety floor is unreachable on Windows.
        assert!(
            max_identity_deviation(&gamma_ramp(GAMMA_FLOOR)) > MAX_RAMP_DEVIATION,
            "GAMMA_FLOOR is below what Windows accepts — the app must plan an overlay"
        );
    }

    #[test]
    fn identity_ramp_deviates_from_itself_by_nothing() {
        // Why `duja --restore` succeeds on the very display that refuses a dim:
        // the identity ramp is the one ramp validation can never reject.
        assert_eq!(max_identity_deviation(&identity_ramp()), 0);
    }

    // --- The error text -----------------------------------------------------

    #[test]
    fn a_zero_code_failure_never_claims_the_operation_succeeded() {
        // The reported defect: `SetDeviceGammaRamp` returns FALSE without setting a
        // last error, so `windows-rs` renders GetLastError() == 0 as "The operation
        // completed successfully. (0x00000000)". 349 log lines said exactly that
        // while the screen did not dim.
        let msg = ramp_failure_message(
            0,
            "The operation completed successfully. (0x00000000)",
            36_044,
        );
        assert!(
            !msg.contains("completed successfully"),
            "a refusal must never claim success: {msg}"
        );
        assert!(msg.contains("refused"), "{msg}");
        assert!(
            msg.contains("36044"),
            "the offending ramp's own deviation is named: {msg}"
        );
        assert!(msg.contains("32768"), "the limit is named: {msg}");
        assert!(
            msg.contains("0.5"),
            "the lowest accepted factor is named: {msg}"
        );
    }

    #[test]
    fn a_real_os_error_is_still_reported_verbatim() {
        // The other half of the contract: when Windows *does* set an error, its own
        // text is what the user needs, so it passes through unchanged.
        let msg = ramp_failure_message(
            -2_147_024_809,
            "The parameter is incorrect. (0x80070057)",
            100,
        );
        assert_eq!(
            msg,
            "SetDeviceGammaRamp failed: The parameter is incorrect. (0x80070057)"
        );
    }

    #[test]
    fn factor_is_clamped_to_floor() {
        // A factor below the floor produces the same ramp as the floor itself.
        assert_eq!(gamma_ramp(0.0), gamma_ramp(GAMMA_FLOOR));
        assert_eq!(gamma_ramp(f32::NAN), identity_ramp());
    }

    #[test]
    fn guard_without_marker_or_touch_is_a_noop_on_drop() {
        let guard = ScreenStateGuard::new(None);
        assert!(guard.touched().is_empty());
        drop(guard); // must not panic or touch anything
    }

    #[test]
    fn guard_marks_and_clears_marker() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("duja-guard-marker-{}.tmp", std::process::id()));
        let _ = clear_marker(&path);
        let mut guard = ScreenStateGuard::new(Some(path.clone()));
        // Directly exercise the marker path (no real display needed).
        guard.mark_if_needed();
        assert!(marker_present(&path));
        guard.restore_now();
        assert!(!marker_present(&path));
    }

    #[test]
    fn restore_display_of_untouched_name_is_a_noop() {
        // Restoring a display the guard never engaged must not error and must not
        // touch the marker.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("duja-restore-display-{}.tmp", std::process::id()));
        let _ = clear_marker(&path);
        let mut guard = ScreenStateGuard::new(Some(path.clone()));
        guard.mark_if_needed();
        assert!(marker_present(&path));
        // Never-touched name: no-op success, marker untouched (gamma still "on").
        guard.restore_display(r"\\.\NOPE").unwrap();
        assert!(marker_present(&path));
        guard.restore_now();
        assert!(!marker_present(&path));
    }

    #[test]
    fn restore_report_cleanliness() {
        let mut r = RestoreReport::default();
        assert!(r.is_clean());
        r.failed.push(("X".to_owned(), "boom".to_owned()));
        assert!(!r.is_clean());
    }

    #[test]
    fn gamma_display_from_name_roundtrips() {
        let d = GammaDisplay::from_device_name(r"\\.\DISPLAY1");
        assert_eq!(d.name(), r"\\.\DISPLAY1");
        // NUL-terminated wide buffer.
        assert_eq!(d.name_wide.last(), Some(&0));
    }

    // --- Fix 2: the crash marker must survive a FAILED restore -------------
    //
    // The marker exists to recover a persistent, unrestored gamma ramp after an
    // unclean or failed exit. Clearing it on a restore that *failed* removes the
    // safety net in the exact case it was designed for. These tests inject a
    // display whose identity restore is guaranteed to fail (a bogus GDI device
    // name → `CreateDCW` fails) and assert the marker is retained.

    /// A GDI device name that does not exist, so `CreateDCW`/restore always fail.
    const BOGUS_DEVICE: &str = r"\\.\DUJA_BOGUS_DISPLAY_DEVICE";

    fn unique_marker(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("duja-{tag}-{}.tmp", std::process::id()))
    }

    /// Build a touched display whose restore fails, asserting the precondition so
    /// a machine where the bogus name *succeeds* fails loudly instead of silently
    /// passing for the wrong reason.
    fn failing_display() -> GammaDisplay {
        let d = GammaDisplay::from_device_name(BOGUS_DEVICE);
        assert!(
            restore_identity(&d).is_err(),
            "precondition: a bogus GDI device must fail restore_identity"
        );
        d
    }

    #[test]
    fn restore_now_keeps_marker_when_a_restore_fails() {
        let path = unique_marker("restore-now-fail");
        let _ = clear_marker(&path);
        let mut guard = ScreenStateGuard::new(Some(path.clone()));
        guard.mark_if_needed();
        assert!(marker_present(&path));
        guard.touched.push(failing_display());

        let report = guard.restore_now();
        assert!(!report.is_clean(), "the failed restore must be reported");
        assert!(
            marker_present(&path),
            "a failed restore_now must NOT clear the marker (never-brick net)"
        );
        // Decision: a failed display is retained in `touched` so Drop retries it.
        assert_eq!(guard.touched().len(), 1, "failed display stays touched");

        guard.touched.clear();
        let _ = clear_marker(&path);
    }

    #[test]
    fn restore_display_keeps_display_and_marker_when_restore_fails() {
        let path = unique_marker("restore-display-fail");
        let _ = clear_marker(&path);
        let mut guard = ScreenStateGuard::new(Some(path.clone()));
        guard.mark_if_needed();
        guard.touched.push(failing_display());

        let result = guard.restore_display(BOGUS_DEVICE);
        assert!(result.is_err(), "a failed restore must surface its error");
        assert_eq!(
            guard.touched().len(),
            1,
            "a failed restore must keep the display touched"
        );
        assert!(
            marker_present(&path),
            "a failed restore_display must NOT clear the marker"
        );

        guard.touched.clear();
        let _ = clear_marker(&path);
    }

    #[test]
    fn drop_keeps_marker_when_a_restore_fails() {
        let path = unique_marker("drop-fail");
        let _ = clear_marker(&path);
        {
            let mut guard = ScreenStateGuard::new(Some(path.clone()));
            guard.mark_if_needed();
            guard.touched.push(failing_display());
            // guard drops here: the failed restore must SKIP clearing the marker.
        }
        assert!(
            marker_present(&path),
            "Drop must keep the marker when a restore fails, so the next launch recovers"
        );
        let _ = clear_marker(&path);
    }
}

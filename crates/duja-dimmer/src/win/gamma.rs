//! The opt-in gamma-ramp path and its crash-safety machinery.
//!
//! Gamma is **not** on the default dimming path: an overlay reaches true black
//! without touching the GPU's gamma ramp, and — unlike a window — a gamma ramp
//! **persists after the process dies**. A crash mid-dim would otherwise leave
//! the user staring at a too-dark desktop with no obvious cure. This module is
//! therefore engaged only explicitly, through [`ScreenStateGuard`], which:
//!
//! - writes a **marker file** (atomic create) before the first gamma engage, so
//!   a fresh start can detect a dirty exit ([`marker_present`]) and call
//!   [`restore_all`] to recover;
//! - restores identity gamma on every touched display on drop, **including a
//!   panic unwind**;
//! - clears the marker on clean teardown.
//!
//! The ramp maths ([`gamma_ramp`]) is pure and unit-tested on every target; the
//! Win32 calls are Windows-only and covered by the hardware-gated live tests.

use std::path::{Path, PathBuf};

use duja_core::dimmer::{DimmerError, clamp_gamma};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW, DeleteDC, EnumDisplayDevicesW,
    HDC,
};
use windows::Win32::UI::ColorSystem::SetDeviceGammaRamp;
use windows::core::PCWSTR;

/// A 16-bit gamma ramp: three identical channels of 256 entries, in the layout
/// `SetDeviceGammaRamp` expects.
pub type GammaRamp = [[u16; 256]; 3];

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
        unsafe { SetDeviceGammaRamp(hdc, ptr) }
            .ok()
            .map_err(|e| DimmerError::Os(format!("SetDeviceGammaRamp failed: {e}")))
    })?
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
/// [`marker_present`] reports a dirty exit. Never fails as a whole: it reports
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

/// What a [`restore_all`] pass did: the displays whose gamma it reset and the
/// ones it could not (with the OS error text).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// Device names whose gamma was restored to identity.
    pub restored: Vec<String>,
    /// `(device name, error)` for each display that could not be restored.
    pub failed: Vec<(String, String)>,
}

impl RestoreReport {
    /// Whether every attempted display was restored (no failures).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Write the crash marker at `path` (atomic create).
///
/// The marker's mere existence signals "gamma was engaged and may not have been
/// restored". Creating it when it already exists is not an error — the previous
/// run was already dirty.
///
/// # Errors
/// The underlying [`std::io::Error`] if the file could not be created for a
/// reason other than already existing.
pub fn mark_dirty(path: &Path) -> std::io::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Remove the crash marker at `path`. A missing marker is success (idempotent).
///
/// # Errors
/// The underlying [`std::io::Error`] if removal failed for a reason other than
/// the file already being absent.
pub fn clear_marker(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether a crash marker exists at `path` (a dirty prior exit).
#[must_use]
pub fn marker_present(path: &Path) -> bool {
    path.exists()
}

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

    #[test]
    fn factor_is_clamped_to_floor() {
        // A factor below the floor produces the same ramp as the floor itself.
        assert_eq!(gamma_ramp(0.0), gamma_ramp(GAMMA_FLOOR));
        assert_eq!(gamma_ramp(f32::NAN), identity_ramp());
    }

    #[test]
    fn marker_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("duja-dimmer-marker-{}.tmp", std::process::id()));
        let _ = clear_marker(&path);
        assert!(!marker_present(&path));
        mark_dirty(&path).unwrap();
        assert!(marker_present(&path));
        // Idempotent create.
        mark_dirty(&path).unwrap();
        assert!(marker_present(&path));
        clear_marker(&path).unwrap();
        assert!(!marker_present(&path));
        // Idempotent clear.
        clear_marker(&path).unwrap();
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

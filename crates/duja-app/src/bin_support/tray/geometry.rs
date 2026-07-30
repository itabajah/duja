//! Adapts [`duja_platform`]'s screen anchor to the app's pure placement types.
//!
//! The Win32 calls this module used to make (`GetCursorPos`,
//! `MonitorFromPoint`, `GetMonitorInfoW`, `GetDpiForMonitor`) now live in
//! [`duja_platform::geometry`], which is where the project confines `unsafe`.
//! That does not make the binary FFI-free — `ShellExecuteW`, the toast's
//! `AppUserModelID` and the reduced-motion query are still app-local, and are
//! tracked in `docs/debt.md`. What it removes is the FFI that had a genuine
//! cross-platform consumer. What is left here is the two things
//! that genuinely belong to the app: converting the platform crate's
//! [`WorkRect`] into the `positioning::Rect` the pure placement kernel is written
//! against, and applying the anchor's own conversion factors at the two points
//! that need them.
//!
//! The two rect structs are field-identical today, and the conversion is
//! deliberately still written out rather than replaced by making them the same
//! type. Keeping `positioning` free of any dependency is what lets its placement
//! tests run as pure arithmetic on every CI OS, with no platform crate in the
//! graph.
//!
//! # Units
//!
//! Everything the placement kernel sees is in **anchor units** — physical pixels
//! on Windows, points on macOS, per [`duja_platform::geometry`]'s contract. Two
//! factors bridge that to the units either side of it:
//!
//! - [`Placement::logical_to_anchor`] turns the flyout's logical (`.slint` design
//!   unit) size into anchor units, so the placement clamp measures the box the
//!   window will really occupy;
//! - [`Placement::anchor_to_physical`], applied by [`to_physical_position`], turns
//!   the resulting origin into the physical pixels `present_at` hands to
//!   `slint::PhysicalPosition`.
//!
//! Which one carries the monitor's scale depends on the platform, and the pair is
//! spelled out per factor rather than with a "respectively" that a reader has to
//! bind back to the bullets above:
//!
//! - **Windows** (anchor units are physical pixels): `logical_to_anchor` is the
//!   monitor's `scale`, `anchor_to_physical` is `1.0`. The `1.0` is what makes
//!   [`to_physical_position`] a bit-for-bit identity — see its own docs.
//! - **macOS** (anchor units are points, i.e. already logical):
//!   `logical_to_anchor` is `1.0`, `anchor_to_physical` is the monitor's `scale`.

use duja_platform::WorkRect;

use crate::bin_support::positioning::Rect;

/// Where to hang a window: the cursor and work area in **anchor units**, plus the
/// two factors that convert into and out of that space.
///
/// A struct rather than a tuple because the two `f32`s are not interchangeable:
/// swapping them compiles cleanly and mis-places the window by the square of the
/// scale factor.
pub(super) struct Placement {
    /// Cursor position in anchor units, y-down.
    pub(super) cursor: (i32, i32),
    /// Work area of the monitor under the cursor, in anchor units.
    pub(super) work: Rect,
    /// Multiply a logical (`.slint` design-unit) size by this to get anchor
    /// units. See [`duja_platform::TrayAnchor::logical_to_anchor`].
    pub(super) logical_to_anchor: f32,
    /// Multiply an anchor-space coordinate by this to get physical pixels. See
    /// [`duja_platform::TrayAnchor::anchor_to_physical`].
    pub(super) anchor_to_physical: f32,
}

/// The cursor position, the work area of the monitor under it, and the anchor's
/// two conversion factors.
///
/// Never fails: every field falls back to a sane default, so the caller always
/// gets a usable anchor. The monitor's raw `scale` is deliberately *not*
/// forwarded — after [ADR-0021] the consumer multiplies by the factors, never by
/// the scale, because which conversion the scale belongs to depends on the
/// platform's anchor unit.
///
/// [ADR-0021]: https://github.com/itabajah/duja/blob/main/docs/adr/0021-tray-anchor-coordinate-contract.md
pub(super) fn cursor_anchor() -> Placement {
    let anchor = duja_platform::cursor_anchor();
    Placement {
        cursor: anchor.cursor,
        work: rect_from(anchor.work_area),
        logical_to_anchor: anchor.logical_to_anchor(),
        anchor_to_physical: anchor.anchor_to_physical(),
    }
}

/// Convert an anchor-space `(x, y)` into the physical-pixel coordinates
/// `FlyoutShell::present_at` / `SettingsShell::present_at` expect.
///
/// `present_at` passes its `(x, y)` to `slint::PhysicalPosition`, and winit's
/// `set_outer_position` converts a physical position to the platform's own unit
/// by *dividing* by the window's scale factor — so on macOS, where the anchor is
/// already in points, the caller has to pre-multiply for that round trip to come
/// back to the point it meant.
///
/// **Bit-identical on Windows.** With `factor == 1.0` the arithmetic is provably
/// the identity, not merely close to it: every `i32` is exactly representable in
/// `f64`, multiplying by `1.0` is exact, `round` of an integer is that integer,
/// and the cast back is exact. There is no `if factor == 1.0` fast path because
/// none is needed, and a float equality test would have been the weaker guarantee
/// of the two.
pub(super) fn to_physical_position(anchor_xy: (i32, i32), factor: f32) -> (i32, i32) {
    (
        scale_coord(anchor_xy.0, factor),
        scale_coord(anchor_xy.1, factor),
    )
}

/// Scale one anchor-space coordinate to physical pixels, saturating.
///
/// The multiplication happens in `f64` (lossless from `i32`) and the result
/// saturates back into `i32`: a float → integer `as` cast in Rust clamps at the
/// target bounds, so an absurd factor pins the window to the edge of the
/// coordinate space instead of wrapping it to the opposite side. A non-finite
/// factor is treated as the identity rather than letting `NaN as i32` collapse the
/// position to `0` (the top-left corner of the primary monitor, which would look
/// like a deliberate placement).
fn scale_coord(value: i32, factor: f32) -> i32 {
    if !factor.is_finite() {
        return value;
    }
    let scaled = (f64::from(value) * f64::from(factor)).round();
    // RATIONALE (cast_possible_truncation): the `as` cast saturates at the `i32`
    // bounds (and the guard above has already excluded NaN), which is exactly the
    // clamping behaviour documented on this function.
    #[allow(clippy::cast_possible_truncation)]
    let out = scaled as i32;
    out
}

/// Convert the platform crate's work rectangle to the placement kernel's.
fn rect_from(rect: WorkRect) -> Rect {
    Rect {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
    }
}

// These tests run on the Windows lane only: the whole `tray` module is
// `cfg(windows)` (it owns the tray icon and the Slint shells). The
// platform-independent half of the same arithmetic — which factor is which, and
// that they multiply to the scale — is pinned cross-platform in
// `duja-platform`'s `geometry` and `mac_geometry` tests.
#[cfg(test)]
mod tests {
    use super::{rect_from, to_physical_position};
    use duja_platform::WorkRect;

    #[test]
    fn conversion_preserves_every_field_including_a_negative_origin() {
        // A monitor left of or above the primary one has negative virtual-desktop
        // coordinates; dropping the sign would place the flyout on the wrong
        // screen.
        let converted = rect_from(WorkRect {
            x: -1920,
            y: -180,
            w: 2560,
            h: 1400,
        });
        assert_eq!(converted.x, -1920);
        assert_eq!(converted.y, -180);
        assert_eq!(converted.w, 2560);
        assert_eq!(converted.h, 1400);
    }

    #[test]
    fn a_unit_factor_is_the_exact_identity() {
        // This is the guarantee that keeps Windows bit-for-bit unchanged by the
        // anchor-unit contract: on a physical-pixel anchor the factor is 1.0 and
        // the position must come back *exactly* as computed, including at the
        // extremes of the coordinate space and on a negative-origin monitor.
        //
        // The last pair is the one that makes this a real check rather than a
        // plausible one: 1_234_567_891 is not representable in `f32`, so an
        // implementation that did the multiply in `f32` instead of `f64` would
        // hand back 1_234_567_936 here and pass every other case.
        for xy in [
            (0, 0),
            (1588, 608),
            (-1920, -180),
            (i32::MIN, i32::MAX),
            (i32::MAX, i32::MIN),
            (1_234_567_891, -1_234_567_891),
        ] {
            assert_eq!(to_physical_position(xy, 1.0), xy, "identity at factor 1.0");
        }
    }

    #[test]
    fn a_retina_factor_scales_both_axes() {
        // The macOS path: a point-space origin becomes physical pixels for
        // `slint::PhysicalPosition`. Scaling only one axis (or neither) would put
        // the flyout on the wrong part of the screen at 2x.
        assert_eq!(to_physical_position((100, 25), 2.0), (200, 50));
        // Fractional factors round to the nearest pixel rather than truncating:
        // 100 * 1.5 = 150, 25 * 1.5 = 37.5 -> 38.
        assert_eq!(to_physical_position((100, 25), 1.5), (150, 38));
        // Negative coordinates (a screen left of / above the primary) scale with
        // their sign intact.
        assert_eq!(to_physical_position((-100, -25), 2.0), (-200, -50));
    }

    #[test]
    fn an_absurd_or_degenerate_factor_cannot_panic_or_wrap() {
        // `duja_platform` sanitises the factor before we ever see it, so these are
        // defence in depth — but the failure mode they exclude (a wrapped
        // coordinate placing the window on the far side of the desktop) is worse
        // than the mis-placement they replace.
        assert_eq!(
            to_physical_position((i32::MAX, i32::MAX), 4.0),
            (i32::MAX, i32::MAX)
        );
        assert_eq!(
            to_physical_position((i32::MIN, i32::MIN), 4.0),
            (i32::MIN, i32::MIN)
        );
        // Non-finite factors fall back to the identity, not to (0, 0).
        assert_eq!(to_physical_position((640, 480), f32::NAN), (640, 480));
        assert_eq!(to_physical_position((640, 480), f32::INFINITY), (640, 480));
    }
}

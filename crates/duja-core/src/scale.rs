//! Scaling a logical extent onto a device-dependent unit.
//!
//! One three-line calculation two crates need: `duja-ui` converts a logical
//! window extent to **physical pixels** for a winit inner-size request, and
//! `duja-app` converts one to **anchor units** for tray placement. They had
//! copies identical in everything but local names (`dpi::physical` and
//! `positioning::anchor_dim`), which is what this module drains.
//!
//! It lives in `duja-core` because that is the crate *both* depend on. (One
//! direction would have worked — `duja-app` depends on `duja-ui`, so `physical`
//! could have been made public and called across — but that points the arrow the
//! wrong way: a tray-geometry helper has no business living in the UI crate, and
//! `duja-ui` could not have borrowed in the other direction at all.)
//!
//! # A related guard this does *not* unify
//!
//! The degenerate-factor guard below (`is_finite() && >= 0.1`) also appears in
//! `positioning::flyout_height_cap`, in `dpi`'s `Resized` arm, and — canonically,
//! per ADR-0021 §4 — as `duja_platform::geometry::sane_scale`, which both anchor
//! factors route through. Those are **not** folded in here: `duja-core` cannot
//! depend on `duja-platform`, so picking one owner is a design decision rather
//! than a hoist. `docs/debt.md` records it.
//!
//! # This function is unit-agnostic, deliberately
//!
//! The two callers do **not** share a unit, and that is not an accident to be
//! tidied away: ADR-0021 makes the anchor unit platform-dependent — physical
//! pixels on Windows and X11, points on macOS — while `duja-ui`'s conversion is
//! always
//! to physical pixels. Only the *arithmetic* is common, so [`scale_extent`] is
//! named for what it computes rather than for what either caller means by it,
//! and each caller keeps its own unit contract in its own docs.
//!
//! Folding the unit into this signature would quietly make the anchor contract
//! look like a pixel contract, which is exactly the confusion ADR-0021 exists to
//! prevent.

/// Scale `logical` by `factor`, rounded, clamped to at least one unit, and
/// guarded against a degenerate factor.
///
/// A `factor` that is not finite, or below `0.1`, is treated as `1.0`: a scale
/// that small is a bad reading rather than a real display, and honouring it
/// would collapse a window to the one-unit floor. `logical` is likewise floored
/// at `1.0` before scaling, so the result is always at least `1` — a zero-extent
/// window is not a thing either caller can use.
///
/// Total: every input returns, including `NaN` and infinities.
///
/// ```
/// use duja_core::scale::scale_extent;
///
/// assert_eq!(scale_extent(100.0, 1.5), 150);
/// assert_eq!(scale_extent(100.0, 1.0), 100);
/// // A degenerate factor falls back to 1.0 rather than collapsing the extent.
/// assert_eq!(scale_extent(100.0, f32::NAN), 100);
/// assert_eq!(scale_extent(100.0, 0.0), 100);
/// // Never zero.
/// assert_eq!(scale_extent(0.0, 2.0), 2);
/// ```
#[must_use]
pub fn scale_extent(logical: f32, factor: f32) -> u32 {
    let factor = if factor.is_finite() && factor >= 0.1 {
        factor
    } else {
        1.0
    };
    let scaled = (logical.max(1.0) * factor).round();
    // RATIONALE(cast_possible_truncation, cast_sign_loss): `scaled` is finite,
    // >= 1.0, and a rounded extent far inside u32; the guards above rule out
    // negatives, NaN and infinities.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let extent = scaled as u32;
    // The floor is applied AFTER rounding, not before: `logical.max(1.0)` alone
    // is not enough, because a small honoured factor can round a one-unit extent
    // back down to zero (1.0 * 0.1 rounds to 0).
    extent.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_scale_rounds_to_the_nearest_unit() {
        assert_eq!(scale_extent(360.0, 1.25), 450);
        assert_eq!(scale_extent(100.0, 1.005), 101);
    }

    #[test]
    fn a_degenerate_factor_falls_back_to_one_rather_than_collapsing() {
        // Each of these would otherwise produce 0 or a nonsense extent. The
        // fallback is 1.0, NOT the one-unit floor: a bad scale reading must
        // leave the window its logical size, not shrink it to a pixel.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -2.0, 0.09] {
            assert_eq!(scale_extent(200.0, bad), 200, "factor {bad}");
        }
    }

    #[test]
    fn the_smallest_honoured_factor_is_still_honoured() {
        // 0.1 is the boundary and is inside the accepted range, so it scales
        // rather than falling back — a `>` instead of `>=` would return 200.
        assert_eq!(scale_extent(200.0, 0.1), 20);
    }

    #[test]
    fn the_result_is_never_zero() {
        assert_eq!(scale_extent(0.0, 1.0), 1);
        assert_eq!(scale_extent(-50.0, 1.0), 1);
        // A tiny extent under a tiny honoured factor still clears the floor.
        assert_eq!(scale_extent(1.0, 0.1), 1);
    }

    #[test]
    fn a_huge_extent_saturates_rather_than_wrapping() {
        // Rust's float->int `as` has saturated since 1.45, so this is a
        // regression guard on that guarantee rather than on our own arithmetic.
        assert_eq!(scale_extent(f32::MAX, 2.0), u32::MAX);
    }
}

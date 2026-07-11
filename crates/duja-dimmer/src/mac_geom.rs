//! Pure geometry for the macOS overlay backend.
//!
//! Cocoa's coordinate space is bottom-left origin, y-up; Duja's
//! [`DisplayBounds`] (like the Core Graphics *global display space*) is top-left
//! origin, y-down. Placing an overlay over a display therefore needs a vertical
//! flip against the primary display's height — the classically bug-prone step
//! that this module isolates as a pure, OS-free function so it compiles and is
//! exhaustively unit- and property-tested on **every** target, exactly like
//! [`plan`](crate::plan). The cfg-gated `mac` backend is its only caller.
//!
//! # Units
//!
//! On macOS a [`DisplayBounds`] carries **points**, not physical pixels: both
//! `CGDisplayBounds` (the natural enumeration source) and `NSWindow` frames are
//! expressed in points, and the window server maps points to backing pixels per
//! display, so no `HiDPI` scaling happens in this layer. (This differs from the
//! Windows backend, where `DisplayBounds` are virtual-desktop *physical pixels*.)

// RATIONALE (clippy::allow dead_code below): these helpers are compiled on every
// target so their tests run on the host CI, but they are only *called* from the
// `#[cfg(target_os = "macos")]` backend — off macOS they read as dead code.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use duja_core::dimmer::DisplayBounds;

/// A Cocoa window frame in points: **bottom-left** origin, y-up — the coordinate
/// system `NSWindow::setFrame_display` expects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CocoaFrame {
    /// Left edge in Cocoa global points (unchanged from the CG left edge).
    pub x: f64,
    /// **Bottom** edge in Cocoa global points (flipped from the CG top edge).
    pub y: f64,
    /// Width in points.
    pub width: f64,
    /// Height in points.
    pub height: f64,
}

/// Convert Core-Graphics-global [`DisplayBounds`] (top-left origin, y-down) to a
/// Cocoa window frame (bottom-left origin, y-up).
///
/// `primary_height` is the height in points of the primary display
/// (`CGMainDisplayID`, whose CG origin is `(0, 0)`) — the reference edge for the
/// flip. The x axis is identical in both systems; only y flips:
///
/// ```text
/// cocoa_bottom = primary_height - (cg_top + height)
/// ```
///
/// The flip is an involution in y: feeding a `CocoaFrame`'s `y` back through the
/// same formula recovers the original CG top, which the round-trip test asserts.
#[must_use]
pub fn cocoa_overlay_frame(bounds: DisplayBounds, primary_height: f64) -> CocoaFrame {
    let x = f64::from(bounds.x);
    let cg_top = f64::from(bounds.y);
    let width = f64::from(bounds.width);
    let height = f64::from(bounds.height);
    CocoaFrame {
        x,
        y: primary_height - (cg_top + height),
        width,
        height,
    }
}

/// Map a quantized overlay alpha byte (`1..=255`, the value the pure
/// [`plan`](crate::plan) kernel produces) to a Cocoa `alphaValue` in
/// `0.0..=1.0` — the direct analogue of Windows' `LWA_ALPHA` byte.
#[must_use]
pub fn alpha_value(alpha_byte: u8) -> f64 {
    f64::from(alpha_byte) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// All the inputs are integer-valued, so the flip arithmetic is exact; a
    /// tight epsilon (rather than `==`, which trips `clippy::float_cmp`) asserts
    /// the exactness while satisfying the lint wall.
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Recover the CG top edge from a computed Cocoa frame (the inverse flip).
    fn cg_top_from(frame: CocoaFrame, primary_height: f64) -> f64 {
        primary_height - (frame.y + frame.height)
    }

    #[test]
    fn primary_at_origin_maps_to_cocoa_origin() {
        // The primary display: CG (0,0), size 1920x1080, primary height 1080.
        let b = DisplayBounds::new(0, 0, 1920, 1080);
        let f = cocoa_overlay_frame(b, 1080.0);
        assert!(close(f.x, 0.0));
        assert!(close(f.y, 0.0), "primary display sits at Cocoa origin");
        assert!(close(f.width, 1920.0));
        assert!(close(f.height, 1080.0));
    }

    #[test]
    fn display_above_primary_has_positive_cocoa_y() {
        // A second display directly above the 1080-tall primary: CG y = -1080.
        let b = DisplayBounds::new(0, -1080, 1920, 1080);
        let f = cocoa_overlay_frame(b, 1080.0);
        // Its bottom edge sits at the top of the primary: Cocoa y = 1080.
        assert!(close(f.y, 1080.0));
    }

    #[test]
    fn display_below_primary_has_negative_cocoa_y() {
        // A second display directly below the primary: CG y = 1080, height 720.
        let b = DisplayBounds::new(0, 1080, 1280, 720);
        let f = cocoa_overlay_frame(b, 1080.0);
        // Cocoa y = 1080 - (1080 + 720) = -720 (below the origin).
        assert!(close(f.y, -720.0));
    }

    #[test]
    fn x_is_unchanged_including_negative_origins() {
        let b = DisplayBounds::new(-2560, 0, 2560, 1440);
        let f = cocoa_overlay_frame(b, 1440.0);
        assert!(close(f.x, -2560.0), "x passes through unchanged");
    }

    #[test]
    fn alpha_value_endpoints() {
        assert!(close(alpha_value(0), 0.0));
        assert!(close(alpha_value(255), 1.0));
        // 224 is the crate's MAX_ALPHA byte; ~0.8784.
        assert!(close(alpha_value(224), 224.0 / 255.0));
    }

    proptest! {
        /// The y flip round-trips: recovering the CG top from the computed Cocoa
        /// frame returns the original CG top, for any bounds and any primary
        /// height. This is the property that guards against an off-by-height or
        /// sign error in the flip.
        #[test]
        fn flip_round_trips(
            x in -30_000i32..30_000,
            y in -30_000i32..30_000,
            w in 1u32..30_000,
            h in 1u32..30_000,
            primary in 1.0f64..30_000.0,
        ) {
            let b = DisplayBounds::new(x, y, w, h);
            let f = cocoa_overlay_frame(b, primary);
            let recovered_top = cg_top_from(f, primary);
            prop_assert!(close(recovered_top, f64::from(y)));
            // Width, height and x are carried through verbatim.
            prop_assert!(close(f.x, f64::from(x)));
            prop_assert!(close(f.width, f64::from(w)));
            prop_assert!(close(f.height, f64::from(h)));
        }
    }
}

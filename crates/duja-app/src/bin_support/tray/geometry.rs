//! Adapts [`duja_platform`]'s screen anchor to the app's pure placement types.
//!
//! The Win32 calls this module used to make (`GetCursorPos`,
//! `MonitorFromPoint`, `GetMonitorInfoW`, `GetDpiForMonitor`) now live in
//! [`duja_platform::geometry`], which is where the project confines `unsafe` —
//! the app binary is meant to be FFI-free. What is left here is the one thing
//! that genuinely belongs to the app: converting the platform crate's
//! [`WorkRect`] into [`positioning::Rect`], the type the pure placement kernel
//! is written against.
//!
//! The two structs are field-identical today, and the conversion is deliberately
//! still written out rather than replaced by making them the same type. Keeping
//! [`positioning`] free of any dependency is what lets its placement tests run
//! as pure arithmetic on every CI OS, with no platform crate in the graph.

use duja_platform::WorkRect;

use crate::bin_support::positioning::Rect;

/// The cursor position, the work area of the monitor under it, and that
/// monitor's scale factor.
///
/// Coordinates are physical device pixels in the virtual-desktop space, y-down;
/// [`duja_platform::geometry`] documents that contract and each backend converts
/// into it. Never fails: every field falls back to a sane default, so the caller
/// always gets a usable anchor.
pub(super) fn cursor_work_area_and_scale() -> ((i32, i32), Rect, f32) {
    let anchor = duja_platform::cursor_anchor();
    (anchor.cursor, rect_from(anchor.work_area), anchor.scale)
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

#[cfg(test)]
mod tests {
    use super::rect_from;
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
}

//! Screen geometry for anchoring a tray flyout: where the cursor is, which
//! monitor's work area surrounds it, and that monitor's scale factor.
//!
//! # One coordinate space, normalized at the backend
//!
//! Every coordinate this module hands out is in **physical device pixels, in the
//! virtual-desktop space, with y increasing downward**. That is the space
//! Windows already speaks, and it is the space a backend is required to convert
//! *into* — not a description of what the OS happens to return.
//!
//! This matters more than it looks. The obvious alternative — pass each
//! platform's native rectangle straight through and let the caller sort it out —
//! is a silent-failure design, because the two platforms disagree in ways that
//! still typecheck:
//!
//! - **Units.** Win32 rects on a Per-Monitor-V2 process are physical pixels;
//!   `CGDisplayBounds` and `NSScreen.visibleFrame` are **points**. On a Retina
//!   display those differ by the backing scale factor, so an un-converted anchor
//!   places the flyout at roughly double its intended offset. Nothing fails; the
//!   window simply lands in the wrong place, and only on hardware the developer
//!   may not own.
//! - **Y axis.** Win32 is y-down; Cocoa is y-up. An un-flipped anchor mirrors the
//!   flyout to the opposite edge of the screen.
//!
//! Both would be caught instantly on a Mac and never in CI. So the conversion is
//! the backend's job, done once, next to the FFI that knows the native
//! convention — the same discipline [`PlatformEvent`](crate::PlatformEvent)
//! applies to OS notifications, and for the same reason: the consumer should not
//! have to know which OS it is running on.
//!
//! # Never fails
//!
//! [`cursor_anchor`] always returns a usable anchor. Every OS query falls back to
//! a sane default rather than propagating an error, because the caller's only
//! recourse would be to invent the same defaults: a flyout placed on a guessed
//! 1080p work area is a cosmetic problem, while no flyout at all is a broken app.

/// A rectangle in physical device pixels, virtual-desktop space, y-down.
///
/// The origin may be negative: a monitor placed left of or above the primary one
/// has negative coordinates in the virtual desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in physical pixels.
    pub w: u32,
    /// Height in physical pixels.
    pub h: u32,
}

/// Where to hang a tray flyout: the cursor, the work area of the monitor under
/// it, and that monitor's scale.
///
/// "Work area" means the monitor's usable region — the screen minus the taskbar
/// (Windows) or minus the menu bar and Dock (macOS). Anchoring to it rather than
/// to the full screen bounds is what keeps the flyout off the shell furniture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrayAnchor {
    /// Cursor position, physical pixels, virtual-desktop space.
    pub cursor: (i32, i32),
    /// Work area of the monitor under the cursor.
    pub work_area: WorkRect,
    /// Physical pixels per logical pixel on that monitor (1.0 = 96 DPI on
    /// Windows, 1.0 = non-Retina on macOS).
    ///
    /// Queried from the *monitor*, not from a window, so a flyout can be sized
    /// and placed before it is ever shown — a one-shot present with no post-show
    /// resize, which is what stops a software renderer presenting a partial
    /// first frame.
    pub scale: f32,
}

/// The fallback work area when the OS cannot tell us: a 1080p desktop with a
/// bottom taskbar.
///
/// Deliberately a plausible desktop rather than a zero rect. Placement clamps
/// the flyout into whatever rectangle it is given, so a zero rect would pin the
/// window to a corner; a wrong-but-sane rectangle degrades to "slightly
/// misplaced" instead.
const DEFAULT_WORK: WorkRect = WorkRect {
    x: 0,
    y: 0,
    w: 1920,
    h: 1040,
};

/// Clamp a scale factor to something a layout can safely multiply by.
///
/// A non-finite or absurdly small value would silently collapse a window to
/// nothing, so anything outside the plausible range falls back to 1.0.
///
/// Shared by every backend that queries a real scale. Today that is Windows
/// only, so it has no caller on other targets — but it stays cross-platform, and
/// unit-tested on every CI OS, because the next backend to land needs exactly
/// this guard and should not re-derive it.
// RATIONALE (dead_code): no non-Windows backend queries a scale yet; the helper
// and its tests stay cross-platform so the macOS implementation inherits both.
#[cfg_attr(not(windows), allow(dead_code))]
fn sane_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale >= 0.1 {
        scale
    } else {
        1.0
    }
}

#[cfg(windows)]
mod platform {
    use std::mem::size_of;

    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    use super::{DEFAULT_WORK, TrayAnchor, WorkRect, sane_scale};

    /// Windows needs no coordinate conversion: a Per-Monitor-V2 process already
    /// receives physical, y-down, virtual-desktop coordinates.
    pub(super) fn cursor_anchor() -> TrayAnchor {
        let cursor = cursor_pos();
        let point = POINT {
            x: cursor.0,
            y: cursor.1,
        };
        // SAFETY: `MonitorFromPoint` takes a POINT by value and returns a monitor
        // handle (the nearest one when the point is off-screen); no pointers are
        // involved.
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };

        let mut info = MONITORINFO {
            // RATIONALE (cast_possible_truncation): a compile-time struct size,
            // far below u32::MAX.
            #[allow(clippy::cast_possible_truncation)]
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `info.cbSize` is set as documented; `GetMonitorInfoW` fills
        // `info` for a valid monitor handle and returns FALSE otherwise, in which
        // case we keep the fallback rather than reading uninitialized fields.
        let ok = unsafe { GetMonitorInfoW(monitor, std::ptr::addr_of_mut!(info)) };
        let work_area = if ok.as_bool() {
            rect_from(info.rcWork)
        } else {
            DEFAULT_WORK
        };

        TrayAnchor {
            cursor,
            work_area,
            scale: monitor_scale(monitor),
        }
    }

    /// The device scale factor of `monitor` (1.0 = 96 DPI), from its effective
    /// DPI. A failed query leaves the 96 DPI seed, i.e. 1.0.
    fn monitor_scale(monitor: HMONITOR) -> f32 {
        let mut dpi_x: u32 = 96;
        let mut dpi_y: u32 = 96;
        // SAFETY: `GetDpiForMonitor` writes the horizontal/vertical effective DPI
        // into the two out-params for a valid monitor handle; `MDT_EFFECTIVE_DPI`
        // is the documented type. On any error the values are left at our 96
        // (= 1.0) seed, which is why the result is ignored.
        let _ = unsafe {
            GetDpiForMonitor(
                monitor,
                MDT_EFFECTIVE_DPI,
                std::ptr::addr_of_mut!(dpi_x),
                std::ptr::addr_of_mut!(dpi_y),
            )
        };
        // RATIONALE (cast_precision_loss): an effective-DPI value is small
        // (72..960) and exactly representable in f32.
        #[allow(clippy::cast_precision_loss)]
        let scale = dpi_x as f32 / 96.0;
        sane_scale(scale)
    }

    /// The cursor position, or `(0, 0)` if it cannot be read.
    fn cursor_pos() -> (i32, i32) {
        let mut point = POINT::default();
        // SAFETY: `GetCursorPos` writes the cursor position into `point`; on
        // failure `point` keeps its zeroed default.
        let ok = unsafe { GetCursorPos(std::ptr::addr_of_mut!(point)) };
        if ok.is_ok() {
            (point.x, point.y)
        } else {
            (0, 0)
        }
    }

    /// Convert a Win32 `RECT` to [`WorkRect`], clamping a degenerate extent to
    /// zero rather than underflowing.
    fn rect_from(rect: windows::Win32::Foundation::RECT) -> WorkRect {
        let w = u32::try_from(rect.right.saturating_sub(rect.left)).unwrap_or(0);
        let h = u32::try_from(rect.bottom.saturating_sub(rect.top)).unwrap_or(0);
        WorkRect {
            x: rect.left,
            y: rect.top,
            w,
            h,
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{DEFAULT_WORK, TrayAnchor};

    /// No screen-geometry backend on this platform yet.
    ///
    /// **This is a placeholder, not a supported configuration**, and it is
    /// documented as such rather than presented as a query that succeeded. It
    /// returns the same fallback Windows uses when its own OS calls fail, so a
    /// caller gets a usable anchor and a flyout lands *somewhere* plausible
    /// instead of not at all.
    ///
    /// The macOS implementation belongs with the app assembly that consumes it
    /// (P6 wave 2): `NSEvent.mouseLocation` for the cursor,
    /// `NSScreen.visibleFrame` for the work area (which is already the work area
    /// — it excludes the menu bar and Dock), and `backingScaleFactor` for the
    /// scale. All three need converting into this module's coordinate space:
    /// multiply points by the backing scale, and flip y from Cocoa's y-up origin
    /// to the y-down virtual-desktop space. Writing that before there is a tray
    /// to place would put an unverifiable conversion in the tree with no consumer
    /// to exercise it.
    pub(super) fn cursor_anchor() -> TrayAnchor {
        TrayAnchor {
            cursor: (0, 0),
            work_area: DEFAULT_WORK,
            scale: 1.0,
        }
    }
}

/// The cursor position, the work area of the monitor under it, and that
/// monitor's scale — see the [module docs](self) for the coordinate space.
///
/// Never fails and never blocks.
#[must_use]
pub fn cursor_anchor() -> TrayAnchor {
    platform::cursor_anchor()
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_WORK, cursor_anchor, sane_scale};

    #[test]
    fn a_degenerate_scale_falls_back_to_one() {
        // A layout multiplies by this, so a non-finite or ~zero factor would
        // collapse the window rather than merely mis-size it.
        assert!((sane_scale(f32::NAN) - 1.0).abs() < f32::EPSILON);
        assert!((sane_scale(f32::INFINITY) - 1.0).abs() < f32::EPSILON);
        assert!((sane_scale(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((sane_scale(-2.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_plausible_scale_is_passed_through() {
        assert!((sane_scale(1.0) - 1.0).abs() < f32::EPSILON);
        assert!((sane_scale(1.5) - 1.5).abs() < f32::EPSILON);
        assert!((sane_scale(3.0) - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_fallback_work_area_is_a_usable_rectangle() {
        // Placement clamps into whatever rectangle it is handed, so a zero or
        // inverted fallback would pin the flyout to a corner. Asserted in a const
        // block: the values are compile-time constants, so this is a guard on the
        // constant itself rather than a runtime check.
        const { assert!(DEFAULT_WORK.w > 0 && DEFAULT_WORK.h > 0) };
    }

    #[test]
    fn an_anchor_is_always_usable() {
        // Whatever the host, and whether or not the OS answers: a scale a layout
        // can multiply by, and a work area it can clamp into.
        let anchor = cursor_anchor();
        assert!(anchor.scale.is_finite() && anchor.scale >= 0.1);
        assert!(anchor.work_area.w > 0 && anchor.work_area.h > 0);
    }
}

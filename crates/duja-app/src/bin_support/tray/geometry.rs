//! The single Win32 geometry query the tray placement needs: the cursor
//! position and the work area of the monitor under it.
//!
//! Kept tiny and isolated so the *placement* logic stays pure and unit-tested
//! (see [`positioning`](crate::bin_support::positioning)); this only fetches the
//! two rectangles to feed it. All `unsafe` here is documented FFI.

// RATIONALE (clippy::cast_possible_truncation): the only cast here is a Win32
// struct size (`MONITORINFO`) into the `u32` `cbSize` field — a tiny
// compile-time constant that cannot truncate.
#![allow(clippy::cast_possible_truncation)]

use std::mem::size_of;

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::bin_support::positioning::Rect;

/// A reasonable default work area if the OS queries fail (a 1080p desktop with a
/// bottom taskbar).
const DEFAULT_WORK: Rect = Rect {
    x: 0,
    y: 0,
    w: 1920,
    h: 1040,
};

/// The cursor position, the work area of the monitor under it, **and** that
/// monitor's device scale factor (physical / logical pixels).
///
/// The scale is queried up front (from the monitor handle, not the flyout window)
/// so the flyout can be sized + placed correctly in physical pixels *before* it
/// is shown — a one-shot present with no post-show resize, which is what stops
/// the software renderer occasionally presenting a partial first frame (item 1).
/// Every field falls back to a sane default if the OS call fails, so the caller
/// always gets a usable anchor (never panics, never blocks).
pub(super) fn cursor_work_area_and_scale() -> ((i32, i32), Rect, f32) {
    let cursor = cursor_pos();
    let point = POINT {
        x: cursor.0,
        y: cursor.1,
    };
    // SAFETY: `MonitorFromPoint` takes a POINT by value and returns a monitor
    // handle (or the nearest one); no pointers are involved.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };

    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info.cbSize` is set as documented; `GetMonitorInfoW` fills `info`
    // for a valid monitor handle and returns FALSE otherwise (we fall back).
    let ok = unsafe { GetMonitorInfoW(monitor, std::ptr::addr_of_mut!(info)) };
    let work = if ok.as_bool() {
        rect_from(info.rcWork)
    } else {
        DEFAULT_WORK
    };
    (cursor, work, monitor_scale(monitor))
}

/// The device scale factor of `monitor` (1.0 = 96 DPI), from its effective DPI.
///
/// The process is Per-Monitor-V2 DPI-aware (see `build.rs`), so this is the true
/// physical scale. A failed query or a degenerate value falls back to `1.0`.
fn monitor_scale(monitor: HMONITOR) -> f32 {
    let mut dpi_x: u32 = 96;
    let mut dpi_y: u32 = 96;
    // SAFETY: `GetDpiForMonitor` writes the horizontal/vertical effective DPI into
    // the two out-params for a valid monitor handle; `MDT_EFFECTIVE_DPI` is the
    // documented type. On any error the values are left at our 96 (= 1.0) defaults.
    let _ = unsafe {
        GetDpiForMonitor(
            monitor,
            MDT_EFFECTIVE_DPI,
            std::ptr::addr_of_mut!(dpi_x),
            std::ptr::addr_of_mut!(dpi_y),
        )
    };
    // RATIONALE (cast_precision_loss): an effective-DPI value is small (72..960)
    // and exactly representable in f32.
    #[allow(clippy::cast_precision_loss)]
    let scale = dpi_x as f32 / 96.0;
    if scale.is_finite() && scale >= 0.1 {
        scale
    } else {
        1.0
    }
}

/// The cursor position, or `(0, 0)` if it cannot be read.
fn cursor_pos() -> (i32, i32) {
    let mut point = POINT::default();
    // SAFETY: `GetCursorPos` writes the cursor position into `point`.
    let ok = unsafe { GetCursorPos(std::ptr::addr_of_mut!(point)) };
    if ok.is_ok() {
        (point.x, point.y)
    } else {
        (0, 0)
    }
}

/// Convert a Win32 `RECT` to the pure [`Rect`], clamping a degenerate extent to
/// zero rather than underflowing.
fn rect_from(rect: windows::Win32::Foundation::RECT) -> Rect {
    let w = u32::try_from(rect.right.saturating_sub(rect.left)).unwrap_or(0);
    let h = u32::try_from(rect.bottom.saturating_sub(rect.top)).unwrap_or(0);
    Rect {
        x: rect.left,
        y: rect.top,
        w,
        h,
    }
}

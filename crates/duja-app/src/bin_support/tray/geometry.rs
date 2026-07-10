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
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
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

/// The current cursor position and the work area of the monitor under it.
///
/// Both fall back to sane defaults if the OS calls fail, so the caller always
/// gets a usable anchor (never panics, never blocks).
pub(super) fn cursor_and_work_area() -> ((i32, i32), Rect) {
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
    (cursor, work)
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

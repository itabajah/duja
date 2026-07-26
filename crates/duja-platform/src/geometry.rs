//! Screen geometry for anchoring a tray flyout: where the cursor is, which
//! monitor's work area surrounds it, and that monitor's scale factor.
//!
//! # Coordinate space
//!
//! **Top-left origin, y increasing downward**, in the unit the platform's own
//! window-positioning API expects: physical pixels on Windows (a
//! Per-Monitor-V2 process), points on macOS.
//!
//! Orientation is normalized by the backend; the *unit* deliberately is not.
//! Both halves of that need justifying, because the obvious alternative — one
//! global physical-pixel space everywhere — is not implementable:
//!
//! - **Y axis, normalized.** Win32 is y-down and Cocoa is y-up, so an un-flipped
//!   anchor mirrors the flyout to the opposite screen edge. This crate is not the
//!   first to hit that: `duja-dimmer`'s `mac_geom` already flips overlay frames,
//!   and its `cocoa_overlay_frame` is the helper a macOS backend here should
//!   reuse rather than re-derive. Note it needs a *reference height* (the primary
//!   display's) to flip against — the flip is not a local operation.
//! - **Unit, not normalized.** macOS has no coherent global physical-pixel
//!   space: the global space is in points and each `NSScreen` carries its own
//!   `backingScaleFactor`, so multiplying global point coordinates by any single
//!   display's factor makes a Retina built-in and a non-Retina external stop
//!   tiling. Points *are* macOS's window-positioning unit, so the honest common
//!   contract is "the unit the window API takes", which costs the consumer
//!   nothing: placement only ever compares the cursor against the work area and
//!   clamps within it, and both are in the same unit by construction.
//!
//! None of the underlying facts are new here — `duja-dimmer`'s `mac_geom` module
//! docs and `docs/STATUS.md`'s "traps surfaced" paragraph have recorded the
//! points-vs-pixels and y-flip divergences since P6 wave 1. What this module adds
//! is a place for the conversion to *live*: at the backend, next to the FFI that
//! knows the native convention, in the same spirit as
//! [`PlatformEvent`](crate::PlatformEvent) normalizing OS notifications.
//!
//! ## Open question for the macOS backend
//!
//! The consumer converts a *logical* window size into anchor units by
//! multiplying by [`TrayAnchor::scale`] (`positioning::physical_window_size` in
//! `duja-app`). That is right on Windows, where anchor units are physical
//! pixels. On macOS anchor units are already points — i.e. already logical — so
//! that multiplication would double-scale on a Retina display. Resolving it
//! (drop the multiply on macOS, or carry an explicit logical-to-anchor factor
//! that is `scale` on Windows and `1.0` on macOS) belongs with the backend that
//! makes it real, on hardware that can show whether it worked.
//!
//! # Never fails
//!
//! [`cursor_anchor`] always returns a usable anchor. Every OS query falls back to
//! a sane default rather than propagating an error, because the caller's only
//! recourse would be to invent the same defaults: a flyout placed on a guessed
//! 1080p work area is a cosmetic problem, while no flyout at all is a broken app.

/// A rectangle in the platform's window-positioning unit, y-down — see the
/// [module docs](self) for what that means per platform.
///
/// The origin may be negative: a monitor placed left of or above the primary one
/// has negative coordinates in the desktop space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: u32,
    /// Height.
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
    /// Cursor position in the space described by the [module docs](self).
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

/// Substitute 1.0 for a scale factor a layout could not safely multiply by.
///
/// Guards the low end only: a non-finite or near-zero factor would collapse a
/// window to nothing rather than merely mis-size it. There is deliberately **no
/// upper bound** — `GetDpiForMonitor` cannot return one (72..960 DPI), and
/// inventing a ceiling would silently cap a future high-density display instead
/// of showing it. A backend whose source can produce garbage at the top end
/// (a detached or zero-size `NSScreen`, say) must guard that itself.
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

    #[cfg(test)]
    mod tests {
        use super::rect_from;
        use windows::Win32::Foundation::RECT;

        /// The Win32 `RECT` conversion is the one piece of real arithmetic in
        /// this module — sign-preserving edges plus two saturating widths — so it
        /// is pinned here rather than at the app's four-field copy downstream,
        /// which is where a regression could not plausibly be introduced.
        ///
        /// Windows-only, because `RECT` is a Win32 type. Stated rather than
        /// glossed: these run on one of the three CI lanes.
        #[test]
        fn a_negative_origin_survives_the_conversion() {
            // A monitor left of or above the primary one has negative
            // virtual-desktop coordinates. Dropping the sign — or clamping the
            // edges the way the extents are clamped — would place the flyout on
            // the wrong screen entirely.
            let converted = rect_from(RECT {
                left: -1920,
                top: -180,
                right: 640,
                bottom: 1220,
            });
            assert_eq!(converted.x, -1920);
            assert_eq!(converted.y, -180);
            assert_eq!(converted.w, 2560);
            assert_eq!(converted.h, 1400);
        }

        #[test]
        fn an_inverted_rect_yields_zero_extents_rather_than_underflowing() {
            // `right < left` is degenerate, not impossible: a disconnected
            // monitor can report one. The subtraction is signed and the result is
            // `u32`, so an unguarded conversion would wrap to ~4 billion and hand
            // placement an absurd work area.
            let converted = rect_from(RECT {
                left: 100,
                top: 200,
                right: 40,
                bottom: 80,
            });
            assert_eq!(converted.w, 0);
            assert_eq!(converted.h, 0);
            assert_eq!(converted.x, 100, "the origin is reported as given");
            assert_eq!(converted.y, 200);
        }

        #[test]
        fn an_extreme_rect_saturates_instead_of_overflowing() {
            // `right - left` across the full i32 range overflows a plain
            // subtraction; `saturating_sub` is what keeps this finite.
            let converted = rect_from(RECT {
                left: i32::MIN,
                top: i32::MIN,
                right: i32::MAX,
                bottom: i32::MAX,
            });
            assert_eq!(converted.w, i32::MAX as u32);
            assert_eq!(converted.h, i32::MAX as u32);
        }

        #[test]
        fn an_ordinary_primary_monitor_converts_unchanged() {
            let converted = rect_from(RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            });
            assert_eq!(converted, super::super::DEFAULT_WORK);
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
        // A placeholder that returns a plausible-looking anchor is indisting-
        // uishable from a working backend: cursor (0, 0) on a Mac is the top-left
        // corner, right where a menu-bar flyout belongs, so a missing
        // implementation would read as "placed slightly oddly" rather than "not
        // implemented". Fail loudly in debug builds so the first person to run
        // the macOS app finds out immediately; release builds still degrade to a
        // usable anchor rather than panicking at the user.
        debug_assert!(
            false,
            "duja-platform: no screen-geometry backend on this target; \
             cursor_anchor() is returning the placeholder fallback"
        );
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
    #[cfg(windows)]
    use super::cursor_anchor;
    use super::{DEFAULT_WORK, sane_scale};

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

    /// End-to-end smoke test of the real backend: it must not panic, and must
    /// hand placement values it can actually use.
    ///
    /// Windows-only, and deliberately so — this calls the live Win32 query path.
    /// On targets with no backend, [`cursor_anchor`] trips a `debug_assert`
    /// announcing the placeholder, so calling it here would fail the test lane by
    /// design rather than tell us anything.
    #[cfg(windows)]
    #[test]
    fn the_real_backend_returns_a_usable_anchor() {
        let anchor = cursor_anchor();
        assert!(
            anchor.scale.is_finite() && anchor.scale >= 0.1,
            "a layout multiplies by this"
        );
        assert!(
            anchor.work_area.w > 0 && anchor.work_area.h > 0,
            "placement clamps into this"
        );
    }
}

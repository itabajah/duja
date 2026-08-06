//! Screen geometry for anchoring a tray flyout: where the cursor is, which
//! monitor's work area surrounds it, and that monitor's scale factor.
//!
//! # Coordinate space
//!
//! **Top-left origin, y increasing downward**, in the unit the platform's own
//! window-positioning API expects: physical pixels on Windows (a
//! Per-Monitor-V2 process) and on X11 (which has no other kind), points on
//! macOS.
//!
//! Orientation is normalized by the backend; the *unit* deliberately is not.
//! Both halves of that need justifying, because the obvious alternative — one
//! global physical-pixel space everywhere — is not implementable:
//!
//! - **Y axis, normalized.** Win32 is y-down and Cocoa is y-up, so an un-flipped
//!   anchor mirrors the flyout to the opposite screen edge. This crate is not the
//!   first to hit that: `duja-dimmer`'s `mac_geom` already flips overlay frames
//!   with `cocoa_overlay_frame`, and the `mac_geometry` module here carries the
//!   *inverse* flip (Cocoa y-up → this crate's y-down). The two must agree; they
//!   are separate helpers only because `duja-platform` may not depend on
//!   `duja-dimmer`. Note the flip needs a *reference height* (the primary
//!   display's) to flip against — it is not a local operation.
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
//! ## Which unit, and the two conversion factors
//!
//! Because the unit is not normalized, a consumer cannot know it from the type
//! alone — so the anchor names it. [`AnchorUnit`] says which space the
//! coordinates are in, and two derived factors say what to multiply by rather
//! than making the caller branch on the variant:
//!
//! - [`TrayAnchor::logical_to_anchor`] — logical (design-unit) window size →
//!   anchor units. `scale` on Windows and X11, `1.0` on macOS (points *are*
//!   logical).
//! - [`TrayAnchor::anchor_to_physical`] — an anchor-space coordinate → the
//!   physical pixels `slint::PhysicalPosition`/winit want. `1.0` on Windows and
//!   X11, `scale` on macOS.
//!
//! Their product is always the (sanitised) `scale`: logical→winit-physical is
//! `×scale` on every platform, and the two factors are only *where* that single
//! multiplication is split. That is the invariant the unit tests pin, and it is
//! what closes the question [ADR-0021] records: multiplying a logical size by
//! `scale` (as the consumer did before this contract existed) is right on
//! Windows and double-scales on a Retina Mac, where anchor units are already
//! logical.
//!
//! Nothing outside this crate should read [`TrayAnchor::scale`] for placement
//! arithmetic; it is the monitor's DPI/backing scale, not a conversion factor.
//!
//! [ADR-0021]: https://github.com/itabajah/duja/blob/main/docs/adr/0021-tray-anchor-coordinate-contract.md
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

/// The unit a [`TrayAnchor`]'s coordinates are expressed in.
///
/// The orientation of the anchor space is normalized by every backend (top-left
/// origin, y-down); the unit deliberately is not, for the reason the [module
/// docs](self) give. This enum is how a consumer learns which one it got —
/// though it should normally use [`TrayAnchor::logical_to_anchor`] and
/// [`TrayAnchor::anchor_to_physical`] instead of matching on the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorUnit {
    /// Physical device pixels, as Win32 reports them to a Per-Monitor-V2
    /// process: monitor rects, the cursor position, and `SetWindowPos` all speak
    /// this one space, so no conversion happens anywhere on the path.
    ///
    /// Produced by the Windows backend and by the Linux one on X11, where root-
    /// window coordinates are device pixels and winit hands a `PhysicalPosition`
    /// straight through — the same space for the same reason, arrived at from a
    /// different protocol.
    ///
    /// Also used by every fallback anchor (the Linux backend's on Wayland, and
    /// the placeholder on a target with no backend at all), where the scale is a
    /// flat 1.0, so both conversion factors are 1.0 and the distinction cannot
    /// matter.
    PhysicalPixels,
    /// Points: macOS's backing-independent unit, and the one every
    /// window-positioning API there takes.
    ///
    /// Produced by the macOS backend. Points are already *logical*, so a Retina
    /// display's `backingScaleFactor` never enters the placement arithmetic —
    /// but it does enter the final hand-off, because Slint/winit want physical
    /// pixels (see [`TrayAnchor::anchor_to_physical`]).
    Points,
}

/// Where to hang a tray flyout: the cursor, the work area of the monitor under
/// it, and that monitor's scale.
///
/// "Work area" means the monitor's usable region — the screen minus the taskbar
/// (Windows), minus the menu bar and Dock (macOS), or minus whatever the panels
/// have reserved with an EWMH strut (X11). Anchoring to it rather than to the
/// full screen bounds is what keeps the flyout off the shell furniture.
///
/// Windows and macOS are told the answer (`rcWork`, `visibleFrame`); X11 has no
/// per-monitor equivalent to ask for and Duja computes it, which is why
/// `linux_geometry`'s `work_area` exists and why it is the part of the Linux
/// backend with the most tests behind it.
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
    ///
    /// **This is not a conversion factor.** It is the monitor's DPI/backing
    /// scale, and *which* conversion it participates in depends on [`unit`]:
    /// nothing outside this crate should multiply by it for placement. Use
    /// [`logical_to_anchor`] and [`anchor_to_physical`] instead — reading
    /// `scale` directly is exactly the mistake that would double-scale a Retina
    /// Mac.
    ///
    /// [`unit`]: Self::unit
    /// [`logical_to_anchor`]: Self::logical_to_anchor
    /// [`anchor_to_physical`]: Self::anchor_to_physical
    pub scale: f32,
    /// Which unit [`cursor`](Self::cursor) and [`work_area`](Self::work_area)
    /// are in — set by the backend that produced them.
    pub unit: AnchorUnit,
}

impl TrayAnchor {
    /// Multiply a *logical* (design-unit) window size by this to get anchor
    /// units, i.e. the space [`cursor`](Self::cursor) and
    /// [`work_area`](Self::work_area) live in.
    ///
    /// [`AnchorUnit::PhysicalPixels`] ⇒ the (sanitised) [`scale`](Self::scale),
    /// because a logical size must grow to physical pixels before it can be
    /// clamped against a physical work area. [`AnchorUnit::Points`] ⇒ `1.0`,
    /// because points already *are* logical units.
    ///
    /// See [`anchor_to_physical`](Self::anchor_to_physical) for the invariant the
    /// two factors satisfy together.
    #[must_use]
    pub fn logical_to_anchor(&self) -> f32 {
        match self.unit {
            AnchorUnit::PhysicalPixels => sane_scale(self.scale),
            AnchorUnit::Points => 1.0,
        }
    }

    /// Multiply an anchor-space coordinate by this to get the physical pixels
    /// `slint::PhysicalPosition` (and winit's `set_outer_position` beneath it)
    /// expect.
    ///
    /// [`AnchorUnit::PhysicalPixels`] ⇒ `1.0`: the anchor is already in that
    /// space. [`AnchorUnit::Points`] ⇒ the (sanitised) [`scale`](Self::scale),
    /// because winit converts a physical position back to points by *dividing*
    /// by the window's scale factor, so a caller with points in hand has to
    /// pre-multiply for the round trip to be the identity.
    ///
    /// # Invariant
    ///
    /// `logical_to_anchor() * anchor_to_physical()` equals the sanitised
    /// [`scale`](Self::scale) on **every** variant: logical → winit-physical is
    /// `×scale` everywhere, and the two factors only say where that single
    /// multiplication is split. A backend that gets the split wrong is caught by
    /// the unit test that asserts this.
    #[must_use]
    pub fn anchor_to_physical(&self) -> f32 {
        match self.unit {
            AnchorUnit::PhysicalPixels => 1.0,
            AnchorUnit::Points => sane_scale(self.scale),
        }
    }
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
/// Shared by every backend that queries a real scale (Windows' effective DPI,
/// macOS' `backingScaleFactor`, X11's `Xft.dpi`-and-friends chain) **and** by
/// the two conversion factors on
/// [`TrayAnchor`], which is why it now has a live caller on every target and no
/// longer carries a dead-code allow: a degenerate scale must be neutralised
/// once, at the single place both factors read it.
pub(crate) fn sane_scale(scale: f32) -> f32 {
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

    use super::{AnchorUnit, DEFAULT_WORK, TrayAnchor, WorkRect, sane_scale};

    /// Windows needs no coordinate conversion: a Per-Monitor-V2 process already
    /// receives physical, y-down, virtual-desktop coordinates — which is exactly
    /// [`AnchorUnit::PhysicalPixels`].
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
            unit: AnchorUnit::PhysicalPixels,
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
            // The macOS backend's `MAX_EXTENT` claims to match *this* ceiling,
            // and this is the only lane that runs the real `rect_from`, so the
            // claim is pinned here against live Windows output rather than
            // against a literal.
            //
            // That covers all three backends between two assertions rather than
            // one: `linux_geometry` carries a third `MAX_EXTENT` and pins it
            // against this same macOS constant, on every lane (both pure modules
            // compile under `cfg(test)` everywhere). Windows = macOS here and
            // Linux = macOS there, so no pair can drift apart without one of the
            // two reddening.
            assert_eq!(
                converted.w,
                crate::mac_geometry::MAX_EXTENT,
                "every backend must cap an absurd extent at the same value"
            );
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

#[cfg(target_os = "macos")]
mod platform {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen};
    use objc2_foundation::{NSPoint, NSRect};

    use super::{AnchorUnit, DEFAULT_WORK, TrayAnchor};
    use crate::mac_geometry::{CocoaPoint, CocoaRect, CocoaScreen, anchor_from_screens};

    /// The anchor used when the `AppKit` query cannot run at all — see
    /// [`cursor_anchor`] for when that is.
    ///
    /// [`AnchorUnit::Points`] with a 1.0 scale, so both conversion factors are
    /// 1.0 and the fallback behaves identically to the Windows one.
    const FALLBACK: TrayAnchor = TrayAnchor {
        cursor: (0, 0),
        work_area: DEFAULT_WORK,
        scale: 1.0,
        unit: AnchorUnit::Points,
    };

    /// The macOS anchor: `NSEvent`'s global mouse location and the `NSScreen`
    /// under it, converted from Cocoa's bottom-left/y-up points into this
    /// module's top-left/y-down points.
    ///
    /// Falls back to [`FALLBACK`] when the query cannot run — either because
    /// there is no [`MainThreadMarker`] (`NSScreen` is a main-thread-only API) or
    /// because `AppKit` reports no screens at all. Both are **guards, not the
    /// normal path**: every call site is a tray/flyout action dispatched on the
    /// Slint main thread, which is the process's real main thread, so the marker
    /// is available. Guarding rather than asserting is what keeps
    /// [`cursor_anchor`](super::cursor_anchor)'s "never fails" contract true if a
    /// future caller reaches it from a worker.
    pub(super) fn cursor_anchor() -> TrayAnchor {
        query_anchor().unwrap_or(FALLBACK)
    }

    /// Collect the live `AppKit` geometry and hand it to the pure conversion.
    ///
    /// The FFI here is deliberately dumb — read the cursor, read every screen's
    /// `frame`/`visibleFrame`/`backingScaleFactor`, copy them into plain `f64`
    /// structs — so that every decision (which screen, how to flip y, how to
    /// clamp a degenerate value) lives in `mac_geometry`, where it is unit-tested
    /// on every CI host including the ones with no Mac.
    fn query_anchor() -> Option<TrayAnchor> {
        let mtm = MainThreadMarker::new()?;
        // `visibleFrame` is *already* the work area: `AppKit` subtracts the menu
        // bar and the Dock from it, so no shell-furniture arithmetic is needed
        // here (unlike Win32, which reports `rcMonitor` and `rcWork` separately).
        let screens = NSScreen::screens(mtm);
        let count = screens.count();
        let mut collected: Vec<CocoaScreen> = Vec::with_capacity(count);
        for index in 0..count {
            let screen = screens.objectAtIndex(index);
            collected.push(CocoaScreen {
                frame: rect_from(screen.frame()),
                visible_frame: rect_from(screen.visibleFrame()),
                backing_scale: screen.backingScaleFactor(),
            });
        }
        // The flip's reference height is the *first* screen's — not the largest,
        // and not the one under the cursor. Cocoa's global coordinate origin is
        // the bottom-left corner of the screen carrying the menu bar, and that is
        // index 0 of `NSScreen::screens` (the screen whose `frame.origin` is
        // `(0, 0)`); `NSScreen::mainScreen` is a different thing — it follows the
        // *key window*, so it moves as the user clicks around and is the wrong
        // reference. Using any other screen's height offsets every flipped
        // coordinate by the difference of the two heights, which is what
        // `mac_geometry`'s `the_flip_reference_is_the_first_screens_height_not_the_cursors`
        // pins.
        anchor_from_screens(point_from(NSEvent::mouseLocation()), &collected)
    }

    /// Copy an `NSPoint` into the pure Cocoa point type.
    ///
    /// The fields move across unconverted because `CGFloat` is `f64` on every
    /// 64-bit Apple target, which is every target Duja builds for (aarch64 and
    /// `x86_64` macOS). Were it ever `f32`, these would stop compiling rather than
    /// silently narrow.
    fn point_from(point: NSPoint) -> CocoaPoint {
        CocoaPoint {
            x: point.x,
            y: point.y,
        }
    }

    /// Copy an `NSRect` into the pure Cocoa rect type (still y-up).
    fn rect_from(rect: NSRect) -> CocoaRect {
        CocoaRect {
            x: rect.origin.x,
            y: rect.origin.y,
            w: rect.size.width,
            h: rect.size.height,
        }
    }

    // No unit tests here, deliberately: every line above is either an `AppKit`
    // call or a field-for-field copy of one, and neither can be exercised without
    // a Mac with a window server. The arithmetic they feed is tested in
    // `mac_geometry`; a test here could only assert that a struct copy copies.
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{AnchorUnit, DEFAULT_WORK, TrayAnchor};
    use crate::linux_geometry::{DisplayEnv, WindowSystem, window_system};

    /// The anchor used when the session is not one this backend can measure.
    ///
    /// [`AnchorUnit::PhysicalPixels`] with a 1.0 scale, so both conversion
    /// factors are 1.0 and it behaves identically to the Windows and macOS
    /// fallbacks.
    const FALLBACK: TrayAnchor = TrayAnchor {
        cursor: (0, 0),
        work_area: DEFAULT_WORK,
        scale: 1.0,
        unit: AnchorUnit::PhysicalPixels,
    };

    /// The Linux anchor: a real measurement on X11, the fallback on Wayland.
    ///
    /// # Why Wayland is not a port of this
    ///
    /// The X11 path here is close to the Windows one — ask where the pointer is,
    /// find the display under it, take that display's work area and scale. Every
    /// step of that is unavailable on Wayland, and not by omission:
    ///
    /// - **There is no global cursor position.** A Wayland client learns pointer
    ///   coordinates only from events delivered to its own surfaces, in that
    ///   surface's coordinates. There is no request that answers "where is the
    ///   pointer", by design.
    /// - **A client cannot position its own toplevel.** `set_outer_position` is a
    ///   no-op on winit's Wayland backend, because `xdg_toplevel` has no request
    ///   for it. So even a correct anchor would not move the flyout.
    /// - **There is no work area to read.** A layer-shell panel's exclusive zone
    ///   is known to the compositor and to no one else.
    ///
    /// The Wayland answer is therefore a different mechanism rather than a second
    /// implementation of this one: the screen coordinates the tray host passes to
    /// `StatusNotifierItem.Activate(x, y)`, which ksni surfaces, feeding a
    /// compositor-side positioner. ADR-0010 records that, wave 5b builds it, and
    /// `docs/debt.md` carries the gap until then. Returning [`FALLBACK`] in the
    /// meantime is honest: the flyout lands where the compositor puts it, which
    /// is what would happen whatever this function returned.
    ///
    /// A session with neither display server — a TTY launch, a service unit —
    /// takes the same path, and has no tray to click in the first place.
    pub(super) fn cursor_anchor() -> TrayAnchor {
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let wayland_socket = std::env::var("WAYLAND_SOCKET").ok();
        let display = std::env::var("DISPLAY").ok();
        let env = DisplayEnv {
            wayland_display: wayland_display.as_deref(),
            wayland_socket: wayland_socket.as_deref(),
            display: display.as_deref(),
        };
        match window_system(env) {
            WindowSystem::X11 => crate::linux::geometry::cursor_anchor().unwrap_or(FALLBACK),
            WindowSystem::Wayland | WindowSystem::None => FALLBACK,
        }
    }

    // No unit tests here, deliberately, and for the reason the macOS backend
    // gives: every line above is an environment read, a `match` on a rule that is
    // tested in `linux_geometry`, or a call into the X11 module that needs a
    // server. A test here could only assert that `std::env::var` reads the
    // environment — and would have to mutate the process's environment to do it,
    // which is unsound in a threaded test harness.
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    use super::{AnchorUnit, DEFAULT_WORK, TrayAnchor};

    /// No screen-geometry backend on this platform.
    ///
    /// **This is a placeholder, not a supported configuration**, and it is
    /// documented as such rather than presented as a query that succeeded. It
    /// returns the same fallback Windows uses when its own OS calls fail, so a
    /// caller gets a usable anchor and a flyout lands *somewhere* plausible
    /// instead of not at all.
    ///
    /// Nothing Duja ships reaches this. The three targets with a tray each have a
    /// backend above, and this arm exists so the crate still compiles on a
    /// fourth — a BSD, say, where the X11 module would very nearly work and has
    /// simply never been built or run.
    ///
    /// It is declared [`AnchorUnit::PhysicalPixels`] purely because the scale is a
    /// flat 1.0, so both conversion factors are 1.0 and the choice cannot affect
    /// anything until a real backend replaces it.
    pub(super) fn cursor_anchor() -> TrayAnchor {
        // A placeholder that returns a plausible-looking anchor is indisting-
        // uishable from a working backend, so a missing implementation would read
        // as "placed slightly oddly" rather than "not implemented". Fail loudly
        // in debug builds so the first person to run it finds out immediately;
        // release builds still degrade to a usable anchor rather than panicking
        // at the user.
        debug_assert!(
            false,
            "duja-platform: no screen-geometry backend on this target; \
             cursor_anchor() is returning the placeholder fallback"
        );
        TrayAnchor {
            cursor: (0, 0),
            work_area: DEFAULT_WORK,
            scale: 1.0,
            unit: AnchorUnit::PhysicalPixels,
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
    use super::{AnchorUnit, DEFAULT_WORK, TrayAnchor, WorkRect, sane_scale};

    /// An anchor with the given unit and scale; the coordinates are irrelevant to
    /// the conversion factors, which read only those two fields.
    fn anchor(unit: AnchorUnit, scale: f32) -> TrayAnchor {
        TrayAnchor {
            cursor: (0, 0),
            work_area: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1040,
            },
            scale,
            unit,
        }
    }

    fn approx(got: f32, want: f32) {
        assert!((got - want).abs() < 1e-6, "expected ~{want}, got {got}");
    }

    #[test]
    fn physical_pixel_anchors_scale_the_logical_size_and_pass_the_position_through() {
        // Windows: the anchor space *is* physical pixels, so a logical window
        // size must grow into it and the resulting position needs no further
        // conversion. Swapping the two factors would shrink the window's
        // clamp box to logical size and then blow the position up by the scale —
        // the flyout would land far off-screen on a 200 % display.
        let a = anchor(AnchorUnit::PhysicalPixels, 2.0);
        approx(a.logical_to_anchor(), 2.0);
        approx(a.anchor_to_physical(), 1.0);
    }

    #[test]
    fn point_anchors_leave_the_logical_size_alone_and_scale_the_position() {
        // macOS: points are already logical, so the size passes through and only
        // the final hand-off to `slint::PhysicalPosition` multiplies. This is the
        // half that the pre-contract `logical × scale` got wrong.
        let a = anchor(AnchorUnit::Points, 2.0);
        approx(a.logical_to_anchor(), 1.0);
        approx(a.anchor_to_physical(), 2.0);
    }

    #[test]
    fn the_two_factors_always_multiply_to_the_sane_scale() {
        // The invariant: logical -> winit-physical is `x scale` on every
        // platform; the unit only decides *where* that multiplication happens.
        // A backend that scales in both halves (or neither) fails here.
        for scale in [1.0f32, 1.25, 1.5, 2.0, 3.0, 0.0, -4.0, f32::NAN] {
            for unit in [AnchorUnit::PhysicalPixels, AnchorUnit::Points] {
                let a = anchor(unit, scale);
                approx(
                    a.logical_to_anchor() * a.anchor_to_physical(),
                    sane_scale(scale),
                );
            }
        }
    }

    #[test]
    fn a_degenerate_scale_is_neutralised_in_both_factors() {
        // Both factors route through `sane_scale`, so neither can hand a layout a
        // NaN or a zero — which would collapse the window or park it at the
        // origin rather than merely mis-place it.
        for scale in [f32::NAN, f32::INFINITY, 0.0, -2.0] {
            for unit in [AnchorUnit::PhysicalPixels, AnchorUnit::Points] {
                let a = anchor(unit, scale);
                approx(a.logical_to_anchor(), 1.0);
                approx(a.anchor_to_physical(), 1.0);
            }
        }
    }

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
    /// Windows-only, and deliberately so, but not for the reason the earlier
    /// version of this comment gave — the three lanes differ from each other, not
    /// just from Windows:
    ///
    /// - on **Linux** it would pass without testing anything. A CI runner has
    ///   neither `DISPLAY` nor `WAYLAND_DISPLAY`, so the backend takes its
    ///   documented fallback, and every assertion below holds against a fallback
    ///   by construction — a green result would mean the environment was empty,
    ///   not that the X11 path works. (Until wave 4b-5 the reason was the
    ///   opposite: the Linux arm was a placeholder that tripped a `debug_assert`,
    ///   so calling this failed the lane by design.)
    /// - on **macOS** it would not fail loudly at all. The libtest harness runs
    ///   test bodies on worker threads, where `MainThreadMarker::new()` is `None`,
    ///   so this would silently exercise the fallback — and then the
    ///   `AnchorUnit::PhysicalPixels` assertion below would fail against the
    ///   fallback's `Points`, for a reason that has nothing to do with the backend
    ///   being correct. Testing the live macOS path needs a `harness = false`
    ///   binary on the real main thread (the shape `duja-dimmer`'s `macos_live`
    ///   test uses) and a window server to talk to.
    ///
    /// The Linux equivalent needs the same shape and an X server to talk to, which
    /// is why `docs/qa-checklist.md` carries it as a human check rather than this
    /// file carrying it as a green test.
    #[cfg(windows)]
    #[test]
    fn the_real_backend_returns_a_usable_anchor() {
        let live = cursor_anchor();
        assert!(
            live.scale.is_finite() && live.scale >= 0.1,
            "both conversion factors are derived from this"
        );
        assert!(
            live.work_area.w > 0 && live.work_area.h > 0,
            "placement clamps into this"
        );
        assert_eq!(
            live.unit,
            AnchorUnit::PhysicalPixels,
            "a Per-Monitor-V2 process gets physical coordinates from Win32"
        );
        approx(live.anchor_to_physical(), 1.0);
        approx(live.logical_to_anchor(), live.scale);
    }
}

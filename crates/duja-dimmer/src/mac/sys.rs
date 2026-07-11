//! `AppKit` and Core Graphics FFI for the overlay windows.
//!
//! Every `unsafe` here is confined behind a safe wrapper and carries a
//! `// SAFETY:` note; the orchestration in [`super`] stays `unsafe`-free. All of
//! these functions **must** run on the main thread — the caller proves it by
//! holding a [`MainThreadMarker`] (for window creation) or by only ever touching
//! an `NSWindow` obtained on the main thread (Cocoa's `NSWindow` is
//! `MainThreadOnly`, so a reference to one cannot have crossed a thread).
//!
//! The overlay recipe (ADR-0003, macOS edition): a borderless, non-opaque
//! `NSWindow` with an opaque-black background and a window-wide `alphaValue`
//! (the flicker-free `SetLayeredWindowAttributes(LWA_ALPHA)` analogue),
//! `ignoresMouseEvents = true` (the security invariant — overlays never
//! intercept input), the shielding window level (covers the Dock, menu bar and
//! ordinary/fullscreen windows), an all-spaces/stationary/fullscreen-auxiliary
//! collection behaviour, and `sharingType = .none`.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSWindow, NSWindowCollectionBehavior, NSWindowLevel,
    NSWindowSharingType, NSWindowStyleMask,
};
use objc2_core_foundation::CGRect;
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID, CGShieldingWindowLevel};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use duja_core::dimmer::DisplayBounds;

use crate::mac_geom::{alpha_value, cocoa_overlay_frame};

/// A translucent-black `NSColor` carrying the overlay's opacity in its alpha
/// component (white `0.0` = black). Setting this as the window's background —
/// with `isOpaque = false` — is the flicker-free opacity mechanism (macOS has no
/// direct `LWA_ALPHA` equivalent that both reaches true black and updates
/// cleanly; the window-wide `alphaValue` is not exposed here, so the fill's own
/// alpha carries it).
fn black_with_alpha(alpha: u8) -> Retained<NSColor> {
    NSColor::colorWithCalibratedWhite_alpha(0.0, alpha_value(alpha))
}

/// Create one click-through black overlay covering `bounds` at `alpha`
/// (`1..=255`), raised to the shielding level and excluded from capture on the
/// OS versions that still honour it.
///
/// The window is `releasedWhenClosed(false)`, so the returned [`Retained`] is
/// its sole owner: dropping it (after [`destroy_overlay`]) deallocates it.
pub(super) fn create_overlay(
    mtm: MainThreadMarker,
    bounds: DisplayBounds,
    alpha: u8,
    primary_height: f64,
) -> Retained<NSWindow> {
    let frame = cocoa_rect(bounds, primary_height);

    // SAFETY: the designated `NSWindow` initializer; `mtm` proves the main
    // thread, `alloc` yields a fresh uninitialised window, the borderless style
    // needs no content view, and `defer = false` builds the backing store now.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    // We hold the sole strong reference; never let AppKit free it on close.
    // SAFETY: sound precisely because the returned `Retained` is the only owner
    // and controls deallocation — turning off release-when-closed prevents a
    // double free when the window is ordered out and the `Retained` later drops.
    unsafe { window.setReleasedWhenClosed(false) };

    window.setOpaque(false);
    window.setHasShadow(false);
    // The fill's alpha component carries the overlay opacity.
    window.setBackgroundColor(Some(&black_with_alpha(alpha)));
    // THE security invariant: the overlay must never intercept input.
    window.setIgnoresMouseEvents(true);
    window.setLevel(shielding_level());
    window.setCollectionBehavior(overlay_collection_behavior());
    // Best-effort capture exclusion (honoured through macOS 14; see module docs
    // in `super` for the macOS 15+ known-limit).
    window.setSharingType(NSWindowSharingType::None);
    window.orderFrontRegardless();

    window
}

/// Move/resize an existing overlay to `bounds` and re-assert its z-order.
pub(super) fn move_overlay(window: &NSWindow, bounds: DisplayBounds, primary_height: f64) {
    window.setFrame_display(cocoa_rect(bounds, primary_height), true);
    window.orderFrontRegardless();
}

/// Set an existing overlay's opacity from a quantized alpha byte, by swapping in
/// a new translucent-black background fill.
pub(super) fn set_alpha(window: &NSWindow, alpha: u8) {
    window.setBackgroundColor(Some(&black_with_alpha(alpha)));
}

/// Remove an overlay from the screen. The caller then drops the [`Retained`],
/// which — with `releasedWhenClosed(false)` — deallocates the window.
pub(super) fn destroy_overlay(window: &NSWindow) {
    window.orderOut(None);
}

/// The height in points of the primary display, the reference edge for the
/// Core Graphics → Cocoa vertical flip.
pub(super) fn primary_display_height_points() -> f64 {
    // Both are pure scalar queries (objc2 marks them safe: no pointer arguments).
    let bounds: CGRect = CGDisplayBounds(CGMainDisplayID());
    bounds.size.height
}

/// Build an `NSRect` (Cocoa points, bottom-left origin) from CG-global bounds.
fn cocoa_rect(bounds: DisplayBounds, primary_height: f64) -> NSRect {
    let f = cocoa_overlay_frame(bounds, primary_height);
    NSRect::new(NSPoint::new(f.x, f.y), NSSize::new(f.width, f.height))
}

/// The window level that covers the Dock, menu bar, and ordinary and fullscreen
/// windows. It cannot cover the OS's own secure surfaces (login window, lock
/// screen, fast-user-switch, secure text entry) or the hardware cursor — those
/// stay undimmed, the documented known-limits.
fn shielding_level() -> NSWindowLevel {
    // Pure query (objc2 marks it safe): no arguments, no pointers, no mutation.
    let level = CGShieldingWindowLevel();
    level as NSWindowLevel
}

/// Join every space, stay put during space switches, and float over other apps'
/// fullscreen spaces.
fn overlay_collection_behavior() -> NSWindowCollectionBehavior {
    NSWindowCollectionBehavior::CanJoinAllSpaces
        | NSWindowCollectionBehavior::Stationary
        | NSWindowCollectionBehavior::FullScreenAuxiliary
}

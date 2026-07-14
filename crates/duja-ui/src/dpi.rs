//! A defensive fractional-DPI buffer re-assert shared by both window shells,
//! plus the single winit event hook each window installs.
//!
//! On this project's target (a fixed-size flyout / settings window) Slint sizes
//! the window correctly for the monitor's scale natively — the earlier "buffer
//! stuck at design-px" symptom turned out to be a DPI-*unaware* measurement
//! artifact (an unaware `GetClientRect` virtualises the rect by `1/scale`), and
//! the PR #28 "compensated layout" that chased it was what produced the real
//! dead space; removing that compensation is the actual fix.
//!
//! What remains here is small and defensive: when a window *does* move to a
//! monitor with a different scale, winit delivers `ScaleFactorChanged` with the
//! real factor, and we re-assert the physical inner size so the buffer tracks
//! it (the standard winit remedy). The same single hook — only one is allowed
//! per window — also forwards focus-loss to the flyout's click-outside
//! dismissal. A best-effort re-assert (`enforce_physical_buffer`) also runs when
//! the flyout's content is resized *while it is already visible* — a hot-plug
//! changing the row count — which is now the only path that calls it; the show
//! path deliberately does not (a show-time resize aged the software renderer into
//! a partial first frame — see the flyout shell's one-shot present).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use i_slint_backend_winit::winit::dpi::PhysicalSize;
use i_slint_backend_winit::winit::event::WindowEvent;
use i_slint_backend_winit::winit::window::Window as WinitWindow;
use i_slint_backend_winit::{EventResult, WinitWindowAccessor};

/// The design logical `(width, height)` a window's buffer should track, shared
/// between the show-time enforce and the scale-change hook.
pub(crate) type DesiredSize = Rc<Cell<(f32, f32)>>;

/// An optional focus-loss callback the flyout installs for click-outside
/// dismissal (settings does not use it).
pub(crate) type FocusLostCb = Rc<RefCell<Option<Box<dyn FnMut()>>>>;

/// Install the single winit event hook for `window`: it keeps the physical
/// buffer at `desired × scale` across `ScaleFactorChanged`, and forwards a
/// focus-loss to `focus_lost` when one is set. A no-op off the winit backend.
///
/// When `track_resize` is set (the user-resizable settings window), a `Resized`
/// also records the new **logical** size into `desired`, so a later scale change
/// re-asserts the size the *user* dragged to rather than the initial seed. The
/// fixed-size flyout passes `false` (its size is app-driven; recording its own
/// re-asserts would only invite rounding drift).
///
/// Only one winit event hook can be registered per window, so both concerns
/// share this one.
pub(crate) fn install_window_hook(
    window: &slint::Window,
    desired: DesiredSize,
    focus_lost: FocusLostCb,
    track_resize: bool,
) {
    window.on_winit_window_event(move |slint_window, event| {
        match event {
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // RATIONALE (cast_possible_truncation, cast_precision_loss): a
                // display scale is tiny and exactly representable in f32.
                #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                let scale = *scale_factor as f32;
                let (lw, lh) = desired.get();
                slint_window.with_winit_window(|w| size_to(w, lw, lh, scale));
            }
            WindowEvent::Resized(size) if track_resize => {
                // Record the user's new size as logical px (physical / scale) so
                // the scale-change arm above re-asserts it. Query the **monitor**
                // scale (as `enforce_physical_buffer` does), never the window's own
                // cached scale: right after `show()` the latter can still read the
                // provisional 1.0, so a show-time programmatic `Resized` would be
                // recorded in the wrong units and mis-size the window on the next
                // scale change. We only *record* here — never `request_inner_size`
                // — so this cannot loop with the resize.
                let scale = slint_window
                    .with_winit_window(|w| {
                        w.current_monitor()
                            .map_or_else(|| w.scale_factor(), |m| m.scale_factor())
                    })
                    .unwrap_or(1.0);
                let scale = if scale.is_finite() && scale >= 0.1 {
                    scale
                } else {
                    1.0
                };
                // RATIONALE (cast_possible_truncation, cast_precision_loss): a
                // display scale is tiny and exactly representable in f32; physical
                // window dimensions are far inside f32's exact-integer range.
                #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                let (scale, pw, ph) = (scale as f32, size.width as f32, size.height as f32);
                if pw >= 1.0 && ph >= 1.0 {
                    desired.set((pw / scale, ph / scale));
                }
            }
            WindowEvent::Focused(false) => {
                if let Some(cb) = focus_lost.borrow_mut().as_mut() {
                    cb();
                }
            }
            _ => {}
        }
        // Let Slint keep processing the event normally.
        EventResult::Propagate
    });
}

/// Best-effort physical-buffer re-assert for a window whose logical content size
/// changed **while it is already visible** (e.g. a hot-plug growing/shrinking the
/// flyout's row count), using the monitor's OS-queried scale. It is deliberately
/// *not* called on the show path — a show-time resize aged the software renderer
/// into a partial first frame (see the flyout shell's one-shot present). The
/// `ScaleFactorChanged` hook remains the authoritative cure for scale moves.
pub(crate) fn enforce_physical_buffer(
    window: &slint::Window,
    logical_width: f32,
    logical_height: f32,
) {
    window.with_winit_window(|w| {
        // The **monitor** handle's scale factor is OS-queried on demand, so it is
        // current even right after `show()` when the window's own cached scale can
        // still read the provisional 1.0.
        // RATIONALE (cast_possible_truncation, cast_precision_loss): a display
        // scale is tiny and exactly representable in f32.
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let scale = w
            .current_monitor()
            .map_or_else(|| w.scale_factor(), |m| m.scale_factor()) as f32;
        size_to(w, logical_width, logical_height, scale);
    });
}

/// Request `logical × scale` physical pixels for the window's inner buffer.
fn size_to(w: &WinitWindow, logical_width: f32, logical_height: f32, scale: f32) {
    let size = PhysicalSize::new(
        physical(logical_width, scale),
        physical(logical_height, scale),
    );
    let _ = w.request_inner_size(size);
}

/// Convert a logical extent to a physical pixel count at `scale`, clamped to at
/// least one pixel and guarded against a degenerate scale.
fn physical(logical: f32, scale: f32) -> u32 {
    let scale = if scale.is_finite() && scale >= 0.1 {
        scale
    } else {
        1.0
    };
    let scaled = (logical.max(1.0) * scale).round();
    // RATIONALE (cast_possible_truncation, cast_sign_loss): `scaled` is finite,
    // >= 1.0, and a rounded pixel count far inside u32; the guards above rule out
    // negatives, NaN and infinities.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = scaled as u32;
    out.max(1)
}

#[cfg(test)]
mod tests {
    use super::physical;

    #[test]
    fn scales_logical_to_physical() {
        assert_eq!(physical(320.0, 1.25), 400);
        assert_eq!(physical(440.0, 1.25), 550);
        assert_eq!(physical(320.0, 1.0), 320);
        assert_eq!(physical(200.0, 2.0), 400);
    }

    #[test]
    fn guards_degenerate_scale_and_extent() {
        assert_eq!(physical(320.0, f32::NAN), 320);
        assert_eq!(physical(320.0, 0.0), 320);
        assert_eq!(physical(0.0, 1.25), 1);
    }
}

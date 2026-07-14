//! Pure geometry for placing the flyout near the tray.
//!
//! The tray lives in a corner of the work area (the screen minus the taskbar).
//! Duja anchors the flyout to the click point but keeps it fully inside the work
//! area with a small margin, and sits it against the work-area edge the taskbar
//! occupies — inferred from the click, since the tray (and cursor) sit in the
//! taskbar corner. All four taskbar placements are supported: a horizontal
//! taskbar (bottom or top) pins the flyout to that edge and centres it
//! horizontally on the cursor; a vertical taskbar (left or right) pins it to that
//! edge and centres it vertically. All arithmetic is injected — the caller
//! supplies the cursor, work area, and flyout size — so the placement is
//! exhaustively unit-testable without any Win32 call.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

/// A rectangle in virtual-desktop pixels (origin may be negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    /// Left edge.
    pub(crate) x: i32,
    /// Top edge.
    pub(crate) y: i32,
    /// Width (pixels).
    pub(crate) w: u32,
    /// Height (pixels).
    pub(crate) h: u32,
}

impl Rect {
    /// The right edge (`x + w`), saturating.
    fn right(self) -> i32 {
        self.x
            .saturating_add(i32::try_from(self.w).unwrap_or(i32::MAX))
    }

    /// The bottom edge (`y + h`), saturating.
    fn bottom(self) -> i32 {
        self.y
            .saturating_add(i32::try_from(self.h).unwrap_or(i32::MAX))
    }
}

/// Which work-area edge the taskbar (and therefore the tray) sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// Horizontal taskbar along the bottom (the common layout).
    Bottom,
    /// Horizontal taskbar along the top.
    Top,
    /// Vertical taskbar along the left.
    Left,
    /// Vertical taskbar along the right.
    Right,
}

/// Infer the taskbar edge from the click point within the work area.
///
/// The tray sits in the corner against the taskbar, so a tray click lands in (or
/// hard against) that edge — it is the work-area edge the cursor is nearest, or
/// beyond (a click inside the taskbar is *outside* the work area on that side, so
/// its signed distance to that edge is the smallest). Ties resolve
/// `Bottom > Top > Right > Left`, favouring the commonest layouts.
fn taskbar_edge(cursor: (i32, i32), work: Rect) -> Edge {
    let d_left = cursor.0.saturating_sub(work.x);
    let d_right = work.right().saturating_sub(cursor.0);
    let d_top = cursor.1.saturating_sub(work.y);
    let d_bottom = work.bottom().saturating_sub(cursor.1);

    let mut edge = Edge::Bottom;
    let mut best = d_bottom;
    for (distance, candidate) in [
        (d_top, Edge::Top),
        (d_right, Edge::Right),
        (d_left, Edge::Left),
    ] {
        if distance < best {
            best = distance;
            edge = candidate;
        }
    }
    edge
}

/// Compute the flyout's top-left corner (physical pixels).
///
/// `cursor` is the click point (typically the tray click / cursor position);
/// `work_area` is the monitor work area under it; `flyout` is the flyout's
/// `(width, height)`; `margin` is the gap kept from every work-area edge.
///
/// The flyout is pinned to the work-area edge the taskbar occupies (inferred by
/// [`taskbar_edge`]) and slid along that edge to follow the cursor — centred
/// horizontally for a bottom/top taskbar, vertically for a left/right one — then
/// clamped so it never crosses a work-area edge. When the work area is smaller
/// than the flyout plus margins, the flyout is aligned to the top-left corner
/// rather than pushed off-screen.
pub(crate) fn flyout_origin(
    cursor: (i32, i32),
    work_area: Rect,
    flyout: (u32, u32),
    margin: i32,
) -> (i32, i32) {
    let fw = i32::try_from(flyout.0).unwrap_or(i32::MAX);
    let fh = i32::try_from(flyout.1).unwrap_or(i32::MAX);

    let min_x = work_area.x.saturating_add(margin);
    let max_x = work_area.right().saturating_sub(fw).saturating_sub(margin);
    let min_y = work_area.y.saturating_add(margin);
    let max_y = work_area.bottom().saturating_sub(fh).saturating_sub(margin);

    // Follow the cursor along the free axis (centred on it), pinned on the other.
    let along_x = clamp(cursor.0.saturating_sub(fw / 2), min_x, max_x);
    let along_y = clamp(cursor.1.saturating_sub(fh / 2), min_y, max_y);

    match taskbar_edge(cursor, work_area) {
        Edge::Bottom => (along_x, clamp(max_y, min_y, max_y)),
        Edge::Top => (along_x, clamp(min_y, min_y, max_y)),
        Edge::Left => (clamp(min_x, min_x, max_x), along_y),
        Edge::Right => (clamp(max_x, min_x, max_x), along_y),
    }
}

/// Clamp `value` into `[lo, hi]`, tolerating an inverted range (`hi < lo`, which
/// happens when the flyout is larger than the work area) by returning `lo`.
fn clamp(value: i32, lo: i32, hi: i32) -> i32 {
    if hi < lo { lo } else { value.clamp(lo, hi) }
}

/// Convert a *logical* `(width, height)` in `f32` design units to a **physical**
/// pixel size at `scale`, rounding and guarding a degenerate scale/extent.
///
/// The Slint window is laid out in logical pixels (`320px`) but the anchor math
/// and `set_position` work in physical pixels (Win32 rects are physical on a
/// Per-Monitor-V2 process). At a scale ≠ 1.0 the two differ, so the caller
/// converts the window's logical design size to physical before clamping —
/// otherwise the anchor keeps a *logical*-sized box on-screen while the real,
/// larger physical window overflows the work-area edge (P0 live-QA bug 4). The
/// height is content-driven (`f32`), so this takes `f32` inputs. A
/// non-finite/degenerate scale falls back to the unscaled dimension.
pub(crate) fn physical_window_size(logical_w: f32, logical_h: f32, scale: f32) -> (u32, u32) {
    (
        physical_dim(logical_w, scale),
        physical_dim(logical_h, scale),
    )
}

/// Scale one logical `f32` dimension to a physical pixel count (see
/// [`physical_window_size`]), clamped to at least one pixel.
fn physical_dim(logical: f32, scale: f32) -> u32 {
    let scale = if scale.is_finite() && scale >= 0.1 {
        scale
    } else {
        1.0
    };
    let scaled = (logical.max(1.0) * scale).round();
    // RATIONALE (cast_possible_truncation, cast_sign_loss): `scaled` is finite,
    // >= 1.0, and a rounded pixel count well within u32; the guards above rule out
    // negatives, NaN and infinities.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = scaled as u32;
    out.max(1)
}

/// The top-left corner (physical pixels) that centres a window of `size` within
/// `work`, clamped so an oversized window pins to the work-area origin.
///
/// Used to place the settings window deliberately on the active monitor rather
/// than letting the OS drop it at a default cascade spot (P0 live-QA bug 4).
pub(crate) fn center_in(work: Rect, size: (u32, u32)) -> (i32, i32) {
    let sw = i32::try_from(size.0).unwrap_or(i32::MAX);
    let sh = i32::try_from(size.1).unwrap_or(i32::MAX);
    let free_w = i32::try_from(work.w).unwrap_or(i32::MAX).saturating_sub(sw);
    let free_h = i32::try_from(work.h).unwrap_or(i32::MAX).saturating_sub(sh);
    let x = work.x.saturating_add((free_w / 2).max(0));
    let y = work.y.saturating_add((free_h / 2).max(0));
    (x, y)
}

/// The largest logical flyout height that fits inside `work` (physical px) at
/// `scale`, leaving `margin` physical px clear top and bottom, and never
/// exceeding `absolute_cap` (logical px). The flyout scrolls its rows beyond
/// this, so a small screen never overflows.
///
/// A degenerate scale (non-finite or `< 0.1`) is treated as `1.0`, matching
/// [`physical_dim`].
pub(crate) fn flyout_height_cap(work: Rect, scale: f32, margin: i32, absolute_cap: f32) -> f32 {
    let scale = if scale.is_finite() && scale >= 0.1 {
        scale
    } else {
        1.0
    };
    let margin_px = u32::try_from(margin.max(0)).unwrap_or(0);
    let usable_physical = work.h.saturating_sub(margin_px.saturating_mul(2));
    // RATIONALE (cast_precision_loss): a work-area height in physical pixels is
    // far below f32's 2^24 exact-integer limit, so this u32 -> f32 is exact.
    #[allow(clippy::cast_precision_loss)]
    let usable_logical = usable_physical as f32 / scale;
    absolute_cap.min(usable_logical)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect {
        x: 0,
        y: 0,
        w: 1920,
        h: 1040, // 1080 minus a 40px bottom taskbar
    };
    const FLYOUT: (u32, u32) = (320, 420);
    const MARGIN: i32 = 12;

    #[test]
    fn sits_against_the_bottom_of_the_work_area() {
        let (_x, y) = flyout_origin((1800, 1030), WORK, FLYOUT, MARGIN);
        // bottom (1040) - height (420) - margin (12) = 608
        assert_eq!(y, 608);
    }

    #[test]
    fn centres_horizontally_on_the_cursor_when_room() {
        let (x, _y) = flyout_origin((900, 1030), WORK, FLYOUT, MARGIN);
        // 900 - 320/2 = 740
        assert_eq!(x, 740);
    }

    #[test]
    fn clamps_to_the_right_edge_near_the_tray() {
        let (x, _y) = flyout_origin((1915, 1035), WORK, FLYOUT, MARGIN);
        // right (1920) - width (320) - margin (12) = 1588
        assert_eq!(x, 1588);
    }

    #[test]
    fn clamps_to_the_left_edge() {
        let (x, _y) = flyout_origin((0, 1035), WORK, FLYOUT, MARGIN);
        assert_eq!(x, WORK.x + MARGIN);
    }

    fn approx(got: f32, want: f32) {
        assert!((got - want).abs() < 0.01, "expected ~{want}, got {got}");
    }

    #[test]
    fn height_cap_is_the_absolute_cap_on_a_tall_screen() {
        // 1040 - 24 = 1016 usable logical at scale 1.0; the 620 cap wins.
        approx(flyout_height_cap(WORK, 1.0, MARGIN, 620.0), 620.0);
    }

    #[test]
    fn height_cap_shrinks_to_fit_a_short_screen() {
        let short = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 500,
        };
        // 500 - 2*12 = 476 usable logical, below the 620 cap.
        approx(flyout_height_cap(short, 1.0, MARGIN, 620.0), 476.0);
    }

    #[test]
    fn height_cap_accounts_for_dpi_scale() {
        let hi_dpi = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1000,
        };
        // (1000 - 24) / 2.0 = 488 logical.
        approx(flyout_height_cap(hi_dpi, 2.0, MARGIN, 620.0), 488.0);
    }

    #[test]
    fn height_cap_treats_a_degenerate_scale_as_one() {
        let short = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 500,
        };
        approx(flyout_height_cap(short, 0.0, MARGIN, 620.0), 476.0);
    }

    #[test]
    fn honours_a_negative_origin_work_area() {
        // A monitor left of the primary sits at negative virtual-desktop x.
        let work = Rect {
            x: -1920,
            y: 0,
            w: 1920,
            h: 1040,
        };
        let (x, _y) = flyout_origin((-10, 1035), work, FLYOUT, MARGIN);
        // right (0) - 320 - 12 = -332
        assert_eq!(x, -332);
    }

    #[test]
    fn oversized_flyout_pins_to_the_corner() {
        let tiny = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let (x, y) = flyout_origin((50, 50), tiny, FLYOUT, MARGIN);
        assert_eq!((x, y), (MARGIN, MARGIN));
    }

    // --- taskbar-edge inference across all four layouts -------------------

    #[test]
    fn infers_each_taskbar_edge_from_the_click() {
        // A click hard against (or inside) an edge is nearest that edge.
        assert_eq!(taskbar_edge((900, 1040), WORK), Edge::Bottom);
        assert_eq!(taskbar_edge((900, 0), WORK), Edge::Top);
        assert_eq!(taskbar_edge((0, 500), WORK), Edge::Left);
        assert_eq!(taskbar_edge((1920, 500), WORK), Edge::Right);
    }

    #[test]
    fn top_taskbar_pins_to_the_top_and_follows_x() {
        // A top taskbar: tray top-right, click near the top edge.
        let (x, y) = flyout_origin((1800, 0), WORK, FLYOUT, MARGIN);
        assert_eq!(y, WORK.y + MARGIN, "flyout hangs from the top edge");
        // Still centred horizontally on the cursor, clamped to the right:
        // right (1920) - width (320) - margin (12) = 1588.
        assert_eq!(x, 1588);
    }

    #[test]
    fn left_taskbar_pins_to_the_left_and_follows_y() {
        // A left taskbar with the tray at its bottom: click near the left edge,
        // low down. The flyout sits against the left edge and is clamped so it
        // stays inside the work area (bottom = 1040 - 420 - 12 = 608).
        let (x, y) = flyout_origin((0, 900), WORK, FLYOUT, MARGIN);
        assert_eq!(x, WORK.x + MARGIN, "flyout sits against the left edge");
        assert_eq!(y, 608, "clamped to the bottom of the work area");
    }

    #[test]
    fn left_taskbar_centres_vertically_when_there_is_room() {
        // A mid-height click leaves room to centre vertically on the cursor:
        // 400 - height (420) / 2 = 190.
        let (_x, y) = flyout_origin((0, 400), WORK, FLYOUT, MARGIN);
        assert_eq!(y, 190);
    }

    #[test]
    fn right_taskbar_pins_to_the_right_and_follows_y() {
        // A right taskbar: the work area is inset from the monitor's right, and
        // the click lands hard against that inset edge.
        let work = Rect {
            x: 0,
            y: 0,
            w: 1880, // 1920 minus a 40px right taskbar
            h: 1080,
        };
        let (x, y) = flyout_origin((1878, 500), work, FLYOUT, MARGIN);
        // right (1880) - width (320) - margin (12) = 1548.
        assert_eq!(x, 1548, "flyout sits against the right edge");
        // Centred vertically on the cursor (500 - 210 = 290).
        assert_eq!(y, 290);
    }

    #[test]
    fn bottom_taskbar_is_still_the_tie_break_default() {
        // A bottom-right corner click (equidistant to bottom and right) resolves
        // to the common bottom-taskbar layout.
        assert_eq!(taskbar_edge((1915, 1035), WORK), Edge::Bottom);
    }

    // --- DPI scaling (P0 live-QA bug 4) ----------------------------------

    #[test]
    fn physical_window_size_scales_f32_logical_dims() {
        // 320x250 logical at 125% → 400x313 (250 * 1.25 = 312.5 → 313).
        assert_eq!(physical_window_size(320.0, 250.0, 1.25), (400, 313));
        // Integer scale is identity.
        assert_eq!(physical_window_size(440.0, 600.0, 1.0), (440, 600));
        // A degenerate scale falls back to the unscaled (rounded) logical size.
        assert_eq!(physical_window_size(320.0, 250.0, f32::NAN), (320, 250));
        assert_eq!(physical_window_size(320.0, 250.0, 0.0), (320, 250));
        // Extents clamp to at least one pixel.
        assert_eq!(physical_window_size(0.0, 0.0, 1.25), (1, 1));
    }

    #[test]
    fn physical_window_stays_on_screen_at_125_percent() {
        // The exact live-QA geometry: a 2560x1440 monitor at 125 % with a 60 px
        // physical bottom taskbar; the tray click lands in the bottom-right.
        let work = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1380,
        };
        let logical = (320u32, 200u32);
        let scale = 1.25;
        let phys = physical_window_size(320.0, 200.0, scale);
        assert_eq!(phys, (400, 250));
        let phys_w = i32::try_from(phys.0).unwrap();
        let phys_h = i32::try_from(phys.1).unwrap();
        let cursor = (2545, 1432);

        // Pre-fix: the anchor was computed from the *logical* size, so the clamp
        // kept only a 320-wide box inside the work area — the real 400-wide
        // window then overran the right edge.
        let bug = flyout_origin(cursor, work, logical, 15);
        assert!(
            bug.0 + phys_w > work.right(),
            "reproduces the off-screen overflow: {} + {} > {}",
            bug.0,
            phys_w,
            work.right()
        );

        // Fixed: anchoring with the physical size keeps the real window fully on
        // screen and pinned against the taskbar edge.
        let fixed = flyout_origin(cursor, work, phys, 15);
        assert!(fixed.0 + phys_w <= work.right(), "right edge on-screen");
        assert!(
            fixed.1 + phys_h <= work.bottom(),
            "bottom edge above the taskbar"
        );
        assert!(fixed.0 >= work.x, "left edge on-screen");
        assert!(fixed.1 >= work.y, "top edge on-screen");
    }

    #[test]
    fn center_in_centres_and_pins_oversized_windows() {
        let work = Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1380,
        };
        // 420x560 logical settings window at 125 % → 525x700 physical.
        let size = physical_window_size(420.0, 560.0, 1.25);
        assert_eq!(size, (525, 700));
        let (x, y) = center_in(work, size);
        // Centred: (2560-525)/2 = 1017, (1380-700)/2 = 340.
        assert_eq!((x, y), (1017, 340));

        // An oversized window pins to the work-area origin rather than going off
        // the top-left.
        let (x, y) = center_in(work, (4000, 4000));
        assert_eq!((x, y), (0, 0));
    }

    #[test]
    fn center_in_honours_a_negative_origin_monitor() {
        let work = Rect {
            x: -2560,
            y: 0,
            w: 2560,
            h: 1380,
        };
        let (x, _y) = center_in(work, (500, 500));
        // -2560 + (2560-500)/2 = -2560 + 1030 = -1530.
        assert_eq!(x, -1530);
    }
}

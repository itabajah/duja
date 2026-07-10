//! Pure geometry for placing the flyout near the tray.
//!
//! The tray lives in a corner of the work area (the screen minus the taskbar).
//! Duja anchors the flyout to the click point but keeps it fully inside the work
//! area with a small margin, and sits it against the work-area edge nearest the
//! taskbar (the bottom edge in the common bottom-taskbar layout). All arithmetic
//! is injected — the caller supplies the cursor, work area, and flyout size — so
//! the placement is exhaustively unit-testable without any Win32 call.

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

/// Compute the flyout's top-left corner (physical pixels).
///
/// `cursor` is the click point (typically the tray click / cursor position);
/// `work_area` is the monitor work area under it; `flyout` is the flyout's
/// `(width, height)`; `margin` is the gap kept from every work-area edge.
///
/// The flyout is centred horizontally on the cursor, then clamped so it never
/// crosses a work-area edge, and pinned to the bottom of the work area (just
/// above a bottom taskbar). When the work area is smaller than the flyout plus
/// margins, the flyout is aligned to the top-left corner rather than pushed
/// off-screen.
pub(crate) fn flyout_origin(
    cursor: (i32, i32),
    work_area: Rect,
    flyout: (u32, u32),
    margin: i32,
) -> (i32, i32) {
    let fw = i32::try_from(flyout.0).unwrap_or(i32::MAX);
    let fh = i32::try_from(flyout.1).unwrap_or(i32::MAX);

    // Horizontal: centre on the cursor, then clamp into the work area.
    let centred_x = cursor.0.saturating_sub(fw / 2);
    let min_x = work_area.x.saturating_add(margin);
    let max_x = work_area.right().saturating_sub(fw).saturating_sub(margin);
    let x = clamp(centred_x, min_x, max_x);

    // Vertical: sit against the bottom edge, above the taskbar margin.
    let min_y = work_area.y.saturating_add(margin);
    let max_y = work_area.bottom().saturating_sub(fh).saturating_sub(margin);
    let y = clamp(max_y, min_y, max_y);

    (x, y)
}

/// Clamp `value` into `[lo, hi]`, tolerating an inverted range (`hi < lo`, which
/// happens when the flyout is larger than the work area) by returning `lo`.
fn clamp(value: i32, lo: i32, hi: i32) -> i32 {
    if hi < lo { lo } else { value.clamp(lo, hi) }
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
}

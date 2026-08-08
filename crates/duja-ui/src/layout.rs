//! The flyout window's geometry, which is arithmetic over the `.slint` markup
//! and therefore belongs to the crate that owns the markup.
//!
//! # Why this moved
//!
//! It lived in `duja-app`'s `tray/state.rs`, as three constants each documented
//! "matches `flyout.slint`" plus a private method mirroring the layout. A
//! no-frame window is not auto-sized to its preferred height, so something has
//! to compute it - but the something was in the crate that cannot see the file
//! it is mirroring.
//!
//! That cost a real defect. [`crate::frame_probe`] was written against the
//! `260` default in the markup, believing it to be the size the app presents;
//! the app presents [`flyout_logical_height`], which for three monitors is
//! **397**. The probe measured a window with two of the three cards in it and
//! reported the number as a three-monitor frame. Neither crate was wrong about
//! its own half - the arithmetic was simply in a place the probe could not
//! reach, so the probe re-derived it and got it wrong.
//!
//! One copy, here, next to the markup it describes.

/// The flyout's fixed logical width (matches `flyout.slint`).
pub const FLYOUT_LOGICAL_WIDTH: f32 = 360.0;

/// The flyout's hard maximum logical height. Beyond this the rows scroll rather
/// than the window growing (matches the `clamp(..., 620px)` in `flyout.slint`).
pub const FLYOUT_MAX_LOGICAL_HEIGHT: f32 = 620.0;

/// The flyout's minimum logical height (the empty-state / single-row floor,
/// matching the `clamp(160px, ...)` in `flyout.slint`). The work-area cap is
/// never allowed to shrink the window below this.
pub const FLYOUT_MIN_LOGICAL_HEIGHT: f32 = 160.0;

/// Padding + header + inter-section gap (no footer).
const CHROME: f32 = 78.0;
/// One card: a name+caption row, then a slider+pill row.
const CARD: f32 = 101.0;
/// The gap between two cards.
const CARD_GAP: f32 = 8.0;
/// The empty-state panel, which is one fixed block rather than a card.
const EMPTY_PANEL: f32 = 100.0;

/// The flyout window's content-derived logical height for `rows` monitors.
///
/// Mirrors the `.slint` layout arithmetic - chrome plus one card per row -
/// because a no-frame window is not auto-sized to its preferred height.
/// Approximate by design: a few pixels of slack sit at the bottom.
#[must_use]
pub fn flyout_logical_height(rows: usize) -> f32 {
    let body = if rows == 0 {
        EMPTY_PANEL
    } else {
        // RATIONALE (arithmetic_side_effects): `n` is at most `u16::MAX` and the
        // products are bounded by a few million, far inside f32's exact-integer
        // range; the result is clamped immediately below in any case.
        #[allow(clippy::arithmetic_side_effects)]
        {
            let n = f32::from(u16::try_from(rows).unwrap_or(u16::MAX));
            n * CARD + (n - 1.0) * CARD_GAP
        }
    };
    // RATIONALE (arithmetic_side_effects): a sum of two bounded positive floats.
    #[allow(clippy::arithmetic_side_effects)]
    let total = CHROME + body;
    total.clamp(FLYOUT_MIN_LOGICAL_HEIGHT, FLYOUT_MAX_LOGICAL_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three numbers that matter, and the middle one is the one a probe
    /// written against the markup's `260` default would have missed.
    #[test]
    fn the_height_grows_with_the_row_count() {
        assert!((flyout_logical_height(0) - 178.0).abs() < f32::EPSILON);
        assert!((flyout_logical_height(1) - 179.0).abs() < f32::EPSILON);
        assert!((flyout_logical_height(2) - 288.0).abs() < f32::EPSILON);
        assert!((flyout_logical_height(3) - 397.0).abs() < f32::EPSILON);
    }

    /// `260` is the markup's *default* `content-height`, not a size the app
    /// ever presents. Pinned because believing otherwise is what made the frame
    /// probe measure a two-card flyout and call it three.
    #[test]
    fn no_row_count_produces_the_markup_default_of_260() {
        for rows in 0..64_usize {
            let h = flyout_logical_height(rows);
            assert!(
                (h - 260.0).abs() > f32::EPSILON,
                "{rows} rows produced exactly the markup default 260, which \
                 would make the probe's original size look correct"
            );
        }
    }

    #[test]
    fn a_tall_stack_is_clamped_rather_than_growing_without_bound() {
        assert!((flyout_logical_height(64) - FLYOUT_MAX_LOGICAL_HEIGHT).abs() < f32::EPSILON);
        assert!(
            (flyout_logical_height(usize::MAX) - FLYOUT_MAX_LOGICAL_HEIGHT).abs() < f32::EPSILON
        );
    }

    /// The empty state is below the floor on its own arithmetic (78 + 100 =
    /// 178), so the floor is what a no-display flyout actually gets... except
    /// it is not, because 178 is already above 160. Asserted so a future change
    /// to either constant cannot silently make the floor load-bearing.
    #[test]
    fn the_floor_is_not_currently_reached_by_any_row_count() {
        assert!(flyout_logical_height(0) > FLYOUT_MIN_LOGICAL_HEIGHT);
        assert!(flyout_logical_height(1) > FLYOUT_MIN_LOGICAL_HEIGHT);
    }
}

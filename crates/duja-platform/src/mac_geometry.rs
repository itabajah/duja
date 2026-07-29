//! Pure Cocoa → tray-anchor geometry for the macOS [`geometry`](crate::geometry)
//! backend: the y-flip, the degenerate-value clamps, and the choice of which
//! screen the cursor is on.
//!
//! Not one line here calls `AppKit`. The macOS backend reads
//! `NSEvent::mouseLocation` and each `NSScreen`'s
//! `frame`/`visibleFrame`/`backingScaleFactor`, copies them into the plain `f64`
//! structs below, and hands them to [`anchor_from_screens`] — so every *decision*
//! is unit-tested on **every** CI host (Windows and Linux included), and only the
//! reads themselves need a Mac. That is the same shape as `mac_events`, for the
//! same reason: the FFI is untestable, the arithmetic is where the bugs are.
//!
//! # Coordinate spaces
//!
//! - **Cocoa global space** (the input): bottom-left origin, **y-up**, in points.
//!   Its origin is the bottom-left of the screen carrying the menu bar, so a
//!   screen stacked *above* the primary has a **positive** `origin.y` (at least
//!   the primary's height) and one *below* it has a **negative** `origin.y`. In
//!   anchor space the signs are the other way round, which is precisely what the
//!   flip is for.
//! - **Anchor space** (the output): top-left origin, **y-down**, still in points
//!   — see [`geometry`](crate::geometry)'s module docs for the contract and
//!   `AnchorUnit::Points`.
//!
//! The flip needs a reference height, the primary display's, which is why it is
//! not a local operation:
//!
//! ```text
//! y_down_top = primary_h - (cocoa_bottom + height)
//! y_down     = primary_h -  cocoa_y                 (for a bare point)
//! ```
//!
//! # Agreement with `duja-dimmer`
//!
//! `duja-dimmer`'s `mac_geom::cocoa_overlay_frame` performs the *forward* flip
//! (y-down → Cocoa) with `cocoa_bottom = primary_height - (top + height)`. This
//! module is its exact inverse, and the two **must agree** — they describe one
//! screen layout, and a disagreement would put the flyout and the dimming overlay
//! in different places on the same display. `round_trips_against_the_dimmer_flip`
//! pins that by round-tripping through the dimmer's own formula.
//!
//! The helper is duplicated rather than shared for a layering reason, not a
//! convenience one: `duja-platform` must not depend on `duja-dimmer`
//! (`duja-dimmer` is a sibling backend crate, not a foundation), and hoisting the
//! flip into `duja-core` would put screen-server geometry in the pure brightness
//! kernel. Two small pure functions with a test tying them together is the honest
//! trade.

// Compiled under `cfg(any(test, target_os = "macos"))` (see `lib.rs`), so it is
// either the real macOS build — where `geometry.rs`'s backend calls every item —
// or a test build, where the tests below do. There is deliberately no dead-code
// allow: neither configuration has an unreachable item, and adding one would hide
// a helper that lost its caller.
use crate::geometry::{AnchorUnit, TrayAnchor, WorkRect, sane_scale};

/// The largest extent a converted [`WorkRect`] reports.
///
/// `i32::MAX`, matching the ceiling the Windows backend's saturating
/// `right - left` can produce. Keeping both backends under the same cap means
/// `x + w` stays inside the `i32` space the contract is expressed in, so the
/// placement kernel downstream (which converts extents back to `i32`) never sees
/// a width it has to invent a substitute for.
const MAX_EXTENT: u32 = i32::MAX.unsigned_abs();

/// A point in Cocoa's global screen space: bottom-left origin, y-up, in points.
///
/// A field-for-field copy of `NSPoint`, so the conversion below is `AppKit`-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CocoaPoint {
    /// Distance right of the primary screen's left edge.
    pub(crate) x: f64,
    /// Distance **up** from the primary screen's bottom edge.
    pub(crate) y: f64,
}

/// A rectangle in Cocoa's global screen space: bottom-left origin, y-up, points.
///
/// A field-for-field copy of `NSRect`, flattened (`origin`/`size` merged) because
/// nothing here needs the nesting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CocoaRect {
    /// Left edge.
    pub(crate) x: f64,
    /// **Bottom** edge, measured up from the primary screen's bottom edge.
    pub(crate) y: f64,
    /// Width.
    pub(crate) w: f64,
    /// Height.
    pub(crate) h: f64,
}

impl CocoaRect {
    /// Whether `point` lies inside this rectangle, treating the right and top
    /// edges as exclusive (so adjacent screens do not both claim a shared edge).
    ///
    /// Any `NaN` in either operand makes every comparison false, so a garbage
    /// coordinate is simply "not in this rectangle" rather than a panic or an
    /// arbitrary hit — [`screen_index_for_cursor`] then falls back deterministically.
    fn contains(self, point: CocoaPoint) -> bool {
        point.x >= self.x
            && point.x < self.x + self.w
            && point.y >= self.y
            && point.y < self.y + self.h
    }
}

/// One `NSScreen`'s geometry, as the macOS backend reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CocoaScreen {
    /// The screen's full `frame` — its bounds in the Cocoa global space.
    pub(crate) frame: CocoaRect,
    /// The screen's `visibleFrame`, which is **already the work area**: `AppKit`
    /// has subtracted the menu bar and the Dock from it.
    pub(crate) visible_frame: CocoaRect,
    /// `backingScaleFactor` — backing pixels per point (2.0 on Retina).
    pub(crate) backing_scale: f64,
}

/// Flip a Cocoa point into anchor space (top-left origin, y-down, points).
///
/// `primary_h` is the height in points of the primary screen — the screen with
/// the menu bar, whose bottom-left corner is the Cocoa origin. The x axis is
/// identical in both spaces; only y flips.
fn point_from_cocoa(point: CocoaPoint, primary_h: f64) -> (i32, i32) {
    (coord_to_i32(point.x), coord_to_i32(primary_h - point.y))
}

/// Flip a Cocoa rectangle into an anchor-space [`WorkRect`].
///
/// The y-down **top** edge is `primary_h - (cocoa_bottom + height)`: flipping a
/// rectangle means flipping its *far* edge, because the edge nearest the Cocoa
/// origin (the bottom) is the one furthest from the anchor-space origin (the
/// top). Using `primary_h - cocoa_bottom` instead — the bare-point flip — would
/// place every work area one screen-height too low.
fn work_rect_from_cocoa(rect: CocoaRect, primary_h: f64) -> WorkRect {
    WorkRect {
        x: coord_to_i32(rect.x),
        y: coord_to_i32(primary_h - (rect.y + rect.h)),
        w: extent_to_u32(rect.w),
        h: extent_to_u32(rect.h),
    }
}

/// Round a Cocoa coordinate to the nearest whole point as an `i32`.
///
/// Cannot panic or wrap: a float → integer `as` cast in Rust saturates at the
/// target's bounds and maps `NaN` to `0`, so an absurd `NSScreen` frame degrades
/// to a coordinate at the edge of the space (which placement clamps) rather than
/// wrapping to the opposite side of the desktop.
fn coord_to_i32(value: f64) -> i32 {
    // RATIONALE (cast_possible_truncation): saturating-and-NaN-to-zero is exactly
    // the behaviour documented above and the reason the cast is used unguarded;
    // `.round()` first makes it a nearest-point conversion rather than a trunc.
    #[allow(clippy::cast_possible_truncation)]
    let out = value.round() as i32;
    out
}

/// Round a Cocoa extent to the nearest whole point as a `u32`, clamped to
/// `0..=MAX_EXTENT`.
///
/// A non-finite or negative extent becomes `0` — a degenerate work area, which
/// placement already handles by pinning to the corner. This is the macOS twin of
/// the Windows backend's inverted-`RECT` guard, and an absurd one is pulled down
/// to [`MAX_EXTENT`] rather than reported as `u32::MAX`.
///
/// The explicit early return is belt-and-braces, not the mechanism: Rust's
/// float → integer cast is *already* saturating and maps `NaN` to `0`, so this
/// function returns the same values without it (deleting the guard leaves the
/// tests below green — checked). It is kept because leaning on those cast
/// semantics for a correctness property is how the property gets silently lost in
/// a later rewrite, and the guard states the intent where the reader is.
fn extent_to_u32(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    // RATIONALE (cast_possible_truncation, cast_sign_loss): the guard above rules
    // out NaN, infinities and negatives, and the `as` cast saturates at
    // `u32::MAX`; the explicit `min` then pulls an absurd extent down to the
    // `i32::MAX` ceiling the Windows backend shares.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = value.round() as u32;
    out.min(MAX_EXTENT)
}

/// Narrow a `backingScaleFactor` to the `f32` [`TrayAnchor::scale`] carries,
/// neutralising a degenerate value through the shared [`sane_scale`] guard.
fn scale_from_cocoa(backing_scale: f64) -> f32 {
    // RATIONALE (cast_possible_truncation): a backing scale is a small, exactly
    // representable value (1.0, 2.0, 3.0), so this is lossless in practice; an
    // out-of-range `f64` becomes an infinity, which `sane_scale` turns into 1.0.
    #[allow(clippy::cast_possible_truncation)]
    let scale = backing_scale as f32;
    sane_scale(scale)
}

/// Which screen in `screens` the cursor is on.
///
/// Returns `None` only for an **empty** slice (the caller has no layout to work
/// with and must fall back to its default anchor). Otherwise it is the index of
/// the first screen whose `frame` contains the cursor, and — when the cursor is
/// outside every frame — deterministically `Some(0)`, **the primary screen**.
///
/// That fallback is not hypothetical: `NSEvent::mouseLocation` can report a
/// position in the gap between two non-aligned screens, or just off the edge of
/// one, and macOS does not offer a `MONITOR_DEFAULTTONEAREST` equivalent. Falling
/// back to the primary rather than the nearest screen is the choice that costs
/// nothing to reason about: the flyout lands on the screen with the menu bar,
/// which is where the tray it hangs from lives.
fn screen_index_for_cursor(cursor: CocoaPoint, screens: &[CocoaScreen]) -> Option<usize> {
    if screens.is_empty() {
        return None;
    }
    let hit = screens
        .iter()
        .position(|screen| screen.frame.contains(cursor));
    Some(hit.unwrap_or(0))
}

/// Build a [`TrayAnchor`] from a Cocoa cursor position and the live screen list.
///
/// `screens` must be in `NSScreen::screens` order: index 0 is the screen with the
/// menu bar, whose bottom-left corner is the Cocoa global origin, so **its
/// height is the reference the y-flip uses**. Returns `None` when the list is
/// empty, which is the one case the caller cannot convert its way out of.
///
/// The resulting anchor is `AnchorUnit::Points` — Cocoa points pass through
/// un-scaled, and the `backingScaleFactor` is carried in `scale` for the final
/// hand-off to `slint::PhysicalPosition` (see
/// [`TrayAnchor::anchor_to_physical`]).
pub(crate) fn anchor_from_screens(
    cursor: CocoaPoint,
    screens: &[CocoaScreen],
) -> Option<TrayAnchor> {
    let primary_h = screens.first()?.frame.h;
    let index = screen_index_for_cursor(cursor, screens)?;
    let screen = screens.get(index)?;
    Some(TrayAnchor {
        cursor: point_from_cocoa(cursor, primary_h),
        work_area: work_rect_from_cocoa(screen.visible_frame, primary_h),
        scale: scale_from_cocoa(screen.backing_scale),
        unit: AnchorUnit::Points,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CocoaPoint, CocoaRect, CocoaScreen, MAX_EXTENT, anchor_from_screens, coord_to_i32,
        extent_to_u32, point_from_cocoa, scale_from_cocoa, screen_index_for_cursor,
        work_rect_from_cocoa,
    };
    use crate::geometry::{AnchorUnit, WorkRect};

    /// A 1920x1080 non-Retina screen at the Cocoa origin (the primary).
    fn primary() -> CocoaScreen {
        CocoaScreen {
            frame: CocoaRect {
                x: 0.0,
                y: 0.0,
                w: 1920.0,
                h: 1080.0,
            },
            // 25 pt menu bar off the top (which in Cocoa is the *high* y), so the
            // visible frame's bottom is unchanged and its height is 1055.
            visible_frame: CocoaRect {
                x: 0.0,
                y: 0.0,
                w: 1920.0,
                h: 1055.0,
            },
            backing_scale: 1.0,
        }
    }

    /// A 2560x1440 Retina screen stacked directly **above** the primary: in Cocoa
    /// a screen above the origin has a *positive* `origin.y` (y is up).
    fn above_primary() -> CocoaScreen {
        CocoaScreen {
            frame: CocoaRect {
                x: 0.0,
                y: 1080.0,
                w: 2560.0,
                h: 1440.0,
            },
            visible_frame: CocoaRect {
                x: 0.0,
                y: 1080.0,
                w: 2560.0,
                h: 1440.0,
            },
            backing_scale: 2.0,
        }
    }

    /// A 1280x720 screen stacked directly **below** the primary: negative Cocoa
    /// `origin.y`, its top edge flush with the primary's bottom.
    fn below_primary() -> CocoaScreen {
        CocoaScreen {
            frame: CocoaRect {
                x: 0.0,
                y: -720.0,
                w: 1280.0,
                h: 720.0,
            },
            visible_frame: CocoaRect {
                x: 0.0,
                y: -720.0,
                w: 1280.0,
                h: 720.0,
            },
            backing_scale: 1.0,
        }
    }

    // --- the y flip -------------------------------------------------------

    #[test]
    fn the_primary_screens_visible_frame_flips_to_the_top_of_anchor_space() {
        // Cocoa bottom 0 + height 1055 against a 1080 reference ⇒ y-down top 25:
        // exactly the menu bar's height, which is where a macOS work area starts.
        // Dropping the flip would give 0 (no menu-bar inset); flipping the wrong
        // edge (`primary_h - bottom`) would give 1080, below the whole screen.
        let converted = work_rect_from_cocoa(primary().visible_frame, 1080.0);
        assert_eq!(
            converted,
            WorkRect {
                x: 0,
                y: 25,
                w: 1920,
                h: 1055,
            }
        );
    }

    #[test]
    fn a_screen_above_the_primary_gets_a_negative_y_down_top() {
        // A screen above the primary is at *negative* y in a top-left/y-down
        // space, the same way a monitor above the primary is on Windows.
        // 1080 - (1080 + 1440) = -1440.
        let converted = work_rect_from_cocoa(above_primary().visible_frame, 1080.0);
        assert_eq!(
            converted.y, -1440,
            "above the primary ⇒ negative y-down top"
        );
        assert_eq!(converted.h, 1440);
    }

    #[test]
    fn a_screen_below_the_primary_gets_a_y_down_top_past_the_primarys_height() {
        // 1080 - (-720 + 720) = 1080: its top edge is flush with the primary's
        // bottom. A dropped flip would report -720 — the opposite side of the
        // desktop, which is the bug this test exists to catch.
        let converted = work_rect_from_cocoa(below_primary().visible_frame, 1080.0);
        assert_eq!(converted.y, 1080);
        assert_eq!(converted.h, 720);
    }

    #[test]
    fn a_negative_x_origin_passes_through_unflipped() {
        // A screen to the *left* of the primary has negative x in both spaces —
        // the x axis is shared, so it must not be touched by the flip.
        let rect = CocoaRect {
            x: -2560.0,
            y: 0.0,
            w: 2560.0,
            h: 1440.0,
        };
        let converted = work_rect_from_cocoa(rect, 1080.0);
        assert_eq!(converted.x, -2560);
        assert_eq!(converted.w, 2560);
    }

    #[test]
    fn a_bare_point_flips_without_the_height_term() {
        // A cursor 40 pt above the bottom of a 1080-tall primary is 1040 pt down
        // from the top. Using the rectangle formula on a point (which has no
        // height) would be off by nothing here, but the reverse mistake — using
        // the point formula on a rectangle — is covered above.
        assert_eq!(
            point_from_cocoa(CocoaPoint { x: 1900.0, y: 40.0 }, 1080.0),
            (1900, 1040)
        );
        // The menu-bar corner, where a tray flyout is anchored from: Cocoa y is
        // the full primary height, so y-down is 0.
        assert_eq!(
            point_from_cocoa(
                CocoaPoint {
                    x: 1900.0,
                    y: 1080.0
                },
                1080.0
            ),
            (1900, 0)
        );
    }

    /// The dimmer and this module describe one screen layout with two inverse
    /// flips, so a change to either must keep this round-trip exact — see the
    /// module docs on why the helper is duplicated rather than shared.
    #[test]
    fn round_trips_against_the_dimmer_flip() {
        /// `duja-dimmer`'s `mac_geom::cocoa_overlay_frame` formula, copied
        /// verbatim: y-down top ⇒ Cocoa bottom.
        fn dimmer_cocoa_bottom(y_down_top: f64, height: f64, primary_h: f64) -> f64 {
            primary_h - (y_down_top + height)
        }

        let primary_h = 1080.0;
        for (y_down_top, height) in [
            (25.0, 1055.0),    // the primary's work area
            (-1440.0, 1440.0), // a screen above
            (1080.0, 720.0),   // a screen below
            (0.0, 1080.0),     // the full primary
        ] {
            let cocoa_bottom = dimmer_cocoa_bottom(y_down_top, height, primary_h);
            let back = work_rect_from_cocoa(
                CocoaRect {
                    x: 0.0,
                    y: cocoa_bottom,
                    w: 100.0,
                    h: height,
                },
                primary_h,
            );
            // RATIONALE (cast_possible_truncation): the loop's inputs are small
            // whole numbers chosen by hand, so this is exact.
            #[allow(clippy::cast_possible_truncation)]
            let want = y_down_top as i32;
            assert_eq!(
                back.y, want,
                "the dimmer's forward flip and this inverse must agree"
            );
        }
    }

    // --- degenerate inputs ------------------------------------------------

    #[test]
    fn a_nan_coordinate_becomes_zero_rather_than_garbage() {
        assert_eq!(coord_to_i32(f64::NAN), 0);
        assert_eq!(extent_to_u32(f64::NAN), 0);
        // A NaN reference height poisons the flip's y but leaves x intact, and
        // still cannot panic or wrap.
        let converted = work_rect_from_cocoa(primary().visible_frame, f64::NAN);
        assert_eq!(converted.y, 0);
        assert_eq!(converted.x, 0);
    }

    #[test]
    fn absurd_and_negative_values_saturate_instead_of_wrapping() {
        // A float -> int cast in Rust saturates, so these cannot wrap around.
        assert_eq!(coord_to_i32(1e300), i32::MAX);
        assert_eq!(coord_to_i32(-1e300), i32::MIN);
        assert_eq!(coord_to_i32(f64::INFINITY), i32::MAX);
        // Extents clamp to the shared i32 ceiling, and a negative or zero extent
        // is reported as the degenerate 0 (placement pins to the corner) rather
        // than reinterpreted as a huge positive width.
        assert_eq!(extent_to_u32(1e300), MAX_EXTENT);
        assert_eq!(extent_to_u32(-1.0), 0);
        assert_eq!(extent_to_u32(0.0), 0);
        assert_eq!(extent_to_u32(f64::NEG_INFINITY), 0);
    }

    #[test]
    fn a_degenerate_backing_scale_is_neutralised() {
        // `NSScreen` should never report these, but a detached screen is exactly
        // the situation where "should never" stops holding.
        assert!((scale_from_cocoa(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((scale_from_cocoa(f64::NAN) - 1.0).abs() < f32::EPSILON);
        assert!((scale_from_cocoa(1e300) - 1.0).abs() < f32::EPSILON);
        assert!((scale_from_cocoa(2.0) - 2.0).abs() < f32::EPSILON);
    }

    // --- screen selection -------------------------------------------------

    #[test]
    fn the_cursor_picks_the_screen_whose_frame_contains_it() {
        let screens = [primary(), above_primary(), below_primary()];
        // Inside the primary.
        assert_eq!(
            screen_index_for_cursor(CocoaPoint { x: 10.0, y: 10.0 }, &screens),
            Some(0)
        );
        // Inside the screen above (Cocoa y beyond the primary's height).
        assert_eq!(
            screen_index_for_cursor(
                CocoaPoint {
                    x: 100.0,
                    y: 2000.0
                },
                &screens
            ),
            Some(1)
        );
        // Inside the screen below (negative Cocoa y).
        assert_eq!(
            screen_index_for_cursor(CocoaPoint { x: 100.0, y: -10.0 }, &screens),
            Some(2)
        );
    }

    #[test]
    fn a_shared_edge_belongs_to_exactly_one_screen() {
        // The primary's top edge (Cocoa y = 1080) is the bottom edge of the screen
        // above. An inclusive upper bound would make the primary claim it too, and
        // the flyout would flicker between screens along that line.
        let screens = [primary(), above_primary()];
        assert_eq!(
            screen_index_for_cursor(
                CocoaPoint {
                    x: 100.0,
                    y: 1080.0
                },
                &screens
            ),
            Some(1)
        );
    }

    #[test]
    fn a_cursor_outside_every_screen_falls_back_to_the_primary() {
        let screens = [primary(), above_primary()];
        // Off to the right of both screens: no frame contains it.
        assert_eq!(
            screen_index_for_cursor(
                CocoaPoint {
                    x: 9000.0,
                    y: 9000.0
                },
                &screens
            ),
            Some(0)
        );
        // A NaN cursor matches nothing, and must not panic or index wildly.
        assert_eq!(
            screen_index_for_cursor(
                CocoaPoint {
                    x: f64::NAN,
                    y: f64::NAN
                },
                &screens
            ),
            Some(0)
        );
    }

    #[test]
    fn an_empty_screen_list_has_no_answer() {
        assert_eq!(
            screen_index_for_cursor(CocoaPoint { x: 0.0, y: 0.0 }, &[]),
            None
        );
        // The whole conversion declines too, so the backend falls back to its
        // documented default anchor rather than inventing a screen.
        assert!(anchor_from_screens(CocoaPoint { x: 0.0, y: 0.0 }, &[]).is_none());
    }

    // --- the assembled anchor --------------------------------------------

    #[test]
    fn a_single_non_retina_screen_converts_to_a_points_anchor() {
        let screens = [primary()];
        let anchor = anchor_from_screens(
            CocoaPoint {
                x: 1900.0,
                y: 1075.0,
            },
            &screens,
        )
        .expect("a non-empty screen list converts");
        // The cursor sits 5 pt below the top of the screen — in the menu bar,
        // where the tray icon is.
        assert_eq!(anchor.cursor, (1900, 5));
        assert_eq!(
            anchor.work_area,
            WorkRect {
                x: 0,
                y: 25,
                w: 1920,
                h: 1055,
            }
        );
        assert_eq!(anchor.unit, AnchorUnit::Points);
        // Points are already logical, so a window size passes through untouched
        // and only the final position is scaled.
        assert!((anchor.logical_to_anchor() - 1.0).abs() < f32::EPSILON);
        assert!((anchor.anchor_to_physical() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_retina_screen_carries_its_backing_scale_into_the_position_factor_only() {
        // Cursor on the Retina screen above the primary. Its work area and the
        // cursor stay in points; the 2.0 backing scale shows up only in
        // `anchor_to_physical`, which is what `slint::PhysicalPosition` needs.
        // Were the scale applied to the coordinates instead, this anchor's cursor
        // would read (200, -2880) and the flyout would be placed two screens away.
        let screens = [primary(), above_primary()];
        let anchor = anchor_from_screens(
            CocoaPoint {
                x: 100.0,
                y: 2000.0,
            },
            &screens,
        )
        .expect("a non-empty screen list converts");
        assert_eq!(anchor.cursor, (100, -920), "1080 - 2000 = -920");
        assert_eq!(anchor.work_area.y, -1440);
        assert!((anchor.scale - 2.0).abs() < f32::EPSILON);
        assert!((anchor.logical_to_anchor() - 1.0).abs() < f32::EPSILON);
        assert!((anchor.anchor_to_physical() - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_flip_reference_is_the_first_screens_height_not_the_cursors() {
        // The primary is 1080 tall and the screen above is 1440. Both the cursor
        // and the work area must flip against 1080; using the *cursor's* screen
        // height (1440) would shift everything on that screen by 360 pt.
        let screens = [primary(), above_primary()];
        let anchor = anchor_from_screens(CocoaPoint { x: 0.0, y: 1080.0 }, &screens)
            .expect("a non-empty screen list converts");
        assert_eq!(anchor.cursor.1, 0, "1080 - 1080 = 0, not 1440 - 1080 = 360");
        assert_eq!(anchor.work_area.y, -1440, "1080 - (1080 + 1440)");
    }
}

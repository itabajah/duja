//! Pure macOS display rules, shared by the two macOS display backends.
//!
//! Everything here is FFI-free arithmetic over values CoreGraphics reports, so it
//! compiles and its tests run on **every** target; only `duja-ddc`'s `mac` backend
//! and `duja-panel`'s `display_services` backend call it. It lives in this crate
//! because both of them need the identical rule and neither may depend on the
//! other: `duja-ddc` owns external monitors, `duja-panel` owns the built-in panel,
//! and a mirror set routinely spans both (a `MacBook` mirroring its screen to a
//! projector is the commonest Mac mirror layout there is). Two copies of a rule
//! that must agree exactly is the defect this module exists to prevent.
//!
//! Windows needs no twin: one `MONITORINFOEX::szDevice` already answers both
//! questions there, and `MONITORINFO::rcMonitor` is already integer pixels.
//!
//! # The display-surface token: which framebuffer a display draws from
//!
//! CoreGraphics gives every attached display its own `CGDirectDisplayID`, unique
//! per display by construction. That is the right token for *addressing* one
//! display (gamma, bounds), and the wrong one for asking "do these two panels
//! show the same pixels?" — which is the question Duja's mirror merge is built
//! on. In macOS Duplicate/mirror mode, N panels render one framebuffer; the app
//! must collapse them into one control with one overlay, and it detects that by
//! **token equality** (`clone_group::group_clones` buckets on the string). Two
//! distinct display ids at identical bounds would produce two singleton groups —
//! two overlays stacked on the same pixels, which is exactly the `#66` defect the
//! merge cured. See ADR-0018 and `duja-app`'s `backend::DisplayGeom`.
//!
//! `CGDisplayMirrorsDisplay` is the rule that turns one into the other: it
//! answers "which display am I mirroring?", returning `kCGNullDirectDisplay`
//! (`0`) for a display that is not mirroring another — either because it is
//! standalone, or because it is the *master* of a mirror set. So the master's own
//! id names the surface, and every clone reports it. That is [`surface_id`].
//!
//! ## This token names a surface; it does **not** address a display
//!
//! Load-bearing, and the reason each backend carries this **alongside** the
//! display's own id rather than in place of it. A clone's surface token is
//! *another display's* id, and that other display need not be one the same
//! backend enumerated at all:
//!
//! - `duja-ddc`'s `enumerate` filters out the built-in panel
//!   (`CGDisplayIsBuiltin`), so on a mirroring `MacBook` every external clone
//!   reports an id that is deliberately absent from the DDC display set and
//!   belongs to a panel `duja-panel`/`DisplayServices` owns. (Since that panel now
//!   reports a surface token of its own — its id, because a master mirrors
//!   nothing — the two backends' tokens meet in the app and the set collapses to
//!   one control. That is the merge working, not a coincidence: both sides apply
//!   this one function.)
//! - `duja-ddc`'s `enumerate` also skips any display whose EDID cannot be read or
//!   parsed, or whose I2C service cannot be resolved. If the *master* is skipped
//!   for one of those reasons, its clones still report its id.
//!
//! For **bucketing** that is harmless, and in fact exactly right: two externals
//! mirroring one built-in genuinely do share a framebuffer, and the shared key
//! collapses them into one control with one overlay whether or not the master is
//! in the list. A bucket key is compared, never dereferenced.
//!
//! For **addressing** it is a defect. Handing this value to
//! `CGSetDisplayTransferByFormula` would dim a display other than the one the
//! caller meant — in the laptop case, the built-in screen instead of the external
//! monitor whose slider the user just dragged, while that monitor did not change
//! at all. The gamma channel must therefore use the display's own id, and
//! `duja-app`'s `BoundsMap` keeps the two behind separately named accessors so a
//! caller has to say which one it wants.

use crate::dimmer::DisplayBounds;

/// A display's own id together with what CoreGraphics says it mirrors.
///
/// A named struct rather than two positional `u32`s: both fields are the same
/// type, so a positional call site could be written the wrong way round and still
/// compile — and `surface_id(mirrors, display_id)` silently degenerates to "the
/// display's own id" for every real display (a real `display_id` is never `0`),
/// i.e. to the `#66` regression, with nothing to see at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorState {
    /// This display's own `CGDirectDisplayID`.
    pub display_id: u32,
    /// `CGDisplayMirrorsDisplay(display_id)`: the master of this display's mirror
    /// set, or `kCGNullDirectDisplay` (`0`) when it mirrors nothing.
    pub mirrors: u32,
}

/// The surface token for a display: which framebuffer it draws from.
///
/// - **standalone display** → its own id (nothing to share);
/// - **mirror-set master** → its own id (`mirrors` is `0` for the master too);
/// - **mirror-set clone** → the master's id.
///
/// Every member of one mirror set therefore yields the same value, and members of
/// different sets never collide — a master's id is its own and no other
/// display's.
///
/// The result is a *key*, not an address: see the module docs for why a clone's
/// token can name a display a given backend never enumerated, and why the gamma
/// channel must not use it.
///
/// # A note on `0`
/// `0` is `kCGNullDirectDisplay`, never a valid display id, so `mirrors == 0`
/// unambiguously means "not mirroring". A `display_id` of `0` would already be an
/// invalid display upstream; this function does not invent a policy for it and
/// simply returns it unchanged.
#[must_use]
pub const fn surface_id(state: MirrorState) -> u32 {
    if state.mirrors == 0 {
        state.display_id
    } else {
        state.mirrors
    }
}

/// A `CGRect` reduced to its four scalars, in **points**.
///
/// The field names match `CGRect`'s own (`origin.x`/`origin.y`,
/// `size.width`/`size.height`) so the FFI call site that fills this in cannot
/// transpose a pair without the mismatch being visible — the same reason
/// [`MirrorState`] is a struct. This crate is pure and must not depend on
/// `core-graphics`, so each backend flattens its own `CGRect` here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CgRect {
    /// Left edge in the global display space, in points (may be negative).
    pub x: f64,
    /// Top edge in the global display space, in points (may be negative).
    pub y: f64,
    /// Width in points.
    pub width: f64,
    /// Height in points.
    pub height: f64,
}

/// Convert a CoreGraphics rect to [`DisplayBounds`], **in points** — the unit
/// [`DisplayBounds`] documents for macOS, and the one the overlay sink consumes.
///
/// Total by construction, which is the whole point of having one copy of it: a
/// float→integer `as` cast in Rust saturates at the target's bounds and maps
/// `NaN` to `0`, and the extents are clamped at `0` first, so no input — however
/// degenerate — produces a wrapped origin or a nonsense size. A zero extent is
/// then caught downstream by [`DisplayBounds::is_empty`].
#[must_use]
pub fn bounds_from_cg_rect(rect: CgRect) -> DisplayBounds {
    // RATIONALE(clippy::cast_possible_truncation, clippy::cast_sign_loss): these
    // are the saturating float→int casts described above, deliberately chosen for
    // their totality; a display rect that overflows `i32`/`u32` points does not
    // exist, and any value that did would clamp rather than wrap.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bounds = DisplayBounds::new(
        rect.x as i32,
        rect.y as i32,
        rect.width.max(0.0) as u32,
        rect.height.max(0.0) as u32,
    );
    bounds
}

#[cfg(test)]
mod tests {
    use super::{CgRect, MirrorState, bounds_from_cg_rect, surface_id};
    use crate::dimmer::DisplayBounds;

    fn token(display_id: u32, mirrors: u32) -> u32 {
        surface_id(MirrorState {
            display_id,
            mirrors,
        })
    }

    /// `kCGNullDirectDisplay` means "mirroring nothing", so the display names its
    /// own surface. Covers both the standalone display and the mirror-set master.
    #[test]
    fn a_display_mirroring_nothing_names_its_own_surface() {
        assert_eq!(token(7, 0), 7);
    }

    /// A clone reports the master's id, not its own — the whole point. Returning
    /// `display_id` here is the `#66` regression: two ids at one framebuffer.
    #[test]
    fn a_clone_names_the_master_it_mirrors() {
        assert_eq!(token(9, 4), 4);
    }

    /// The master is chosen because `mirrors` says so, **never** because of how
    /// the two ids compare.
    ///
    /// This is the case a fixture set where every clone outranks its master cannot
    /// see: with only `token(9, 4)` and `token(12, 4)` to satisfy, a
    /// `min(display_id, mirrors)` rule passes the whole suite while being a live
    /// `#66` defect — a set with master `10` and clone `3` would then yield two
    /// tokens for one framebuffer. macOS display ids carry no ordering relation to
    /// mirror roles, so the low-id-master case is as ordinary as the other.
    #[test]
    fn the_master_wins_even_when_its_id_is_the_larger_one() {
        assert_eq!(token(3, 10), 10);
    }

    /// The property `group_clones` actually depends on: every member of one
    /// mirror set produces one token, and that token is shared with no other set.
    #[test]
    fn every_member_of_a_mirror_set_yields_one_shared_token() {
        // Set A: master 4, clones 9 and 12. Set B: master 5, clone 6.
        let set_a = [token(4, 0), token(9, 4), token(12, 4)];
        let set_b = [token(5, 0), token(6, 5)];

        assert!(
            set_a.iter().all(|&t| t == set_a[0]),
            "a mirror set must collapse to one token: {set_a:?}"
        );
        assert!(
            set_b.iter().all(|&t| t == set_b[0]),
            "a mirror set must collapse to one token: {set_b:?}"
        );
        assert_ne!(
            set_a[0], set_b[0],
            "two distinct mirror sets must not share a surface"
        );
    }

    /// A clone whose master is not enumerated **by the same backend** — the
    /// `MacBook` mirroring its built-in panel, which `duja-ddc` filters out —
    /// still shares one token with its fellow clones. Bucketing must not require
    /// the master to be in the same list, because in the commonest Mac mirror
    /// layout it is not.
    ///
    /// This is also the case that decides the token cannot double as a gamma
    /// address: `1` here belongs to a panel `duja-ddc` never returns. It is the
    /// panel `duja-panel` *does* return, and because that backend applies this
    /// same function to its own `MirrorState`, the built-in reports `1` too and
    /// the whole set meets in one bucket app-side.
    #[test]
    fn clones_of_a_master_another_backend_owns_still_share_one_surface() {
        // 1 stands in for the built-in panel, filtered by `CGDisplayIsBuiltin`, so
        // it never appears in the DDC list these tokens are grouped within.
        let externals = [token(20, 1), token(21, 1)];
        assert_eq!(
            externals[0], externals[1],
            "two externals mirroring one built-in share a framebuffer"
        );
        assert_eq!(
            externals[0], 1,
            "the token names the surface even though no enumerated DDC display owns it"
        );
        // And the built-in itself, enumerated by the *panel* backend, lands on the
        // same token by the mirror-set-master rule — which is what lets the app
        // collapse a cross-backend mirror set into one control.
        assert_eq!(token(1, 0), externals[0]);
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CgRect {
        CgRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn an_ordinary_rect_converts_field_for_field() {
        assert_eq!(
            bounds_from_cg_rect(rect(0.0, 0.0, 1920.0, 1080.0)),
            DisplayBounds::new(0, 0, 1920, 1080)
        );
    }

    /// A display left of, or above, the primary sits at a negative origin; only
    /// the *extents* are unsigned.
    #[test]
    fn a_negative_origin_survives_the_conversion() {
        assert_eq!(
            bounds_from_cg_rect(rect(-1920.0, -200.0, 1920.0, 1080.0)),
            DisplayBounds::new(-1920, -200, 1920, 1080)
        );
    }

    /// Totality: `NaN` maps to `0` and a negative extent clamps to `0` rather than
    /// wrapping to ~4 billion, which is what an unclamped `as u32` on a negative
    /// float would once have done — and what would size an overlay window at an
    /// absurd extent.
    #[test]
    fn degenerate_input_clamps_instead_of_wrapping() {
        let bounds = bounds_from_cg_rect(rect(f64::NAN, f64::NAN, -5.0, -5.0));
        assert_eq!(bounds, DisplayBounds::new(0, 0, 0, 0));
        assert!(bounds.is_empty());
    }

    /// The saturating half of the same property, on the other end of the range: a
    /// rect far outside any real display space clamps to the integer bounds
    /// instead of wrapping to a small or negative number.
    #[test]
    fn an_out_of_range_rect_saturates_instead_of_wrapping() {
        let bounds = bounds_from_cg_rect(rect(-1e30, 1e30, 1e30, 1e30));
        assert_eq!(bounds.x, i32::MIN);
        assert_eq!(bounds.y, i32::MAX);
        assert_eq!(bounds.width, u32::MAX);
        assert_eq!(bounds.height, u32::MAX);
    }

    /// Points are fractional; the conversion truncates toward zero rather than
    /// rounding, which is what `as` does and what both backends have always done.
    /// Pinned so a later "tidy-up" to `round()` is a deliberate choice rather than
    /// a silent one-point drift in every overlay frame.
    #[test]
    fn fractional_points_truncate_toward_zero() {
        assert_eq!(
            bounds_from_cg_rect(rect(10.9, -10.9, 100.9, 100.9)),
            DisplayBounds::new(10, -10, 100, 100)
        );
    }
}

//! The macOS **display-surface token**: which framebuffer a display draws from.
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
//! This module is FFI-free so it compiles and its tests run on every CI OS; only
//! the macOS backend calls it.

/// The surface token for a display, from its own id and the id it mirrors.
///
/// `mirrors` is `CGDisplayMirrorsDisplay(display_id)`: the master of the mirror
/// set this display belongs to, or `kCGNullDirectDisplay` (`0`) when this display
/// mirrors nothing. So:
///
/// - **standalone display** → its own id (nothing to share);
/// - **mirror-set master** → its own id (`mirrors` is `0` for the master too);
/// - **mirror-set clone** → the master's id.
///
/// Every member of one mirror set therefore yields the same value, and members of
/// different sets never collide (a master's id is its own and no other display's).
///
/// The token stays **addressable**, which the gamma channel requires: it is
/// always a real `CGDirectDisplayID`, and for a mirror set it is specifically the
/// master's — the drawable member of the set.
///
/// # A note on `0`
/// `0` is `kCGNullDirectDisplay`, never a valid display id, so `mirrors == 0`
/// unambiguously means "not mirroring". A `display_id` of `0` would already be an
/// invalid display upstream; this function does not invent a policy for it and
/// simply returns it unchanged.
#[must_use]
pub(crate) const fn surface_id(display_id: u32, mirrors: u32) -> u32 {
    if mirrors == 0 { display_id } else { mirrors }
}

#[cfg(test)]
mod tests {
    use super::surface_id;

    /// `kCGNullDirectDisplay` means "mirroring nothing", so the display names its
    /// own surface. Covers both the standalone display and the mirror-set master.
    #[test]
    fn a_display_mirroring_nothing_names_its_own_surface() {
        assert_eq!(surface_id(7, 0), 7);
        assert_eq!(surface_id(u32::MAX, 0), u32::MAX);
    }

    /// A clone reports the master's id, not its own — the whole point. Returning
    /// `display_id` here is the `#66` regression: two ids at one framebuffer.
    #[test]
    fn a_clone_names_the_master_it_mirrors() {
        assert_eq!(surface_id(9, 4), 4);
    }

    /// The property `group_clones` actually depends on: every member of one
    /// mirror set produces one token, and that token is shared with no other set.
    #[test]
    fn every_member_of_a_mirror_set_yields_one_shared_token() {
        // Set A: master 4, clones 9 and 12. Set B: master 5, clone 6.
        let set_a = [surface_id(4, 0), surface_id(9, 4), surface_id(12, 4)];
        let set_b = [surface_id(5, 0), surface_id(6, 5)];

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

    /// Extended (non-mirrored) desktops stay distinct — the token must not
    /// over-merge, or independent monitors would collapse into one control.
    #[test]
    fn extended_displays_keep_distinct_surfaces() {
        assert_ne!(surface_id(1, 0), surface_id(2, 0));
    }
}

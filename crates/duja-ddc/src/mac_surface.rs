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
//! # This token names a surface; it does **not** address a display
//!
//! Load-bearing, and the reason `DdcDisplay` carries this **alongside**
//! `cg_display_id` rather than in place of it. A clone's surface token is
//! *another display's* id, and that other display need not be one Duja
//! enumerated at all:
//!
//! - `enumerate` filters out the built-in panel (`CGDisplayIsBuiltin`), and a
//!   `MacBook` mirroring its built-in screen to a projector is the most common Mac
//!   mirror configuration there is — with the built-in as the master. Every
//!   external clone then reports an id that is deliberately absent from the DDC
//!   display set and belongs to a panel `duja-panel`/`DisplayServices` owns.
//! - `enumerate` also skips any display whose EDID cannot be read or parsed, or
//!   whose I2C service cannot be resolved. If the *master* is skipped for one of
//!   those reasons, its clones still report its id.
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
//! at all. The gamma channel must therefore use `DdcDisplay::cg_display_id`, and
//! `duja-app`'s `BoundsMap` keeps the two behind separately named accessors so a
//! caller has to say which one it wants.
//!
//! This module is FFI-free so it compiles and its tests run on every CI OS; only
//! the macOS backend calls it.

/// A display's own id together with what CoreGraphics says it mirrors.
///
/// A named struct rather than two positional `u32`s: both fields are the same
/// type, so a positional call site could be written the wrong way round and still
/// compile — and `surface_id(mirrors, display_id)` silently degenerates to "the
/// display's own id" for every real display (a real `display_id` is never `0`),
/// i.e. to the `#66` regression, with nothing to see at the call site.
#[derive(Clone, Copy)]
pub(crate) struct MirrorState {
    /// This display's own `CGDirectDisplayID`.
    pub(crate) display_id: u32,
    /// `CGDisplayMirrorsDisplay(display_id)`: the master of this display's mirror
    /// set, or `kCGNullDirectDisplay` (`0`) when it mirrors nothing.
    pub(crate) mirrors: u32,
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
/// token can name a display Duja never enumerated, and why the gamma channel must
/// not use it.
///
/// # A note on `0`
/// `0` is `kCGNullDirectDisplay`, never a valid display id, so `mirrors == 0`
/// unambiguously means "not mirroring". A `display_id` of `0` would already be an
/// invalid display upstream; this function does not invent a policy for it and
/// simply returns it unchanged.
#[must_use]
pub(crate) const fn surface_id(state: MirrorState) -> u32 {
    if state.mirrors == 0 {
        state.display_id
    } else {
        state.mirrors
    }
}

#[cfg(test)]
mod tests {
    use super::{MirrorState, surface_id};

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

    /// A clone whose master is **not** an enumerated display — the `MacBook`
    /// mirroring its built-in panel, which `enumerate` filters out — still shares
    /// one token with its fellow clones. Bucketing must not require the master to
    /// be present, because in the commonest Mac mirror layout it is not.
    ///
    /// This is also the case that decides the token cannot double as a gamma
    /// address: `1` here belongs to a panel this backend never returns.
    #[test]
    fn clones_of_an_unenumerated_master_still_share_one_surface() {
        // 1 stands in for the built-in panel, filtered by `CGDisplayIsBuiltin`, so
        // it never appears in the list these tokens are grouped within.
        let externals = [token(20, 1), token(21, 1)];
        assert_eq!(
            externals[0], externals[1],
            "two externals mirroring one built-in share a framebuffer"
        );
        assert_eq!(
            externals[0], 1,
            "the token names the surface even though no enumerated display owns it"
        );
    }
}

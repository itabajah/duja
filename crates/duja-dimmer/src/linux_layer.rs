//! The decisions a Wayland layer-shell overlay makes, separated from making it.
//!
//! Creating a click-through translucent layer surface is imperative
//! `wayland-client` work that no CI lane can run. The choices inside it are not,
//! and each fails in a way that is invisible until someone has a screen in front
//! of them — or that is not visible at all, because it kills the connection:
//!
//! - **What the surface asks not to be moved for.** `set_exclusive_zone`
//!   defaults to `0`, which asks the compositor to *move the surface out from
//!   under* anything holding an exclusive zone. For a panel, that is correct. For
//!   a dimmer it leaves a bright undimmed strip exactly where the panel is. The
//!   value a dimmer wants is `-1`, and the protocol names wallpapers and lock
//!   screens as the precedent.
//! - **Whether an omitted size is even legal.** `set_size(0, 0)` asks the
//!   compositor to size the surface, which is the only way to get this right
//!   under fractional scaling. But *"you must set your anchor to opposite edges
//!   in the dimensions you omit; not doing so is a protocol error"* — so size and
//!   anchor are one decision, not two. Getting it wrong is `invalid_size`, which
//!   terminates the client rather than degrading.
//! - **Whether clicks reach the desktop.** `keyboard_interactivity: none` covers
//!   the keyboard and nothing else. Pointer input needs an **empty**
//!   `wl_surface` input region. Forget it and the overlay swallows every click,
//!   which is the Wayland shape of the same hazard
//!   [`crate::linux_overlay::XFIXES_INPUT_SHAPE_VERSION`] guards on X11.
//!
//! So they live here, as plain data and plain arithmetic, tested on all three
//! lanes — the same split [`crate::linux_caps`], [`crate::linux_overlay`] and
//! [`crate::linux_gamma`] use, and for the same reason: this module names no
//! `wayland-client` type, so it compiles where that crate does not exist.
//!
//! Whether a session *has* layer-shell at all is not decided here.
//! [`crate::linux_caps`] already answers that from the registry, and already
//! records that layer-shell and `zwlr_gamma_control_v1` are independent — a
//! Plasma session has the first and not the second.

/// The `zwlr_layer_shell_v1` layer a dimming overlay belongs in.
///
/// `background` (0), `bottom` (1), `top` (2), `overlay` (3), ordered by z depth.
/// A dimmer has to be above ordinary shell surfaces *and* above panels, so it is
/// the topmost one. Pinned as a constant because the wire value is what travels;
/// an off-by-one here puts the dim under the windows it is meant to dim, and
/// nothing reports an error.
pub const LAYER_OVERLAY: u32 = 3;

/// `zwlr_layer_surface_v1::anchor`, a bitfield.
pub const ANCHOR_TOP: u32 = 1;
/// See [`ANCHOR_TOP`].
pub const ANCHOR_BOTTOM: u32 = 2;
/// See [`ANCHOR_TOP`].
pub const ANCHOR_LEFT: u32 = 4;
/// See [`ANCHOR_TOP`].
pub const ANCHOR_RIGHT: u32 = 8;

/// All four edges: what a surface that covers a whole output anchors to.
pub const ANCHOR_ALL: u32 = ANCHOR_TOP | ANCHOR_BOTTOM | ANCHOR_LEFT | ANCHOR_RIGHT;

/// The exclusive zone a dimmer asks for.
///
/// `-1` is *"do not move me to accommodate other surfaces, extend me to the edges
/// I am anchored to"*. The default is `0`, which is the opposite request.
pub const DIMMER_EXCLUSIVE_ZONE: i32 = -1;

/// Whether a surface accepts pointer input.
///
/// A two-variant enum rather than a bool because the failure is silent and the
/// polarity is easy to invert: [`PointerInput::Empty`] is the region that lets
/// clicks through, and "empty region" reads like "no region set", which is the
/// state that swallows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerInput {
    /// An explicitly empty `wl_surface` input region: every click passes through
    /// to whatever is underneath.
    Empty,
    /// No input region set, so the surface accepts pointer input across its whole
    /// extent. For a fullscreen dimmer this makes the desktop unclickable.
    Inherited,
}

/// What a dimming layer surface asks the compositor for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerSurface {
    /// Which layer, see [`LAYER_OVERLAY`].
    pub layer: u32,
    /// Which edges, see [`ANCHOR_ALL`].
    pub anchor: u32,
    /// See [`DIMMER_EXCLUSIVE_ZONE`].
    pub exclusive_zone: i32,
    /// `0` delegates the width to the compositor. Legal only under the rule in
    /// [`size_is_legal`].
    pub width: u32,
    /// `0` delegates the height to the compositor. Same rule.
    pub height: u32,
    /// See [`PointerInput`].
    pub pointer_input: PointerInput,
}

/// Whether this size is legal for this anchor.
///
/// The protocol's rule, verbatim: *"If you pass 0 for either value, the
/// compositor will assign it and inform you of the assignment in the configure
/// event. You must set your anchor to opposite edges in the dimensions you omit;
/// not doing so is a protocol error."*
///
/// That protocol error is `invalid_size`, and a protocol error is fatal to the
/// whole `wayland-client` connection — not a `closed` event this backend could
/// recover from. So this is checked before the request is sent rather than
/// handled after.
#[must_use]
pub const fn size_is_legal(anchor: u32, width: u32, height: u32) -> bool {
    let horizontal = ANCHOR_LEFT | ANCHOR_RIGHT;
    let vertical = ANCHOR_TOP | ANCHOR_BOTTOM;
    let width_ok = width != 0 || (anchor & horizontal) == horizontal;
    let height_ok = height != 0 || (anchor & vertical) == vertical;
    width_ok && height_ok
}

/// The layer surface a dimmer creates on one output.
///
/// The size is `(0, 0)` on purpose: it hands sizing to the compositor, which is
/// the only way to cover an output exactly under fractional scaling. Duja cannot
/// compute that rectangle itself — `wl_output` reports physical pixels and an
/// integer scale, which is precisely why the workspace declares
/// `wayland-protocols` for `zxdg_output_manager_v1` at all — and here it does not
/// need to.
#[must_use]
pub const fn dimmer_surface() -> LayerSurface {
    LayerSurface {
        layer: LAYER_OVERLAY,
        anchor: ANCHOR_ALL,
        exclusive_zone: DIMMER_EXCLUSIVE_ZONE,
        width: 0,
        height: 0,
        pointer_input: PointerInput::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ANCHOR_ALL, ANCHOR_BOTTOM, ANCHOR_LEFT, ANCHOR_RIGHT, ANCHOR_TOP, DIMMER_EXCLUSIVE_ZONE,
        LAYER_OVERLAY, PointerInput, dimmer_surface, size_is_legal,
    };

    /// The wire values, pinned against `wlr-layer-shell-unstable-v1.xml`. These
    /// are not arbitrary names — they travel, and every one of them fails
    /// silently rather than loudly.
    #[test]
    fn the_protocol_constants_are_the_ones_on_the_wire() {
        assert_eq!(LAYER_OVERLAY, 3, "overlay is the topmost of four layers");
        assert_eq!(ANCHOR_TOP, 1);
        assert_eq!(ANCHOR_BOTTOM, 2);
        assert_eq!(ANCHOR_LEFT, 4);
        assert_eq!(ANCHOR_RIGHT, 8);
        assert_eq!(ANCHOR_ALL, 15, "all four edges");
    }

    /// The defaulted value is the wrong one, so this pins the difference rather
    /// than the value: `0` asks to be moved aside for the panel.
    #[test]
    fn a_dimmer_refuses_to_be_moved_out_from_under_a_panel() {
        assert_eq!(DIMMER_EXCLUSIVE_ZONE, -1);
        assert_ne!(
            DIMMER_EXCLUSIVE_ZONE, 0,
            "0 is the protocol default and asks the compositor to move the \
             surface aside for any panel, which leaves an undimmed strip"
        );
        assert_eq!(dimmer_surface().exclusive_zone, DIMMER_EXCLUSIVE_ZONE);
    }

    #[test]
    fn a_dimmer_sits_above_the_windows_it_dims() {
        assert_eq!(dimmer_surface().layer, LAYER_OVERLAY);
    }

    /// The hazard is the polarity: an *empty* region passes clicks through, and
    /// no region at all captures them.
    #[test]
    fn a_dimmer_lets_every_click_reach_the_desktop() {
        assert_eq!(dimmer_surface().pointer_input, PointerInput::Empty);
    }

    /// The protocol rule, in both directions. Omitting a dimension without both
    /// of its opposite anchors is `invalid_size`, which is fatal to the
    /// connection.
    #[test]
    fn an_omitted_dimension_needs_both_of_its_opposite_anchors() {
        // Both omitted, all four anchors: legal.
        assert!(size_is_legal(ANCHOR_ALL, 0, 0));

        // Width omitted with only one horizontal anchor: not legal.
        assert!(!size_is_legal(
            ANCHOR_LEFT | ANCHOR_TOP | ANCHOR_BOTTOM,
            0,
            0
        ));
        assert!(!size_is_legal(
            ANCHOR_RIGHT | ANCHOR_TOP | ANCHOR_BOTTOM,
            0,
            0
        ));

        // Height omitted with only one vertical anchor: not legal.
        assert!(!size_is_legal(
            ANCHOR_TOP | ANCHOR_LEFT | ANCHOR_RIGHT,
            0,
            0
        ));
        assert!(!size_is_legal(
            ANCHOR_BOTTOM | ANCHOR_LEFT | ANCHOR_RIGHT,
            0,
            0
        ));

        // A stated dimension needs no anchors at all.
        assert!(size_is_legal(0, 1920, 1080));
        // ...and a stated width still lets an omitted height be checked.
        assert!(!size_is_legal(ANCHOR_TOP, 1920, 0));
        assert!(size_is_legal(ANCHOR_TOP | ANCHOR_BOTTOM, 1920, 0));
    }

    /// The two decisions are coupled, so the surface this module actually builds
    /// has to satisfy the rule this module actually states. Neither test above
    /// catches a `dimmer_surface` that drops an anchor.
    #[test]
    fn the_surface_a_dimmer_builds_is_legal_by_that_rule() {
        let surface = dimmer_surface();
        assert!(
            size_is_legal(surface.anchor, surface.width, surface.height),
            "a delegated size with a partial anchor is invalid_size, which \
             terminates the connection"
        );
    }

    /// Sizing is the compositor's job here, and that is the point: Duja cannot
    /// compute a fractionally-scaled output rectangle from `wl_output` alone.
    #[test]
    fn a_dimmer_lets_the_compositor_size_it() {
        let surface = dimmer_surface();
        assert_eq!((surface.width, surface.height), (0, 0));
    }
}

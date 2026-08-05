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
//! - **What the surface is actually filled with.** A dim is one translucent black
//!   rectangle, so every level it can ever show fits in [`dim_pool`] — a kilobyte
//!   written once, indexed by [`dim_pool_offset`]. An offset past the end is not a
//!   wrong colour, it is a `wl_shm` error, and those are fatal too.
//! - **What size the compositor said to be.** [`viewport_destination`] turns a
//!   `configure` into a `wp_viewport.set_destination`, and refuses the sizes that
//!   request would answer with `bad_value`.
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

use duja_core::dimmer::DisplayBounds;

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

/// Bytes per pixel in `wl_shm`'s `argb8888`, where a slice index needs it.
pub const ARGB_BYTES: usize = 4;

/// The same number as [`ARGB_BYTES`], where the wire needs it: the offset and the
/// stride of `wl_shm_pool.create_buffer` are both `int`, and this buffer is one
/// pixel wide, so its stride is one pixel.
///
/// Written out rather than cast from its twin. Every conversion between the two
/// would trip a pedantic cast lint and need an `#[allow]`, and the test that pins
/// them to each other is the stronger statement in any case — it fails if either
/// one moves, which a cast by construction cannot.
pub const ARGB_STRIDE: i32 = 4;

/// How many dim levels there are: one per `u8` alpha, which is the whole range
/// [`duja_core::dimmer::DimCommand`] can ask for.
pub const DIM_LEVELS: usize = u8::MAX as usize + 1;

/// The size of the pool [`dim_pool`] fills. A kilobyte.
pub const DIM_POOL_BYTES: usize = DIM_LEVELS * ARGB_BYTES;

/// Every dim level a Wayland overlay can ever show, as one `wl_shm` pool.
///
/// One 1x1 `argb8888` pixel per alpha, in ascending order, so the pool is written
/// **once** at startup and never again. That is not an optimisation, it is what
/// removes the only shared-memory hazard this backend would otherwise have: a
/// pool the client rewrites while the compositor may be sampling it is a data race
/// across a process boundary that no `wl_buffer.release` handling makes safe for a
/// buffer still attached elsewhere. Nothing here is ever rewritten, so there is
/// nothing to race, and changing a dim level is an attach of a different
/// pre-existing buffer rather than a write.
///
/// One pixel per level is enough because a `wp_viewport` scales it to the output;
/// see [`viewport_destination`]. Without that the pool would have to hold a full
/// framebuffer per output — tens of megabytes, re-rendered per slider sample.
///
/// The pixel is [`crate::linux_overlay::premultiplied_black`], the same value the
/// X11 backend puts in a window's background: `argb8888` is defined as
/// *"\[31:0\] A:R:G:B 8:8:8:8 little endian"*, so its bytes are exactly that `u32`
/// in little-endian order, on any host. Premultiplied is what the compositor
/// expects, and black premultiplies to zero in all three colour channels, which is
/// why the alpha byte is the only one that ever varies here.
#[must_use]
pub fn dim_pool() -> [u8; DIM_POOL_BYTES] {
    let mut pool = [0_u8; DIM_POOL_BYTES];
    for alpha in 0..=u8::MAX {
        let at = usize::from(alpha).saturating_mul(ARGB_BYTES);
        if let Some(slot) = pool.get_mut(at..at.saturating_add(ARGB_BYTES)) {
            slot.copy_from_slice(&crate::linux_overlay::premultiplied_black(alpha).to_le_bytes());
        }
    }
    pool
}

/// Where `alpha`'s pixel starts in [`dim_pool`], as `wl_shm_pool.create_buffer`
/// counts.
///
/// `i32` because that is the request's type, and the arithmetic cannot overflow
/// it: the largest offset is `255 * 4`. An offset that ran past the end of the pool
/// would not be a wrong shade — the compositor answers `wl_shm.error.invalid_fd`
/// or refuses the buffer outright, and either way the connection dies — which is
/// why this and [`dim_pool`] are pinned against each other by a test rather than
/// each against its own arithmetic.
/// Not `const`: `i32::from` is not a const trait method yet, and widening with an
/// `as` cast instead would trade a compile-time guarantee nothing here needs for a
/// pedantic-lint `#[allow]`.
#[must_use]
pub fn dim_pool_offset(alpha: u8) -> i32 {
    // Cannot saturate: the largest product is `255 * 4`. Spelled this way because
    // the crate denies bare arithmetic, and a `#[allow]` here would be a standing
    // exemption on the one expression whose overflow is a protocol error.
    i32::from(alpha).saturating_mul(ARGB_STRIDE)
}

/// Which of the compositor's outputs a display's rectangle names, if any is still
/// free.
///
/// `zwlr_layer_shell_v1.get_layer_surface` takes a `wl_output`, so a Wayland
/// overlay is **bound to an output** rather than placed at a rectangle on a root
/// window the way the X11 one is. Everything above this layer speaks in
/// [`DisplayBounds`], so something has to turn one into the other, and this is it —
/// it is what the Wayland backend passes as `placeable` to
/// [`crate::linux_overlay::plan_record`].
///
/// # Equality, not containment
///
/// The rectangle is compared exactly, which is only correct because both sides
/// come from the same place: `logical` is each output's `zxdg_output_v1` logical
/// geometry, and a [`DimCommand`](duja_core::dimmer::DimCommand)'s bounds reached
/// the caller through [`crate::linux_outputs::join`], which took them from that
/// same event. A near-match would mean the two have diverged, and dimming the
/// nearest output would then be a guess about which monitor the user meant.
///
/// # `taken` is what makes mirroring work
///
/// Two mirrored outputs are two `wl_output`s at one logical rectangle, and the
/// layer above sends one command per *display*, so both commands carry the same
/// bounds. Without excluding what is already dimmed, both overlays would land on
/// the first output: one monitor dimmed twice, the other not at all, and no error
/// anywhere. `taken` is the indices the caller's live overlays already hold.
///
/// An output whose `zxdg_output_v1` geometry has not arrived yet is `None` and is
/// never chosen. That is a display left undimmed for one apply rather than an
/// overlay on the wrong monitor, and the next apply has the geometry.
#[must_use]
pub fn take_output(
    wanted: DisplayBounds,
    logical: &[Option<DisplayBounds>],
    taken: &[usize],
) -> Option<usize> {
    logical
        .iter()
        .enumerate()
        .find(|(index, bounds)| **bounds == Some(wanted) && !taken.contains(index))
        .map(|(index, _)| index)
}

/// The `wp_viewport.set_destination` a `zwlr_layer_surface_v1.configure` implies,
/// or `None` when there is no legal one.
///
/// The surface is a single pixel scaled to the whole output, so the destination is
/// the size the compositor just assigned. Two of those are not requestable:
///
/// - **Zero.** `configure` states outright that *"if the width or height arguments
///   are zero, it means the client should decide its own window dimension"*, and
///   Duja cannot — that is the entire reason [`dimmer_surface`] delegates sizing.
///   Passing it on anyway is `wp_viewport.error.bad_value` (*"negative or zero
///   values in width or height"*), which is a protocol error and kills the
///   connection, taking every other output's overlay with it.
/// - **Above `i32::MAX`.** `configure` carries `uint` and `set_destination` takes
///   `int`, so the wire itself cannot express the top half of the range. No
///   compositor sends it, and the zero check below would catch a wrapped one
///   anyway — an `as` cast turns every such width into a negative. Converting
///   instead of casting is here to say which of the two rules refused it, so a
///   later edit to either cannot quietly leave the other doing both jobs.
///
/// `None` means the surface stays unmapped rather than that the connection ends:
/// an output nobody can size is one output not dimmed, and the rest keep working.
#[must_use]
pub fn viewport_destination(width: u32, height: u32) -> Option<(i32, i32)> {
    let width = i32::try_from(width).ok()?;
    let height = i32::try_from(height).ok()?;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::{
        ANCHOR_ALL, ANCHOR_BOTTOM, ANCHOR_LEFT, ANCHOR_RIGHT, ANCHOR_TOP, ARGB_BYTES, ARGB_STRIDE,
        DIM_POOL_BYTES, DIMMER_EXCLUSIVE_ZONE, DisplayBounds, LAYER_OVERLAY, PointerInput,
        dim_pool, dim_pool_offset, dimmer_surface, size_is_legal, take_output,
        viewport_destination,
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

    /// One pixel's worth of bytes, counted two ways because the wire and a slice
    /// index disagree about the type. A drift between them puts every level at
    /// the wrong offset.
    #[test]
    fn a_pixel_is_the_same_size_on_the_wire_as_in_the_pool() {
        assert_eq!(usize::try_from(ARGB_STRIDE).unwrap(), ARGB_BYTES);
    }

    /// The two halves of the pool scheme, pinned against **each other**. Either
    /// alone is self-consistent while being wrong: a builder that strides by 3 and
    /// an offset that strides by 3 agree with themselves and hand the compositor
    /// a shade nobody asked for.
    #[test]
    fn a_level_is_black_at_exactly_its_own_alpha() {
        let pool = dim_pool();
        for alpha in 0..=u8::MAX {
            let at = usize::try_from(dim_pool_offset(alpha)).unwrap();
            let pixel = pool.get(at..at + ARGB_BYTES).unwrap();
            assert_eq!(
                pixel,
                [0, 0, 0, alpha],
                "argb8888 is A:R:G:B little endian, so black premultiplies to \
                 three zero bytes and the alpha byte is the level, at {alpha}"
            );
        }
    }

    /// The off-by-one that is a protocol error rather than a wrong colour: a
    /// `create_buffer` whose offset plus stride runs past the pool is refused and
    /// the connection dies.
    #[test]
    fn the_last_level_ends_exactly_at_the_end_of_the_pool() {
        let pool = dim_pool();
        assert_eq!(pool.len(), DIM_POOL_BYTES);
        let last = usize::try_from(dim_pool_offset(u8::MAX)).unwrap();
        assert_eq!(
            last + ARGB_BYTES,
            pool.len(),
            "the brightest level's pixel is the final four bytes; anything else \
             either wastes the pool or reads off the end of it"
        );
    }

    /// `configure` is allowed to say "you decide", and this backend cannot — the
    /// whole reason it delegates sizing. Passing the zero on to `set_destination`
    /// is `bad_value`, which is fatal to the connection rather than to one output.
    #[test]
    fn a_configure_that_states_no_size_is_not_a_viewport_destination() {
        assert_eq!(viewport_destination(0, 1080), None);
        assert_eq!(viewport_destination(1920, 0), None);
        assert_eq!(viewport_destination(0, 0), None);
    }

    /// `configure` carries `uint` and `set_destination` takes `int`, so the top
    /// half of the range has no representation. Converting rather than casting is
    /// what keeps that from wrapping into a negative, which is the same protocol
    /// error by a longer route.
    #[test]
    fn a_size_the_wire_cannot_carry_is_refused() {
        assert_eq!(viewport_destination(u32::MAX, 1080), None);
        assert_eq!(viewport_destination(1920, u32::MAX), None);
        let too_wide = u32::try_from(i32::MAX).unwrap() + 1;
        assert_eq!(viewport_destination(too_wide, 1080), None);
        assert_eq!(
            viewport_destination(too_wide - 1, 1080),
            Some((i32::MAX, 1080)),
            "the boundary itself is representable and must not be refused"
        );
    }

    #[test]
    fn a_stated_configure_size_is_the_destination() {
        assert_eq!(viewport_destination(1920, 1080), Some((1920, 1080)));
        assert_eq!(viewport_destination(1, 1), Some((1, 1)));
    }

    fn at(x: i32) -> DisplayBounds {
        DisplayBounds::new(x, 0, 1920, 1080)
    }

    #[test]
    fn a_display_is_dimmed_on_the_output_whose_rectangle_it_is() {
        let outputs = [Some(at(0)), Some(at(1920)), Some(at(3840))];
        assert_eq!(
            take_output(DisplayBounds::new(1920, 0, 1920, 1080), &outputs, &[]),
            Some(1)
        );
    }

    /// The mirroring case, which is the whole reason `taken` exists. Two outputs
    /// at one rectangle receive one overlay each; without this both commands
    /// resolve to output 0 and the second monitor is never dimmed.
    #[test]
    fn two_outputs_mirroring_one_rectangle_get_one_overlay_each() {
        let outputs = [Some(at(0)), Some(at(0))];
        let wanted = DisplayBounds::new(0, 0, 1920, 1080);

        let first = take_output(wanted, &outputs, &[]).unwrap();
        let second = take_output(wanted, &outputs, &[first]).unwrap();

        assert_ne!(first, second);
        // And a third display at the same rectangle has nowhere left to go, which
        // is a display undimmed rather than a second overlay stacked on one
        // monitor.
        assert_eq!(take_output(wanted, &outputs, &[first, second]), None);
    }

    /// A rectangle no output has is not rounded to the nearest one. Both sides of
    /// the comparison come from the same `zxdg_output_v1` event, so a near-match
    /// means they have diverged, and picking the closest monitor would be a guess
    /// about which screen the user asked for.
    #[test]
    fn a_rectangle_no_output_has_is_not_matched_to_the_closest() {
        let outputs = [Some(at(0)), Some(at(1920))];
        assert_eq!(
            take_output(DisplayBounds::new(1919, 0, 1920, 1080), &outputs, &[]),
            None
        );
        assert_eq!(
            take_output(DisplayBounds::new(0, 0, 1920, 1081), &outputs, &[]),
            None
        );
    }

    /// Geometry arrives as an event, so an output can be bound and not yet
    /// placed. Dimming it anyway would put an overlay on a monitor chosen by
    /// registry order.
    #[test]
    fn an_output_that_has_not_said_where_it_is_yet_is_never_chosen() {
        let outputs = [None, Some(at(0))];
        assert_eq!(
            take_output(DisplayBounds::new(0, 0, 1920, 1080), &outputs, &[]),
            Some(1)
        );
        assert_eq!(
            take_output(DisplayBounds::new(0, 0, 1920, 1080), &[None], &[]),
            None
        );
    }
}

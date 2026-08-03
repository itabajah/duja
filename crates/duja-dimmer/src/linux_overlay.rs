//! The decisions an X11 overlay window makes, separated from making it.
//!
//! Creating a click-through translucent window is imperative X11 work that no CI
//! lane can run. Three of the choices inside it are not, and each of them fails
//! in a way that is invisible until someone has a screen in front of them:
//!
//! - **Which visual to use.** Get this wrong and the overlay is opaque. There is
//!   no partial failure and no error to report — the window is created, mapped,
//!   and black.
//! - **What pixel to fill with.** The alpha lives in the top byte of a
//!   premultiplied ARGB value, and a colour component that is not zero at low
//!   alpha is a *brighter* overlay than asked for rather than a darker one.
//! - **Whether the rectangle is even expressible.** X11 window geometry is 16-bit
//!   signed position and 16-bit unsigned size. A desktop wider than 32767 pixels
//!   silently wraps if the conversion is done with `as`.
//!
//! So they live here, as plain arithmetic over plain data, and are tested on all
//! three lanes — the same split [`crate::linux_caps`] and [`crate::linux_outputs`]
//! use, and for the same reason: this module names no `x11rb` type, so it
//! compiles where that crate does not exist.

use duja_core::dimmer::DisplayBounds;

/// `_NET_WM_BYPASS_COMPOSITOR`'s "never bypass" value.
///
/// Every compositing manager unredirects a fullscreen window as a performance
/// optimisation, and an always-on-top fullscreen window is exactly what an
/// overlay is. Unredirected means the X server draws it directly, which ignores
/// the alpha channel — the same solid-black screen the compositor check in
/// [`crate::linux_caps`] exists to prevent, reached past that check. The EWMH
/// answer is this property: 0 means no preference, 1 means the window would like
/// to bypass, and 2 means it must never be bypassed.
pub const BYPASS_COMPOSITOR_NEVER: u32 = 2;

/// The pixel an overlay is filled with for a given alpha byte.
///
/// **Premultiplied** ARGB, which is what a compositing manager expects from a
/// 32-bit visual: each colour component is already scaled by the alpha, so black
/// at any alpha has all three at zero and only the top byte varies. Writing an
/// un-premultiplied `0xAA00_0000 | 0x00FF_FFFF` here would ask the compositor to
/// blend *white* at that alpha and wash the screen out instead of dimming it.
///
/// `alpha` is the quantized byte the [`crate::plan`] kernel produces, so 0 is
/// "no overlay" (the backend destroys the window rather than filling it with
/// this) and 255 is fully opaque.
#[must_use]
pub const fn premultiplied_black(alpha: u8) -> u32 {
    (alpha as u32) << 24
}

/// One visual the X server offers, reduced to what choosing needs.
///
/// Plain data rather than an `x11rb` `Visualtype`, so the rule below is testable
/// on every lane. The caller flattens the server's `allowed_depths` into these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualCandidate {
    /// The visual's id, which is what the choice returns.
    pub id: u32,
    /// The depth this visual was listed under.
    pub depth: u8,
    /// Whether the class is `TrueColor`. Only a `TrueColor` visual has fixed
    /// channel masks; a `DirectColor` one of the same depth needs a colormap
    /// loaded with a ramp before its pixels mean anything.
    pub true_color: bool,
    /// The red channel mask.
    pub red_mask: u32,
    /// The green channel mask.
    pub green_mask: u32,
    /// The blue channel mask.
    pub blue_mask: u32,
}

/// The depth an overlay needs: 24 bits of colour and 8 of alpha.
pub const ARGB_DEPTH: u8 = 32;

/// The bits the three colour masks must cover between them, leaving the top byte
/// for alpha.
const COLOUR_MASK: u32 = 0x00FF_FFFF;

/// Choose the visual to create the overlay window on.
///
/// Returns the id of a depth-32 `TrueColor` visual whose three colour masks cover
/// exactly the low 24 bits, so the high byte is the alpha channel — the only
/// shape on which [`premultiplied_black`] means what it says.
///
/// `None` when the server offers no such visual, and the caller must then **not**
/// fall back to the root's depth-24 visual. That window would be created and
/// mapped successfully and would be opaque black, which is worse than no overlay
/// at all: a display that cannot be dimmed in software is a missing feature, and
/// a screen that goes black with no visible way back is a broken machine.
///
/// The first match wins. Servers list visuals in a fixed order and any visual
/// satisfying every condition here is interchangeable for this purpose — the
/// masks *are* the pixel layout, so two visuals that pass have the same one.
#[must_use]
pub fn choose_argb_visual(candidates: &[VisualCandidate]) -> Option<u32> {
    candidates
        .iter()
        .find(|visual| {
            visual.depth == ARGB_DEPTH
                && visual.true_color
                // Disjoint and covering: three non-overlapping masks whose union
                // is the low 24 bits. A visual whose channels overlap, or that
                // spends a bit above the 24th, does not leave a clean alpha byte
                // and `premultiplied_black` would not address it correctly.
                && visual.red_mask & visual.green_mask == 0
                && visual.red_mask & visual.blue_mask == 0
                && visual.green_mask & visual.blue_mask == 0
                && visual.red_mask | visual.green_mask | visual.blue_mask == COLOUR_MASK
        })
        .map(|visual| visual.id)
}

/// The X11 window geometry for a display's bounds, or `None` if it does not fit.
///
/// X11 window position is a pair of **signed 16-bit** integers and size a pair of
/// **unsigned 16-bit** ones — a protocol limit, not an implementation one. A
/// desktop wide enough to put a monitor past 32767 pixels is unusual and not
/// impossible (four 8K displays side by side reach it), and doing this conversion
/// with `as` would wrap such a monitor's overlay to the far side of the screen,
/// covering a display the user did not ask to dim.
///
/// A zero-area rectangle is also refused. `X11` rejects a zero width or height
/// with a `BadValue`, and the caller has nothing to cover anyway.
#[must_use]
pub fn x11_rect(bounds: DisplayBounds) -> Option<(i16, i16, u16, u16)> {
    if bounds.is_empty() {
        return None;
    }
    let x = i16::try_from(bounds.x).ok()?;
    let y = i16::try_from(bounds.y).ok()?;
    let width = u16::try_from(bounds.width).ok()?;
    let height = u16::try_from(bounds.height).ok()?;
    // The far edge has to land inside the protocol's range too: a monitor that
    // starts at 30000 and is 4000 wide cannot be described, and truncating it
    // would leave part of the screen undimmed with no indication why.
    let right = i32::from(x).checked_add(i32::from(width))?;
    let bottom = i32::from(y).checked_add(i32::from(height))?;
    i16::try_from(right).ok()?;
    i16::try_from(bottom).ok()?;
    Some((x, y, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argb(id: u32) -> VisualCandidate {
        VisualCandidate {
            id,
            depth: 32,
            true_color: true,
            red_mask: 0x00FF_0000,
            green_mask: 0x0000_FF00,
            blue_mask: 0x0000_00FF,
        }
    }

    fn opaque(id: u32) -> VisualCandidate {
        VisualCandidate {
            id,
            depth: 24,
            true_color: true,
            red_mask: 0x00FF_0000,
            green_mask: 0x0000_FF00,
            blue_mask: 0x0000_00FF,
        }
    }

    /// Black is black at every alpha; only the top byte moves. Premultiplied, so
    /// the colour components stay zero rather than scaling with the alpha.
    #[test]
    fn the_fill_pixel_carries_alpha_in_the_top_byte_and_nothing_else() {
        assert_eq!(premultiplied_black(0), 0x0000_0000);
        assert_eq!(premultiplied_black(1), 0x0100_0000);
        assert_eq!(premultiplied_black(128), 0x8000_0000);
        assert_eq!(premultiplied_black(255), 0xFF00_0000);
    }

    /// The failure this guards against is a wash-out rather than a no-op: an
    /// un-premultiplied white-with-alpha pixel asks the compositor to blend
    /// *white* over the screen, which brightens where the user asked to dim.
    #[test]
    fn no_alpha_ever_produces_a_non_black_colour() {
        for alpha in 0..=255_u8 {
            assert_eq!(
                premultiplied_black(alpha) & COLOUR_MASK,
                0,
                "alpha {alpha} leaked colour"
            );
        }
    }

    #[test]
    fn a_depth_32_true_color_visual_is_chosen() {
        assert_eq!(choose_argb_visual(&[opaque(1), argb(7)]), Some(7));
    }

    /// The one that would black out a screen. A depth-24 visual is what the root
    /// window uses and what a naive `create_window` inherits; it has no alpha
    /// channel, so the overlay is drawn opaque and the monitor goes dark with no
    /// error anywhere.
    #[test]
    fn a_server_with_no_argb_visual_gets_no_overlay_rather_than_an_opaque_one() {
        assert_eq!(choose_argb_visual(&[opaque(1), opaque(2)]), None);
    }

    /// `DirectColor` has the same depth and different semantics: its channels are
    /// colormap indices, so a pixel value means nothing until a ramp is loaded.
    #[test]
    fn a_non_true_color_visual_is_refused() {
        let direct = VisualCandidate {
            true_color: false,
            ..argb(3)
        };
        assert_eq!(choose_argb_visual(&[direct]), None);
    }

    /// A visual whose colour channels do not cover exactly the low 24 bits leaves
    /// no clean alpha byte, so the fill pixel would not address the alpha channel.
    #[test]
    fn a_visual_whose_masks_do_not_leave_an_alpha_byte_is_refused() {
        let short = VisualCandidate {
            blue_mask: 0x0000_003F,
            ..argb(4)
        };
        let overlapping = VisualCandidate {
            green_mask: 0x0000_FFFF,
            ..argb(5)
        };
        let spilling = VisualCandidate {
            red_mask: 0xFF00_0000,
            ..argb(6)
        };

        assert_eq!(choose_argb_visual(&[short]), None);
        assert_eq!(choose_argb_visual(&[overlapping]), None);
        assert_eq!(choose_argb_visual(&[spilling]), None);
    }

    #[test]
    fn no_visuals_at_all_is_not_a_panic() {
        assert_eq!(choose_argb_visual(&[]), None);
    }

    #[test]
    fn an_ordinary_monitor_converts_to_x11_geometry() {
        assert_eq!(
            x11_rect(DisplayBounds::new(1920, 0, 2560, 1440)),
            Some((1920, 0, 2560, 1440))
        );
    }

    /// A monitor left of or above the primary sits at negative coordinates, and
    /// X11 positions are signed, so this is ordinary rather than exceptional.
    #[test]
    fn a_negative_origin_converts() {
        assert_eq!(
            x11_rect(DisplayBounds::new(-2560, -400, 2560, 1440)),
            Some((-2560, -400, 2560, 1440))
        );
    }

    /// The protocol limit. Doing this with `as` would wrap the overlay to the
    /// far side of the desktop, covering a display the user did not dim.
    #[test]
    fn an_origin_past_the_protocol_limit_is_refused_rather_than_wrapped() {
        assert!(x11_rect(DisplayBounds::new(40_000, 0, 1920, 1080)).is_none());
        assert!(x11_rect(DisplayBounds::new(0, -40_000, 1920, 1080)).is_none());
    }

    /// The origin fits and the far edge does not. Truncating would leave part of
    /// the monitor undimmed with nothing to explain it.
    #[test]
    fn a_rectangle_whose_far_edge_overflows_is_refused() {
        assert!(x11_rect(DisplayBounds::new(30_000, 0, 4_000, 1080)).is_none());
        assert!(x11_rect(DisplayBounds::new(0, 30_000, 1920, 4_000)).is_none());
        // And the boundary itself is fine.
        assert!(x11_rect(DisplayBounds::new(30_000, 0, 2_767, 1080)).is_some());
    }

    /// A size past 65535 cannot be described at all.
    #[test]
    fn a_size_past_the_protocol_limit_is_refused() {
        assert!(x11_rect(DisplayBounds::new(0, 0, 70_000, 1080)).is_none());
    }

    /// Zero area is a `BadValue` from the server and nothing to cover anyway.
    #[test]
    fn a_zero_area_rectangle_is_refused() {
        assert!(x11_rect(DisplayBounds::new(0, 0, 0, 1080)).is_none());
        assert!(x11_rect(DisplayBounds::new(0, 0, 1920, 0)).is_none());
    }

    /// The EWMH value that forbids unredirection. Pinned because it is the
    /// difference between an overlay that dims and one that blacks the screen out
    /// the moment a fullscreen window appears, and nothing else in the tree would
    /// catch a wrong constant.
    #[test]
    fn bypass_compositor_is_the_never_value() {
        assert_eq!(BYPASS_COMPOSITOR_NEVER, 2);
    }
}

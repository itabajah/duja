//! The decisions an X11 overlay window makes, separated from making it.
//!
//! Creating a click-through translucent window is imperative X11 work that no CI
//! lane can run. Three of the choices inside it are not, and each of them fails
//! in a way that is invisible until someone has a screen in front of them:
//!
//! - **Which visual to use.** Get this wrong and the overlay is opaque. There is
//!   no partial failure and no error to report — the window is created, mapped,
//!   and black.
//! - **Where the alpha goes in the pixel.** It is the **top** byte of a 32-bit
//!   value whose other three are the colour. Put it anywhere else and the
//!   overlay is either invisible or a coloured wash.
//! - **Whether the rectangle is even expressible.** X11 window geometry is 16-bit
//!   signed position and 16-bit unsigned size. A desktop wider than 32767 pixels
//!   silently wraps if the conversion is done with `as`.
//!
//! So they live here, as plain arithmetic over plain data, and are tested on all
//! three lanes — the same split [`crate::linux_caps`] and [`crate::linux_outputs`]
//! use, and for the same reason: this module names no `x11rb` type, so it
//! compiles where that crate does not exist.

use duja_core::dimmer::DisplayBounds;

use crate::plan::OverlayOp;

/// The `XFixes` major version that introduced `SetWindowShapeRegion`, which is
/// the whole click-through mechanism on X11.
///
/// Here rather than beside its one caller for the same reason as
/// [`BYPASS_COMPOSITOR_NEVER`]: it is a bare constant whose failure is invisible
/// in both directions — too high refuses sessions that would have worked, too low
/// admits a server that cannot set an input region, and that one is a window that
/// swallows every click.
pub const XFIXES_INPUT_SHAPE_VERSION: u32 = 2;

/// What an overlay op should do, and what the backend should then record.
///
/// The distinction is the whole point. The backend keeps a record of what is on
/// screen, and [`crate::plan`] diffs against it — so an op that is *skipped*
/// must not be recorded, and an op that does something *other* than what it says
/// must be recorded as that other thing. Getting either wrong leaves the record
/// describing a window that does not exist, after which the planner never emits
/// `Create` for that display again and it cannot be dimmed for the rest of the
/// session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// Do nothing, record nothing.
    Nothing,
    /// Do the op, record it as given.
    AsPlanned,
    /// Destroy this display's window instead, and record **that** — because
    /// either there is no window to act on, or the op would put one where this
    /// backend cannot put it.
    ///
    /// Recording a `Destroy` rather than nothing is what makes the diverged case
    /// *recover*: it drops the stale entry, so the next plan emits a fresh
    /// `Create`. Recording nothing would leave the entry in place and re-plan
    /// the same impossible op forever.
    DestroyInstead,
}

/// Decide what one op does, given whether the backend has a window for it and
/// whether the op's rectangle is one this backend can put a surface on.
///
/// Pure, so the rule that keeps the record honest is tested on every lane —
/// unlike the windowing it drives, which no lane can run.
///
/// # Shared by both Linux overlay backends
///
/// The rest of this module is X11's. This function and [`Recorded`] are not: the
/// hazard they exist for is the planner's, so it is identical on a layer surface,
/// and a second copy of it in [`crate::linux_layer`] would be two places for one
/// hard-won rule to be fixed in. What differs between the two is only what makes a
/// rectangle placeable, and that is why it is an argument rather than a call to
/// [`x11_rect`] inside here:
///
/// - **X11** asks whether the rectangle fits 16-bit window geometry
///   ([`x11_rect`]); a display beyond it would otherwise get a *wrapped* window
///   over a monitor the user never dimmed.
/// - **Wayland** asks whether some `wl_output` has exactly that logical rectangle
///   and is not already dimmed, because a layer surface is bound to an output
///   rather than placed on a root window
///   ([`crate::linux_layer::take_output`]).
#[must_use]
pub fn plan_record(op: &OverlayOp, has_window: bool, placeable: bool) -> Recorded {
    match op {
        // Nowhere to put it. Skipping is right — the alternative is a window over
        // a display the user did not dim — and there is no window yet, so there is
        // nothing to record either way, and the next plan will try again.
        OverlayOp::Create { .. } => {
            if placeable {
                Recorded::AsPlanned
            } else {
                Recorded::Nothing
            }
        }
        // Moved somewhere this backend cannot place it, or moved when there is
        // nothing to move.
        OverlayOp::MoveResize { .. } => {
            if has_window && placeable {
                Recorded::AsPlanned
            } else {
                Recorded::DestroyInstead
            }
        }
        OverlayOp::SetAlpha { .. } => {
            if has_window {
                Recorded::AsPlanned
            } else {
                Recorded::DestroyInstead
            }
        }
        OverlayOp::Destroy { .. } => Recorded::AsPlanned,
    }
}

/// `_NET_WM_BYPASS_COMPOSITOR`'s "never bypass" value.
///
/// Every compositing manager unredirects a fullscreen window as a performance
/// optimisation, and an always-on-top fullscreen window is exactly what an
/// overlay is. Unredirected means the X server draws it directly, which ignores
/// the alpha channel — the same solid-black screen the compositor check in
/// [`crate::linux_caps`] exists to prevent, reached past that check. The EWMH
/// answer is this property: 0 means no preference, 1 means the window would like
/// to bypass, and 2 means it must never be bypassed.
///
/// **This is a mitigation, not a guarantee, and the difference matters.** The
/// property is defined for application windows, and a compositor evaluates
/// unredirection against the window it is considering — picom's
/// `unredir-if-possible` unredirects the *whole screen* rather than one window,
/// so a property on the overlay cannot bind that decision. Setting it is the only
/// standard lever there is; whether it is enough is a question for a real session
/// with a real compositor, which is why the QA checklist gates on it and
/// `docs/debt.md` keeps the row open.
pub const BYPASS_COMPOSITOR_NEVER: u32 = 2;

/// The pixel an overlay is filled with for a given alpha byte: black, with the
/// alpha in the top byte.
///
/// The format an X11 compositing manager reads from a 32-bit visual is
/// **premultiplied** ARGB (`PictStandardARGB32`), and this value satisfies that —
/// but for black it is a distinction without a difference, because premultiplied
/// and straight-alpha black are the same bytes. [`crate::linux_caps`] says the
/// same thing from the other direction, about why an uncomposited X session
/// paints an opaque rectangle at every alpha. Naming the format here is a note
/// for anyone who later wants a fill that is *not* black; it is not the reason
/// this function is shaped the way it is.
///
/// What the function is actually for is the byte layout: alpha in the top eight
/// bits, colour in the low 24, matching the visual [`choose_argb_visual`]
/// insists on. Get that wrong and the overlay is invisible or a coloured wash.
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
/// The first match wins. Two visuals that pass can still have *different* colour
/// layouts (RGB and BGR both satisfy every condition), and that is fine here for
/// one reason only: the overlay is filled with black, which is the same bytes in
/// either. A fill that was not black would have to read the masks rather than
/// assume them.
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
    // The far edge is refused too, and this one is a choice rather than a
    // protocol limit: `CreateWindow` takes an `INT16` origin and a `CARD16` size
    // independently and says nothing about their sum. But an overlay whose far
    // edge is past 32767 sits in a region the server's own `BoxRec` (`short x2`)
    // cannot describe, so what it covers stops being predictable. Refusing gives
    // an undimmed monitor; allowing it gives a partly-dimmed one with nothing to
    // explain the seam.
    let right = i32::from(x).checked_add(i32::from(width))?;
    let bottom = i32::from(y).checked_add(i32::from(height))?;
    i16::try_from(right).ok()?;
    i16::try_from(bottom).ok()?;
    Some((x, y, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::id::StableDisplayId;

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

    /// The colour bytes stay zero at every alpha, which is what makes the value
    /// valid as premultiplied ARGB — the format an X11 compositor reads from a
    /// 32-bit visual. For black the two encodings coincide, so this cannot catch
    /// a premultiplication mistake; what it catches is a fill that stopped being
    /// black, which would blend a colour over the screen instead of dimming it.
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

    /// `SetWindowShapeRegion` arrived in `XFixes` 2.0, and it is the entire
    /// click-through mechanism. Too high refuses sessions that would have worked;
    /// too low admits a server that cannot set an input region, which is a window
    /// that swallows every click.
    #[test]
    fn the_input_shape_needs_xfixes_two() {
        assert_eq!(XFIXES_INPUT_SHAPE_VERSION, 2);
    }

    // --- what each op does to the record -------------------------------------

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("DEL", 0xA131, Some(serial)).unwrap()
    }

    fn ordinary() -> DisplayBounds {
        DisplayBounds::new(0, 0, 1920, 1080)
    }

    fn unexpressible() -> DisplayBounds {
        DisplayBounds::new(40_000, 0, 1920, 1080)
    }

    /// What to pass for `placeable` on the two ops that carry no rectangle. The
    /// value cannot matter for them, and
    /// `an_op_with_no_rectangle_ignores_whether_one_is_placeable` is what makes
    /// that true rather than assumed.
    const NO_RECTANGLE: bool = true;

    #[test]
    fn an_ordinary_op_is_done_and_recorded_as_given() {
        let has = true;
        assert_eq!(
            plan_record(
                &OverlayOp::Create {
                    id: id("a"),
                    bounds: ordinary(),
                    alpha: 128
                },
                false,
                x11_rect(ordinary()).is_some()
            ),
            Recorded::AsPlanned
        );
        assert_eq!(
            plan_record(
                &OverlayOp::MoveResize {
                    id: id("a"),
                    bounds: ordinary()
                },
                has,
                x11_rect(ordinary()).is_some()
            ),
            Recorded::AsPlanned
        );
        assert_eq!(
            plan_record(
                &OverlayOp::SetAlpha {
                    id: id("a"),
                    alpha: 64
                },
                has,
                NO_RECTANGLE
            ),
            Recorded::AsPlanned
        );
        assert_eq!(
            plan_record(&OverlayOp::Destroy { id: id("a") }, has, NO_RECTANGLE),
            Recorded::AsPlanned
        );
    }

    /// A create the backend cannot perform must leave **no** entry. Recording it
    /// would make the planner believe a window exists, and it never emits
    /// `Create` for a display it thinks is already covered — so that display
    /// could not be dimmed again for the rest of the session, even after it moved
    /// back into range.
    #[test]
    fn a_create_that_cannot_be_placed_records_nothing() {
        assert_eq!(
            plan_record(
                &OverlayOp::Create {
                    id: id("a"),
                    bounds: unexpressible(),
                    alpha: 128
                },
                false,
                x11_rect(unexpressible()).is_some()
            ),
            Recorded::Nothing
        );
    }

    /// **The recovery case, and the one that is easy to get backwards.** When the
    /// record and the screen have diverged — an entry with no window — doing
    /// nothing and recording nothing leaves the entry in place, so the next plan
    /// emits the same impossible op, forever. Recording a `Destroy` drops the
    /// entry, and the plan after that emits a fresh `Create`.
    #[test]
    fn an_op_on_a_missing_window_records_a_destroy_so_the_next_plan_recreates() {
        assert_eq!(
            plan_record(
                &OverlayOp::MoveResize {
                    id: id("a"),
                    bounds: ordinary()
                },
                false,
                x11_rect(ordinary()).is_some()
            ),
            Recorded::DestroyInstead
        );
        assert_eq!(
            plan_record(
                &OverlayOp::SetAlpha {
                    id: id("a"),
                    alpha: 64
                },
                false,
                NO_RECTANGLE
            ),
            Recorded::DestroyInstead
        );
    }

    /// A window moved somewhere X11 cannot express goes away, and the record says
    /// so. Keeping it would leave an overlay at the old rectangle covering a
    /// display the user has moved on from.
    #[test]
    fn a_move_to_an_unexpressible_rectangle_destroys_instead() {
        assert_eq!(
            plan_record(
                &OverlayOp::MoveResize {
                    id: id("a"),
                    bounds: unexpressible()
                },
                true,
                x11_rect(unexpressible()).is_some()
            ),
            Recorded::DestroyInstead
        );
    }

    /// `placeable` is about a rectangle, and two of the four ops do not carry
    /// one. Asserting they ignore it in **both** directions is what lets the two
    /// backends pass whatever is convenient there — and what would catch a later
    /// edit that started consulting it, which on Wayland would mean an alpha
    /// change destroying a working overlay because no *other* output was free.
    #[test]
    fn an_op_with_no_rectangle_ignores_whether_one_is_placeable() {
        for placeable in [true, false] {
            assert_eq!(
                plan_record(
                    &OverlayOp::SetAlpha {
                        id: id("a"),
                        alpha: 64
                    },
                    true,
                    placeable
                ),
                Recorded::AsPlanned
            );
            assert_eq!(
                plan_record(&OverlayOp::Destroy { id: id("a") }, true, placeable),
                Recorded::AsPlanned
            );
        }
    }

    /// A destroy is always itself, including for a display with no window: the
    /// backend's destroy is a no-op there, and recording it drops any stale entry.
    #[test]
    fn a_destroy_is_always_recorded_even_with_no_window() {
        assert_eq!(
            plan_record(&OverlayOp::Destroy { id: id("a") }, false, NO_RECTANGLE),
            Recorded::AsPlanned
        );
    }
}

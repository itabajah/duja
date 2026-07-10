//! The pure diffing kernel: turn a desired dimming state into the minimal set
//! of overlay-window operations, given what is currently on screen.
//!
//! This is the TDD heart of the crate. It is deliberately free of any OS type —
//! it speaks only [`duja_core`] vocabulary and the [`OverlayOp`] verbs the
//! Windows backend executes — so it compiles and is exhaustively tested on every
//! target. The backend keeps a [`Vec<OverlayEntry>`] describing the overlays it
//! is showing, calls [`plan_transition`] with the caller's desired
//! [`DimCommand`]s, executes the returned ops, and folds them back into its
//! state.
//!
//! # Rules
//!
//! For each display id, comparing current vs desired:
//! - present in desired with alpha `> 0`, absent from current ⇒ [`OverlayOp::Create`];
//! - present in both, bounds changed ⇒ [`OverlayOp::MoveResize`] (emitted before
//!   an alpha change so a resized overlay never flashes at the old size);
//! - present in both, alpha changed ⇒ [`OverlayOp::SetAlpha`];
//! - present in current, absent from desired (or desired alpha `0`) ⇒
//!   [`OverlayOp::Destroy`];
//! - desired alpha `0` and not currently shown ⇒ nothing (no empty overlay).
//!
//! Alpha is compared on its quantized 0..=255 byte value (what
//! `SetLayeredWindowAttributes` actually takes), so sub-quantum float jitter
//! never emits a redundant op.

use duja_core::continuum::MAX_ALPHA;
use duja_core::dimmer::{DimCommand, DisplayBounds, clamp_alpha};
use duja_core::id::StableDisplayId;

/// The overlay Duja is currently showing for one display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayEntry {
    /// Which display this overlay covers.
    pub id: StableDisplayId,
    /// The overlay's current bounds.
    pub bounds: DisplayBounds,
    /// The overlay's current quantized alpha byte (`1..=255`; an entry is only
    /// ever recorded for a *visible* overlay).
    pub alpha: u8,
}

/// One operation the backend performs on an overlay window.
///
/// Ops are emitted in a safe order per display (move before alpha) and grouped
/// so all creations/updates precede unrelated destructions is *not* required —
/// each op names its display id, so they are independent across displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayOp {
    /// Create a new overlay window for `id` at `bounds` with `alpha`.
    Create {
        /// Target display.
        id: StableDisplayId,
        /// Where to place the new overlay.
        bounds: DisplayBounds,
        /// Initial quantized alpha (`1..=255`).
        alpha: u8,
    },
    /// Move/resize the existing overlay for `id` to `bounds`.
    MoveResize {
        /// Target display.
        id: StableDisplayId,
        /// The new bounds.
        bounds: DisplayBounds,
    },
    /// Change the existing overlay's alpha to `alpha` (`1..=255`).
    SetAlpha {
        /// Target display.
        id: StableDisplayId,
        /// The new quantized alpha.
        alpha: u8,
    },
    /// Destroy the overlay for `id` (desired alpha fell to zero, or the display
    /// left the desired set).
    Destroy {
        /// Target display.
        id: StableDisplayId,
    },
}

/// Quantize a sanitized overlay alpha in `0.0..=`[`MAX_ALPHA`] to the `0..=255`
/// byte `SetLayeredWindowAttributes` expects.
///
/// `0.0` maps to `0` (no overlay) and [`MAX_ALPHA`] to its exact byte; the input
/// is clamped first, so any value — including `NaN` — is total and in range.
#[must_use]
pub fn quantize_alpha(alpha: f32) -> u8 {
    let clamped = clamp_alpha(alpha);
    // clamped ∈ [0, MAX_ALPHA] ⊆ [0, 1]; * 255 ∈ [0, 255]; round then cast is
    // lossless and cannot overflow u8.
    let scaled = (clamped * 255.0).round();
    // RATIONALE (clippy::cast_possible_truncation / cast_sign_loss): `scaled` is
    // a rounded value in [0.0, 255.0] (MAX_ALPHA ≤ 1.0), so the cast to u8 is
    // exact and never truncates or loses a sign.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        scaled as u8
    }
}

/// The largest alpha byte an overlay can carry, from [`MAX_ALPHA`]. Exposed so
/// backends and tests share the exact ceiling the continuum produces.
#[must_use]
pub fn max_alpha_byte() -> u8 {
    quantize_alpha(MAX_ALPHA)
}

/// Diff the desired dimming state against the current overlays, returning the
/// minimal ordered list of [`OverlayOp`]s that transforms current into desired.
///
/// `current` is the backend's view of the overlays on screen (visible only);
/// `desired` is the caller's full [`DimCommand`] set. Commands are sanitized and
/// alpha-quantized internally, so callers may pass raw continuum output. A
/// duplicate id in `desired` keeps the **last** occurrence (a later command for
/// the same display wins), matching a "full desired state" apply.
#[must_use]
pub fn plan_transition(current: &[OverlayEntry], desired: &[DimCommand]) -> Vec<OverlayOp> {
    let mut ops = Vec::new();

    // Walk current entries in their given order, deciding update-or-destroy.
    for entry in current {
        match find_last(desired, &entry.id) {
            Some(cmd) => {
                let alpha = quantize_alpha(cmd.overlay_alpha);
                if alpha == 0 {
                    ops.push(OverlayOp::Destroy {
                        id: entry.id.clone(),
                    });
                    continue;
                }
                if cmd.bounds != entry.bounds {
                    ops.push(OverlayOp::MoveResize {
                        id: entry.id.clone(),
                        bounds: cmd.bounds,
                    });
                }
                if alpha != entry.alpha {
                    ops.push(OverlayOp::SetAlpha {
                        id: entry.id.clone(),
                        alpha,
                    });
                }
            }
            None => ops.push(OverlayOp::Destroy {
                id: entry.id.clone(),
            }),
        }
    }

    // Walk desired for ids not already on screen: create the visible ones. Skip
    // a desired id that appears earlier as a duplicate (only the last wins) and
    // any whose last occurrence resolves to a different, zero, command.
    for (i, cmd) in desired.iter().enumerate() {
        if is_current(current, &cmd.id) {
            continue;
        }
        if !is_last_occurrence(desired, i) {
            continue;
        }
        let alpha = quantize_alpha(cmd.overlay_alpha);
        if alpha == 0 {
            continue;
        }
        ops.push(OverlayOp::Create {
            id: cmd.id.clone(),
            bounds: cmd.bounds,
            alpha,
        });
    }

    ops
}

/// The last command in `desired` targeting `id`, if any (later wins).
fn find_last<'a>(desired: &'a [DimCommand], id: &StableDisplayId) -> Option<&'a DimCommand> {
    desired.iter().rev().find(|c| &c.id == id)
}

/// Whether `current` holds an overlay for `id`.
fn is_current(current: &[OverlayEntry], id: &StableDisplayId) -> bool {
    current.iter().any(|e| &e.id == id)
}

/// Whether index `i` is the last occurrence of its id within `desired`.
fn is_last_occurrence(desired: &[DimCommand], i: usize) -> bool {
    let Some(cmd) = desired.get(i) else {
        return false;
    };
    !desired
        .iter()
        .skip(i.saturating_add(1))
        .any(|c| c.id == cmd.id)
}

/// Fold an executed op list back into a current-overlay list, producing the new
/// state the backend should record. Pure, so the backend's bookkeeping is
/// testable without any window.
///
/// Applying `apply_ops(current, &plan_transition(current, desired))` yields the
/// canonical visible-overlay state for `desired` — the round-trip property the
/// tests assert.
#[must_use]
pub fn apply_ops(current: &[OverlayEntry], ops: &[OverlayOp]) -> Vec<OverlayEntry> {
    let mut next: Vec<OverlayEntry> = current.to_vec();
    for op in ops {
        match op {
            OverlayOp::Create { id, bounds, alpha } => {
                next.push(OverlayEntry {
                    id: id.clone(),
                    bounds: *bounds,
                    alpha: *alpha,
                });
            }
            OverlayOp::MoveResize { id, bounds } => {
                if let Some(e) = next.iter_mut().find(|e| &e.id == id) {
                    e.bounds = *bounds;
                }
            }
            OverlayOp::SetAlpha { id, alpha } => {
                if let Some(e) = next.iter_mut().find(|e| &e.id == id) {
                    e.alpha = *alpha;
                }
            }
            OverlayOp::Destroy { id } => {
                next.retain(|e| &e.id != id);
            }
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("AAA", 0x0001, Some(serial)).unwrap()
    }

    fn bounds(x: i32) -> DisplayBounds {
        DisplayBounds::new(x, 0, 1920, 1080)
    }

    fn cmd(serial: &str, x: i32, alpha: f32) -> DimCommand {
        DimCommand::new(id(serial), bounds(x), alpha, None)
    }

    fn entry(serial: &str, x: i32, alpha: u8) -> OverlayEntry {
        OverlayEntry {
            id: id(serial),
            bounds: bounds(x),
            alpha,
        }
    }

    /// The canonical round-trip: applying the plan reproduces the desired
    /// visible-overlay state, regardless of the starting point.
    fn assert_reaches(current: &[OverlayEntry], desired: &[DimCommand]) {
        let ops = plan_transition(current, desired);
        let next = apply_ops(current, &ops);

        // Every visible desired command has a matching entry, and vice versa.
        let mut expected: Vec<(StableDisplayId, DisplayBounds, u8)> = Vec::new();
        for (i, c) in desired.iter().enumerate() {
            if !is_last_occurrence(desired, i) {
                continue;
            }
            let a = quantize_alpha(c.overlay_alpha);
            if a > 0 {
                expected.push((c.id.clone(), c.bounds, a));
            }
        }
        assert_eq!(next.len(), expected.len(), "overlay count mismatch");
        for (eid, ebounds, ealpha) in expected {
            let found = next
                .iter()
                .find(|e| e.id == eid)
                .expect("desired overlay present after transition");
            assert_eq!(found.bounds, ebounds, "bounds for {eid}");
            assert_eq!(found.alpha, ealpha, "alpha for {eid}");
        }
    }

    #[test]
    fn empty_to_empty_is_noop() {
        assert!(plan_transition(&[], &[]).is_empty());
    }

    #[test]
    fn create_for_new_visible_display() {
        let ops = plan_transition(&[], &[cmd("a", 0, 0.5)]);
        assert!(matches!(ops.as_slice(), [OverlayOp::Create { .. }]));
        assert_reaches(&[], &[cmd("a", 0, 0.5)]);
    }

    #[test]
    fn zero_alpha_new_display_creates_nothing() {
        let ops = plan_transition(&[], &[cmd("a", 0, 0.0)]);
        assert!(ops.is_empty());
    }

    #[test]
    fn alpha_change_emits_set_alpha_only() {
        let cur = vec![entry("a", 0, 128)];
        let ops = plan_transition(&cur, &[cmd("a", 0, MAX_ALPHA)]);
        match ops.as_slice() {
            [OverlayOp::SetAlpha { id: i, alpha }] => {
                assert_eq!(*i, id("a"));
                assert_eq!(*alpha, max_alpha_byte());
            }
            other => panic!("expected one SetAlpha, got {other:?}"),
        }
    }

    #[test]
    fn unchanged_alpha_and_bounds_emits_nothing() {
        let a = quantize_alpha(0.5);
        let cur = vec![entry("a", 0, a)];
        let ops = plan_transition(&cur, &[cmd("a", 0, 0.5)]);
        assert!(
            ops.is_empty(),
            "no-op transition must be empty, got {ops:?}"
        );
    }

    #[test]
    fn sub_quantum_jitter_is_not_an_op() {
        let a = quantize_alpha(0.5);
        let cur = vec![entry("a", 0, a)];
        // A float nudge too small to change the quantized byte.
        let ops = plan_transition(&cur, &[cmd("a", 0, 0.5 + 0.0005)]);
        assert!(ops.is_empty(), "sub-quantum jitter emitted {ops:?}");
    }

    #[test]
    fn bounds_change_moves_before_alpha() {
        let cur = vec![entry("a", 0, 100)];
        let ops = plan_transition(&cur, &[cmd("a", 500, MAX_ALPHA)]);
        assert!(matches!(
            ops.as_slice(),
            [OverlayOp::MoveResize { .. }, OverlayOp::SetAlpha { .. }]
        ));
    }

    #[test]
    fn bounds_change_only_emits_move_only() {
        let a = quantize_alpha(0.5);
        let cur = vec![entry("a", 0, a)];
        let ops = plan_transition(&cur, &[cmd("a", 500, 0.5)]);
        assert!(matches!(ops.as_slice(), [OverlayOp::MoveResize { .. }]));
    }

    #[test]
    fn removed_from_desired_destroys() {
        let cur = vec![entry("a", 0, 100)];
        let ops = plan_transition(&cur, &[]);
        assert_eq!(ops, vec![OverlayOp::Destroy { id: id("a") }]);
        assert_reaches(&cur, &[]);
    }

    #[test]
    fn zero_alpha_destroys_existing() {
        let cur = vec![entry("a", 0, 100)];
        let ops = plan_transition(&cur, &[cmd("a", 0, 0.0)]);
        assert_eq!(ops, vec![OverlayOp::Destroy { id: id("a") }]);
        assert_reaches(&cur, &[cmd("a", 0, 0.0)]);
    }

    #[test]
    fn mixed_add_move_alpha_remove_batch() {
        // a: exists, alpha change; b: exists, moves; c: exists, removed;
        // d: new, created.
        let cur = vec![entry("a", 0, 80), entry("b", 100, 120), entry("c", 200, 60)];
        let desired = vec![
            cmd("a", 0, MAX_ALPHA),
            cmd("b", 999, 0.47),
            cmd("d", 300, 0.6),
        ];
        assert_reaches(&cur, &desired);
    }

    #[test]
    fn duplicate_desired_last_wins() {
        // Two commands for "a": the last (visible, moved) is authoritative.
        let desired = vec![cmd("a", 0, 0.0), cmd("a", 700, 0.6)];
        let ops = plan_transition(&[], &desired);
        match ops.as_slice() {
            [
                OverlayOp::Create {
                    id: i, bounds: b, ..
                },
            ] => {
                assert_eq!(*i, id("a"));
                assert_eq!(*b, bounds(700));
            }
            other => panic!("expected single Create, got {other:?}"),
        }
        assert_reaches(&[], &desired);
    }

    #[test]
    fn duplicate_desired_resolving_to_zero_creates_nothing() {
        let desired = vec![cmd("a", 0, 0.6), cmd("a", 0, 0.0)];
        let ops = plan_transition(&[], &desired);
        assert!(
            ops.is_empty(),
            "last-wins zero must create nothing: {ops:?}"
        );
    }

    #[test]
    fn idempotent_reapply_is_noop() {
        let desired = vec![cmd("a", 0, 0.5), cmd("b", 200, MAX_ALPHA)];
        let ops = plan_transition(&[], &desired);
        let settled = apply_ops(&[], &ops);
        let again = plan_transition(&settled, &desired);
        assert!(
            again.is_empty(),
            "re-applying identical state emitted {again:?}"
        );
    }

    #[test]
    fn quantize_endpoints() {
        assert_eq!(quantize_alpha(0.0), 0);
        assert_eq!(quantize_alpha(-1.0), 0);
        assert_eq!(quantize_alpha(f32::NAN), 0);
        assert_eq!(quantize_alpha(1.0), max_alpha_byte());
        assert_eq!(quantize_alpha(f32::INFINITY), max_alpha_byte());
        // MAX_ALPHA = 0.88 -> round(224.4) = 224.
        assert_eq!(max_alpha_byte(), 224);
    }
}

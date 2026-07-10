//! The cross-platform software-dimming vocabulary.
//!
//! Below a display's hardware brightness floor Duja dims in software (ADR-0003:
//! overlay-first). This module defines the *declarative* order the OS backend
//! executes — one [`DimCommand`] per display describing the desired overlay
//! opacity (and, on the opt-in path, a gamma factor) — and the [`Dimmer`] trait
//! the backend implements. The backend **diffs** the desired state against what
//! it currently shows; callers therefore always pass the *full* set of displays
//! that should be dimmed, and an absent display means "no overlay".
//!
//! Nothing here is OS-specific: the types are plain data and the trait is a
//! vocabulary, so the whole module compiles and is unit-tested on every target.
//! The Windows overlay backend lives in the `duja-dimmer` crate.
//!
//! # Totality
//!
//! Alpha and gamma arrive from the [`continuum`](crate::continuum) mapping and
//! are already in range, but a backend must never trust that: every value is
//! run through [`clamp_alpha`] / [`clamp_gamma`] (both total — they map `NaN`
//! and infinities to safe values and never panic) before it reaches an OS call.
//! [`DimCommand::sanitized`] applies both in one step.

// RATIONALE: the vocabulary intentionally namespaces its types (DimCommand /
// DimmerError) within the `dimmer` module; the names are fixed by the plan and
// read best fully qualified at call sites.
#![allow(clippy::module_name_repetitions)]

use crate::continuum::MAX_ALPHA;
use crate::id::StableDisplayId;

/// The lowest gamma scale Duja will drive on the opt-in gamma path.
///
/// A gamma factor multiplies the ramp slope; driving it toward zero crushes the
/// image to black and — because Windows gamma persists after process death — a
/// too-dark ramp left by a crash is hard to recover from without a reboot. The
/// floor keeps even a fully-engaged gamma ramp legible. The overlay path (the
/// default) reaches true black instead and is unaffected by this bound.
pub const GAMMA_FLOOR: f32 = 0.3;

/// Physical-pixel bounds of one display in the virtual desktop.
///
/// The origin can be negative: a monitor left of, or above, the primary sits at
/// negative virtual-desktop coordinates. Width and height are unsigned because a
/// zero-or-negative extent is not a display; a backend sizes an overlay window
/// directly from these fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayBounds {
    /// Left edge in virtual-desktop pixels (may be negative).
    pub x: i32,
    /// Top edge in virtual-desktop pixels (may be negative).
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl DisplayBounds {
    /// Construct bounds from an origin and size.
    #[must_use]
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        DisplayBounds {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether the bounds enclose at least one pixel.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// One display's desired software-dimming order.
///
/// `overlay_alpha` is the opacity of the black overlay in `0.0..=`[`MAX_ALPHA`]
/// (0 = no overlay, so the backend removes any window it is showing for this
/// display). `gamma` is the opt-in gamma scale in [`GAMMA_FLOOR`]`..=1.0`, or
/// `None` for the default overlay-only path — the Windows overlay backend
/// ignores it and gamma is driven through a separate, explicit API (a crashed
/// gamma ramp persists, so it is never engaged implicitly by a batch apply).
///
/// The fields are public for ergonomic construction in callers and tests;
/// backends must call [`sanitized`](Self::sanitized) (or the [`new`](Self::new)
/// constructor) so out-of-range values never reach an OS call.
#[derive(Debug, Clone, PartialEq)]
pub struct DimCommand {
    /// Which display this order targets.
    pub id: StableDisplayId,
    /// Where the display sits in the virtual desktop.
    pub bounds: DisplayBounds,
    /// Overlay opacity, `0.0..=`[`MAX_ALPHA`] (0 = no overlay).
    pub overlay_alpha: f32,
    /// Opt-in gamma scale, [`GAMMA_FLOOR`]`..=1.0`, or `None` for overlay-only.
    pub gamma: Option<f32>,
}

impl DimCommand {
    /// Build a command, clamping `overlay_alpha` and `gamma` into range.
    #[must_use]
    pub fn new(
        id: StableDisplayId,
        bounds: DisplayBounds,
        overlay_alpha: f32,
        gamma: Option<f32>,
    ) -> Self {
        DimCommand {
            id,
            bounds,
            overlay_alpha: clamp_alpha(overlay_alpha),
            gamma: gamma.map(clamp_gamma),
        }
    }

    /// A copy with `overlay_alpha` and `gamma` clamped into their valid ranges.
    ///
    /// Idempotent, total, and never-panicking: a backend runs every command
    /// through this before touching the OS, so a `NaN`, a negative alpha, or a
    /// gamma below the safety floor can never reach a Win32 call.
    #[must_use]
    pub fn sanitized(&self) -> Self {
        DimCommand {
            id: self.id.clone(),
            bounds: self.bounds,
            overlay_alpha: clamp_alpha(self.overlay_alpha),
            gamma: self.gamma.map(clamp_gamma),
        }
    }

    /// Whether this command asks for any visible overlay (alpha above zero).
    ///
    /// Compares the *sanitized* alpha, so a `NaN` or negative input reads as
    /// "no overlay" rather than accidentally engaging one.
    #[must_use]
    pub fn has_overlay(&self) -> bool {
        clamp_alpha(self.overlay_alpha) > 0.0
    }
}

/// Clamp an overlay alpha into `0.0..=`[`MAX_ALPHA`], totally.
///
/// `NaN` maps to `0.0` (no overlay — the safe default); `+∞` clamps to
/// [`MAX_ALPHA`] and `-∞` to `0.0`. Never panics.
#[must_use]
pub fn clamp_alpha(alpha: f32) -> f32 {
    if alpha.is_nan() {
        return 0.0;
    }
    alpha.clamp(0.0, MAX_ALPHA)
}

/// Clamp a gamma factor into [`GAMMA_FLOOR`]`..=1.0`, totally.
///
/// `NaN` maps to `1.0` (identity — never darkens); values below the floor clamp
/// up to [`GAMMA_FLOOR`] and values above `1.0` clamp down to `1.0`. Never
/// panics.
#[must_use]
pub fn clamp_gamma(factor: f32) -> f32 {
    if factor.is_nan() {
        return 1.0;
    }
    factor.clamp(GAMMA_FLOOR, 1.0)
}

/// A software-dimming backend: applies a full desired state declaratively.
///
/// [`apply`](Dimmer::apply) receives the complete set of displays that should
/// be dimmed *right now*; the implementation diffs it against what it is showing
/// and creates, moves, re-alphas, or removes overlays to match.
/// [`clear`](Dimmer::clear) removes every overlay and restores identity gamma —
/// the state a clean shutdown (or `duja-app --restore`) must leave behind.
///
/// Implementors are [`Send`] (the backend usually owns a worker thread) and
/// [`Debug`](std::fmt::Debug).
pub trait Dimmer: Send + std::fmt::Debug {
    /// Apply the full desired dimming state, diffing against the current one.
    ///
    /// `commands` is authoritative: a display present with `overlay_alpha > 0`
    /// gets (or keeps) an overlay at that opacity and position; a display absent
    /// from `commands`, or present with alpha `0`, has its overlay removed.
    ///
    /// # Errors
    /// Returns [`DimmerError`] if the backend cannot realise the state (an OS
    /// windowing call failed, or the worker thread is gone).
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError>;

    /// Remove every overlay and restore identity gamma on all touched displays.
    ///
    /// # Errors
    /// Returns [`DimmerError`] if teardown could not be completed.
    fn clear(&mut self) -> Result<(), DimmerError>;
}

/// A failure applying or clearing software dimming.
///
/// The OS variant carries a human-readable description rather than a
/// platform-specific error type, so the surface is identical on every target.
#[derive(Debug, thiserror::Error)]
pub enum DimmerError {
    /// The backend's worker thread has stopped or its command channel is closed.
    #[error("the dimmer backend thread is not running")]
    Backend,
    /// An OS windowing or gamma call failed.
    #[error("an OS dimming call failed: {0}")]
    Os(String),
    /// Software dimming is not available on this platform build.
    #[error("software dimming is not supported on this platform")]
    Unsupported,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> StableDisplayId {
        StableDisplayId::from_parts("AAA", 0x0001, Some("unit")).unwrap()
    }

    #[test]
    fn clamp_alpha_keeps_in_range_values() {
        assert!((clamp_alpha(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((clamp_alpha(0.5) - 0.5).abs() < f32::EPSILON);
        assert!((clamp_alpha(MAX_ALPHA) - MAX_ALPHA).abs() < f32::EPSILON);
    }

    #[test]
    fn clamp_alpha_bounds_out_of_range() {
        assert!((clamp_alpha(-1.0) - 0.0).abs() < f32::EPSILON);
        assert!((clamp_alpha(2.0) - MAX_ALPHA).abs() < f32::EPSILON);
        assert!((clamp_alpha(f32::INFINITY) - MAX_ALPHA).abs() < f32::EPSILON);
        assert!((clamp_alpha(f32::NEG_INFINITY) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn clamp_alpha_maps_nan_to_zero() {
        assert!((clamp_alpha(f32::NAN) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn clamp_gamma_bounds_and_defaults() {
        assert!((clamp_gamma(1.0) - 1.0).abs() < f32::EPSILON);
        assert!((clamp_gamma(0.5) - 0.5).abs() < f32::EPSILON);
        assert!((clamp_gamma(0.0) - GAMMA_FLOOR).abs() < f32::EPSILON);
        assert!((clamp_gamma(-5.0) - GAMMA_FLOOR).abs() < f32::EPSILON);
        assert!((clamp_gamma(2.0) - 1.0).abs() < f32::EPSILON);
        assert!((clamp_gamma(f32::NAN) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dim_command_new_clamps_both_channels() {
        let c = DimCommand::new(id(), DisplayBounds::new(0, 0, 100, 100), 9.0, Some(0.0));
        assert!((c.overlay_alpha - MAX_ALPHA).abs() < f32::EPSILON);
        assert_eq!(c.gamma, Some(GAMMA_FLOOR));
    }

    #[test]
    fn dim_command_sanitized_is_idempotent() {
        let raw = DimCommand {
            id: id(),
            bounds: DisplayBounds::new(-10, -20, 800, 600),
            overlay_alpha: f32::NAN,
            gamma: Some(f32::NAN),
        };
        let once = raw.sanitized();
        let twice = once.sanitized();
        assert_eq!(once, twice);
        assert!((once.overlay_alpha - 0.0).abs() < f32::EPSILON);
        assert_eq!(once.gamma, Some(1.0));
    }

    #[test]
    fn has_overlay_reads_sanitized_alpha() {
        let visible = DimCommand::new(id(), DisplayBounds::new(0, 0, 1, 1), 0.4, None);
        let hidden = DimCommand::new(id(), DisplayBounds::new(0, 0, 1, 1), 0.0, None);
        let nanned = DimCommand {
            id: id(),
            bounds: DisplayBounds::new(0, 0, 1, 1),
            overlay_alpha: f32::NAN,
            gamma: None,
        };
        assert!(visible.has_overlay());
        assert!(!hidden.has_overlay());
        assert!(!nanned.has_overlay());
    }

    #[test]
    fn display_bounds_empty_detects_zero_extent() {
        assert!(DisplayBounds::new(0, 0, 0, 100).is_empty());
        assert!(DisplayBounds::new(0, 0, 100, 0).is_empty());
        assert!(!DisplayBounds::new(0, 0, 1, 1).is_empty());
    }

    #[test]
    fn dimmer_error_is_display_and_debug() {
        let e = DimmerError::Os("boom".to_owned());
        assert!(e.to_string().contains("boom"));
        assert!(format!("{e:?}").contains("Os"));
        assert!(DimmerError::Backend.to_string().contains("not running"));
        assert!(
            DimmerError::Unsupported
                .to_string()
                .contains("not supported")
        );
    }
}

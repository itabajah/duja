//! The brightness continuum: one user slider mapped onto hardware backlight
//! plus software dimming (overlay alpha or gamma), with a seamless handoff at
//! the hardware floor.
//!
//! This is the product's differentiator and the purest TDD target. The mapping
//! is a pure function of `(user_pct, config)` — no I/O, no clock.

// RATIONALE: the domain vocabulary intentionally namespaces its types
// (ContinuumConfig / ContinuumOutput); these names are frozen by the plan and
// read better fully qualified at call sites than shortened.
#![allow(clippy::module_name_repetitions)]

use crate::model::DimMode;

/// The maximum overlay opacity applied at user level 0.
///
/// Chosen below 1.0 so a fully-dimmed screen is near-black but never fully
/// opaque by default (the user can always still see the screen and Duja's UI).
pub const MAX_ALPHA: f32 = 0.88;

/// Per-display configuration for the brightness continuum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuumConfig {
    /// The hardware brightness floor, or `None` for a software-only display.
    ///
    /// `Some(floor)`: the lowest hardware percentage Duja will drive; below it,
    /// software dimming takes over. `None`: the display has no hardware
    /// brightness range at all, so [`ContinuumOutput::hardware_pct`] is always
    /// `None` and software dimming spans the whole slider.
    pub hardware_floor: Option<u8>,
    /// How sub-floor dimming is realised.
    pub mode: DimMode,
}

impl ContinuumConfig {
    /// A hardware-backed display with the given floor and dim mode.
    #[must_use]
    pub fn hardware(floor_pct: u8, mode: DimMode) -> Self {
        ContinuumConfig {
            hardware_floor: Some(floor_pct),
            mode,
        }
    }

    /// A software-only display (no hardware backlight); overlay/gamma only.
    #[must_use]
    pub fn software_only(mode: DimMode) -> Self {
        ContinuumConfig {
            hardware_floor: None,
            mode,
        }
    }
}

/// The output of the continuum mapping: what to drive on each channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContinuumOutput {
    /// Hardware brightness percentage to write, or `None` for software-only.
    pub hardware_pct: Option<u8>,
    /// Overlay opacity in `[0.0, MAX_ALPHA]` (0 = no overlay).
    pub overlay_alpha: f32,
    /// Gamma scale in `[1.0 - MAX_ALPHA, 1.0]` when [`DimMode::Gamma`] is
    /// engaged below the floor, otherwise `None`.
    pub gamma: Option<f32>,
}

/// Map a unified user brightness level (`0..=100`) to hardware + software
/// dimming, per the display's [`ContinuumConfig`].
///
/// At or above the hardware floor the user level drives the hardware directly
/// (no overlay). Below the floor the hardware pins at the floor and the chosen
/// [`DimMode`] takes over. Values above 100 are clamped.
///
/// # Examples
/// ```
/// use duja_core::continuum::{map_user_level, ContinuumConfig, MAX_ALPHA};
/// use duja_core::model::DimMode;
///
/// let cfg = ContinuumConfig::hardware(30, DimMode::Overlay);
/// // Above the floor: pure hardware.
/// assert_eq!(map_user_level(80, &cfg).hardware_pct, Some(80));
/// // At zero: hardware pinned at the floor, overlay at full strength.
/// let dark = map_user_level(0, &cfg);
/// assert_eq!(dark.hardware_pct, Some(30));
/// assert!((dark.overlay_alpha - MAX_ALPHA).abs() < 1e-6);
/// ```
#[must_use]
pub fn map_user_level(user_pct: u8, cfg: &ContinuumConfig) -> ContinuumOutput {
    let user_u8 = user_pct.min(100);
    let user = f32::from(user_u8);

    let Some(floor_pct) = cfg.hardware_floor else {
        // Software-only: overlay/gamma spans the whole range; hardware is
        // never touched.
        let alpha_full = MAX_ALPHA * (100.0 - user) / 100.0;
        return finish(None, alpha_full, cfg.mode);
    };
    let floor_u8 = floor_pct.min(100);
    let floor = f32::from(floor_u8);

    if user_u8 >= floor_u8 {
        // Pure hardware: the user level maps straight onto the hardware value.
        ContinuumOutput {
            hardware_pct: Some(user_u8),
            overlay_alpha: 0.0,
            gamma: None,
        }
    } else {
        // Below the floor: pin hardware, dim in software. `floor` is >= 1 here
        // (user_u8 < floor_u8), so the division is well-defined.
        let alpha_full = MAX_ALPHA * (floor - user) / floor;
        finish(Some(floor_u8), alpha_full, cfg.mode)
    }
}

/// Apply the dim mode to a computed overlay strength, producing the output for
/// the sub-floor / software-only regime.
fn finish(hardware_pct: Option<u8>, alpha_full: f32, mode: DimMode) -> ContinuumOutput {
    let alpha = alpha_full.clamp(0.0, MAX_ALPHA);
    match mode {
        DimMode::Overlay => ContinuumOutput {
            hardware_pct,
            overlay_alpha: alpha,
            gamma: None,
        },
        DimMode::Gamma => ContinuumOutput {
            hardware_pct,
            overlay_alpha: 0.0,
            gamma: Some((1.0 - alpha).clamp(1.0 - MAX_ALPHA, 1.0)),
        },
        DimMode::Off => ContinuumOutput {
            hardware_pct,
            overlay_alpha: 0.0,
            gamma: None,
        },
    }
}

/// Reflect an externally-observed hardware percentage back to a slider level.
///
/// Above the floor the user slider maps 1:1 onto hardware, so this is the
/// identity there. A reading below the floor (something else drove the panel
/// under Duja's software floor) is reported as the floor — the lowest level
/// Duja represents with pure hardware.
///
/// On a software-only display (`hardware_floor == None`) there is no hardware
/// channel to reflect; the reading is passed through clamped to `0..=100`.
#[must_use]
pub fn reverse_map(hw_pct: u8, cfg: &ContinuumConfig) -> u8 {
    let floor = cfg.hardware_floor.unwrap_or(0).min(100);
    hw_pct.clamp(floor, 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DimMode;
    use proptest::prelude::*;

    /// Effective perceived luminance in `[0, 1]` from a mapping output: the
    /// hardware fraction attenuated by overlay alpha or gamma. Continuity and
    /// monotonicity are asserted against this single scalar.
    fn perceived(out: &ContinuumOutput) -> f32 {
        let hw = out.hardware_pct.map_or(1.0, |p| f32::from(p) / 100.0);
        let soft = match out.gamma {
            Some(g) => g,
            None => 1.0 - out.overlay_alpha,
        };
        hw * soft
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-6
    }

    fn any_mode() -> impl Strategy<Value = DimMode> {
        prop_oneof![
            Just(DimMode::Overlay),
            Just(DimMode::Gamma),
            Just(DimMode::Off),
        ]
    }

    fn any_cfg() -> impl Strategy<Value = ContinuumConfig> {
        (proptest::option::of(0u8..=100u8), any_mode()).prop_map(|(hardware_floor, mode)| {
            ContinuumConfig {
                hardware_floor,
                mode,
            }
        })
    }

    /// A floor in 0..=100 paired with a user level at or above that floor.
    fn floor_and_user() -> impl Strategy<Value = (u8, u8)> {
        (0u8..=100u8).prop_flat_map(|floor| (Just(floor), floor..=100u8))
    }

    // --- concrete unit tests ---

    #[test]
    fn full_brightness_is_pure_hardware() {
        let out = map_user_level(100, &ContinuumConfig::hardware(50, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(100));
        assert!(close(out.overlay_alpha, 0.0));
        assert_eq!(out.gamma, None);
    }

    #[test]
    fn above_floor_maps_directly_to_hardware() {
        let out = map_user_level(75, &ContinuumConfig::hardware(50, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(75));
        assert!(close(out.overlay_alpha, 0.0));
    }

    #[test]
    fn at_floor_overlay_alpha_is_zero() {
        let out = map_user_level(50, &ContinuumConfig::hardware(50, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(50));
        assert!(close(out.overlay_alpha, 0.0));
    }

    #[test]
    fn below_floor_engages_overlay_and_pins_hardware() {
        let out = map_user_level(25, &ContinuumConfig::hardware(50, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(50));
        // alpha = MAX_ALPHA * (50 - 25) / 50 = MAX_ALPHA / 2
        assert!(close(out.overlay_alpha, MAX_ALPHA * 0.5));
        assert_eq!(out.gamma, None);
    }

    #[test]
    fn zero_reaches_max_alpha() {
        let out = map_user_level(0, &ContinuumConfig::hardware(50, DimMode::Overlay));
        assert!(close(out.overlay_alpha, MAX_ALPHA));
        assert_eq!(out.hardware_pct, Some(50));
    }

    #[test]
    fn off_mode_clamps_at_floor_without_overlay() {
        let out = map_user_level(20, &ContinuumConfig::hardware(50, DimMode::Off));
        assert_eq!(out.hardware_pct, Some(50));
        assert!(close(out.overlay_alpha, 0.0));
        assert_eq!(out.gamma, None);
    }

    #[test]
    fn gamma_mode_reduces_gamma_below_floor() {
        let out = map_user_level(0, &ContinuumConfig::hardware(50, DimMode::Gamma));
        assert!(close(out.overlay_alpha, 0.0));
        match out.gamma {
            Some(g) => assert!(close(g, 1.0 - MAX_ALPHA)),
            None => panic!("expected gamma to be engaged"),
        }
    }

    #[test]
    fn software_only_spans_the_full_range_with_overlay() {
        let cfg = ContinuumConfig::software_only(DimMode::Overlay);
        let dark = map_user_level(0, &cfg);
        assert_eq!(dark.hardware_pct, None);
        assert!(close(dark.overlay_alpha, MAX_ALPHA));

        let bright = map_user_level(100, &cfg);
        assert_eq!(bright.hardware_pct, None);
        assert!(close(bright.overlay_alpha, 0.0));
    }

    #[test]
    fn floor_100_pins_hardware_high_and_dims_via_overlay() {
        let out = map_user_level(50, &ContinuumConfig::hardware(100, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(100));
        // alpha = MAX_ALPHA * (100 - 50) / 100 = MAX_ALPHA / 2
        assert!(close(out.overlay_alpha, MAX_ALPHA * 0.5));
    }

    #[test]
    fn reverse_map_is_identity_above_floor() {
        assert_eq!(
            reverse_map(70, &ContinuumConfig::hardware(50, DimMode::Overlay)),
            70
        );
        assert_eq!(
            reverse_map(100, &ContinuumConfig::hardware(50, DimMode::Overlay)),
            100
        );
    }

    #[test]
    fn reverse_map_clamps_below_floor_to_floor() {
        assert_eq!(
            reverse_map(30, &ContinuumConfig::hardware(50, DimMode::Overlay)),
            50
        );
    }

    #[test]
    fn reverse_map_passes_through_on_software_only() {
        let cfg = ContinuumConfig::software_only(DimMode::Overlay);
        assert_eq!(reverse_map(0, &cfg), 0);
        assert_eq!(reverse_map(42, &cfg), 42);
        assert_eq!(reverse_map(255, &cfg), 100);
    }

    #[test]
    fn out_of_range_user_is_clamped() {
        let out = map_user_level(200, &ContinuumConfig::hardware(50, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(100));
        assert!(close(out.overlay_alpha, 0.0));
    }

    // --- property tests (plan §4.2) ---

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Perceived output never decreases as the user level rises.
        #[test]
        fn continuum_monotonic_over_full_range(cfg in any_cfg()) {
            let levels: Vec<f32> = (0u8..=100)
                .map(|u| perceived(&map_user_level(u, &cfg)))
                .collect();
            for pair in levels.windows(2) {
                if let [a, b] = pair {
                    prop_assert!(*b >= *a - 1e-4, "non-monotonic {a} -> {b} for {cfg:?}");
                }
            }
        }

        /// No visible jump in perceived output at the hardware-floor handoff.
        #[test]
        fn continuum_continuous_at_hw_floor(floor in 1u8..=100, mode in any_mode()) {
            let cfg = ContinuumConfig::hardware(floor, mode);
            let at = perceived(&map_user_level(floor, &cfg));
            let below = perceived(&map_user_level(floor.saturating_sub(1), &cfg));
            prop_assert!((at - below).abs() < 0.02, "jump at floor {floor}: {below} -> {at}");
        }

        /// 100% is exactly full brightness for every configuration.
        #[test]
        fn continuum_endpoints_exact(cfg in any_cfg()) {
            let out = map_user_level(100, &cfg);
            prop_assert!(out.overlay_alpha.abs() <= f32::EPSILON);
            prop_assert!((perceived(&out) - 1.0).abs() < 1e-4);
            if cfg.hardware_floor.is_some() {
                prop_assert_eq!(out.hardware_pct, Some(100));
            } else {
                prop_assert_eq!(out.hardware_pct, None);
            }
        }

        /// The darkest overlay level is exactly MAX_ALPHA (hardware pinned).
        #[test]
        fn continuum_dark_endpoint_is_max_alpha(floor in 1u8..=100) {
            let cfg = ContinuumConfig::hardware(floor, DimMode::Overlay);
            let out = map_user_level(0, &cfg);
            prop_assert!((out.overlay_alpha - MAX_ALPHA).abs() <= f32::EPSILON);
            prop_assert_eq!(out.hardware_pct, Some(floor));
        }

        /// Above the floor the hardware value equals the user level, and
        /// reflecting it back reproduces the slider within ±1.
        #[test]
        fn continuum_reverse_map_roundtrip((floor, user) in floor_and_user(), mode in any_mode()) {
            let cfg = ContinuumConfig::hardware(floor, mode);
            prop_assert_eq!(map_user_level(user, &cfg).hardware_pct, Some(user));
            prop_assert!(reverse_map(user, &cfg).abs_diff(user) <= 1);
        }

        /// Software-only displays never touch hardware and stay monotonic.
        #[test]
        fn continuum_degenerate_hw_range(user in 0u8..=100, mode in any_mode()) {
            let cfg = ContinuumConfig::software_only(mode);
            let out = map_user_level(user, &cfg);
            prop_assert_eq!(out.hardware_pct, None);
        }
    }
}

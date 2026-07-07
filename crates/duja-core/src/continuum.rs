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

// ---- specs first (TDD); implementation follows in the next commit ----

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
        (0u8..=100u8, any_mode(), any::<bool>()).prop_map(|(hw_floor_pct, mode, has_hardware)| {
            ContinuumConfig {
                hw_floor_pct,
                mode,
                has_hardware,
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
        assert_eq!(reverse_map(70, &ContinuumConfig::hardware(50, DimMode::Overlay)), 70);
        assert_eq!(reverse_map(100, &ContinuumConfig::hardware(50, DimMode::Overlay)), 100);
    }

    #[test]
    fn reverse_map_clamps_below_floor_to_floor() {
        assert_eq!(reverse_map(30, &ContinuumConfig::hardware(50, DimMode::Overlay)), 50);
    }

    #[test]
    fn out_of_range_user_is_clamped() {
        let out = map_user_level(200, &ContinuumConfig::hardware(50, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(100));
        assert!(close(out.overlay_alpha, 0.0));
    }

    // --- property tests (plan §4.2) ---

    proptest! {
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
            if cfg.has_hardware {
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

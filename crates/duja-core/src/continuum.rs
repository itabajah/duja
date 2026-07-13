//! The brightness continuum: one user slider mapped onto hardware backlight
//! plus software dimming (overlay alpha or gamma), on a **perceptual** scale.
//!
//! This is the product's differentiator and the purest TDD target. The mapping
//! is a pure function of `(user_pct, config)` — no I/O, no clock.
//!
//! # The perceptual scale
//!
//! The slider position **is** perceived brightness: slider 20 means "20% as
//! bright as full", independent of the hardware floor or the panel. A monitor's
//! true luminance range is not knowable over DDC, so each display carries one
//! tunable number — [`min_perceived_pct`](ContinuumConfig::min_perceived_pct),
//! the perceived brightness the panel shows at hardware zero (default handled by
//! config). Call it `m`. Hardware value `h` (0..=100) therefore sits at slider
//! position `pos(h) = m + (100 − m)·h/100`: hardware zero is at slider `m`
//! (**line A**), hardware max at slider 100.
//!
//! The configured hardware floor `f` is a **write limit**, not a scale change:
//! it only bounds how low Duja drives the panel. Its slider position
//! `B = pos(f)` (**line B**) is where hardware hands off to software dimming.
//! Below `B` the hardware pins at `f` and the overlay (or gamma) supplies the
//! missing darkness so perceived brightness still equals the slider position.
//! Changing `f` moves line B but never rescales the slider.

// RATIONALE: the domain vocabulary intentionally namespaces its types
// (ContinuumConfig / ContinuumOutput / SliderGeometry); these names are frozen
// by the plan and read better fully qualified at call sites than shortened.
#![allow(clippy::module_name_repetitions)]

use crate::model::DimMode;

/// The maximum overlay opacity applied at user level 0.
///
/// Chosen below 1.0 so a fully-dimmed screen is near-black but never fully
/// opaque by default (the user can always still see the screen and Duja's UI).
pub const MAX_ALPHA: f32 = 0.88;

/// The largest `min_perceived_pct` the mapping honours. Clamping here keeps
/// `100 − m ≥ 5`, so the hardware section never collapses and the slider→hardware
/// inversion stays well-conditioned. (The settings UI clamps to a tighter, more
/// sensible range; this is the defensive core bound.)
const MAX_MIN_PERCEIVED: u8 = 95;

/// Per-display configuration for the brightness continuum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuumConfig {
    /// The hardware brightness floor, or `None` for a software-only display.
    ///
    /// `Some(floor)`: the lowest hardware percentage Duja will drive; below its
    /// slider position (`B = pos(floor)`), software dimming takes over. `None`:
    /// the display has no hardware brightness range at all, so
    /// [`ContinuumOutput::hardware_pct`] is always `None` and software dimming
    /// spans the whole slider.
    pub hardware_floor: Option<u8>,
    /// Perceived brightness (%) the panel shows at hardware zero — the `m` that
    /// anchors the perceptual scale (see the module docs). Ignored for a
    /// software-only display. Clamped to `..=95` at use.
    pub min_perceived_pct: u8,
    /// How sub-floor dimming is realised.
    pub mode: DimMode,
}

impl ContinuumConfig {
    /// A hardware-backed display with the given floor, perceptual anchor and dim
    /// mode.
    #[must_use]
    pub fn hardware(floor_pct: u8, min_perceived_pct: u8, mode: DimMode) -> Self {
        ContinuumConfig {
            hardware_floor: Some(floor_pct),
            min_perceived_pct,
            mode,
        }
    }

    /// A software-only display (no hardware backlight); overlay/gamma only.
    ///
    /// The perceptual anchor is irrelevant without hardware, so it is fixed at 0
    /// (keeping equality between software-only configs clean).
    #[must_use]
    pub fn software_only(mode: DimMode) -> Self {
        ContinuumConfig {
            hardware_floor: None,
            min_perceived_pct: 0,
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

/// Where the slider's reference lines sit and how low it can go, as fractions of
/// the usable track (`0.0..=1.0`). Consumed by the flyout to draw the markers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliderGeometry {
    /// Fraction at which hardware zero sits (**line A**), or `None` for a
    /// software-only display (no hardware ⇒ no markers).
    pub hw_zero: Option<f32>,
    /// Fraction at which hardware hands off to software dimming (**line B**, the
    /// floor), or `None` for a software-only display. Equals
    /// [`hw_zero`](Self::hw_zero) when the floor is 0 (the two lines coincide).
    pub transition: Option<f32>,
    /// The lowest reachable slider fraction: `0.0` when software dimming can take
    /// the slider to full dark, or the transition fraction when the dim mode is
    /// [`DimMode::Off`] (below the floor is unreachable).
    pub min_usable: f32,
}

/// The slider position (`0.0..=100.0`) of hardware value `h` (`0..=100`) given
/// the perceptual anchor `m`. Hardware zero maps to slider `m`, hardware max to
/// slider 100, linearly between.
fn pos(h: f32, m: f32) -> f32 {
    m + (100.0 - m) * h / 100.0
}

/// Map a unified user brightness level (`0..=100`, = perceived brightness %) to
/// hardware + software dimming, per the display's [`ContinuumConfig`].
///
/// At or above the transition `B = pos(floor)` the slider drives the hardware
/// directly (no overlay). Below `B` the hardware pins at the floor and the
/// chosen [`DimMode`] supplies the rest so perceived brightness still equals the
/// slider. Values above 100 are clamped.
///
/// # Examples
/// ```
/// use duja_core::continuum::{map_user_level, ContinuumConfig, MAX_ALPHA};
/// use duja_core::model::DimMode;
///
/// // Floor 0, perceptual anchor 25: hardware spans slider 25..=100.
/// let cfg = ContinuumConfig::hardware(0, 25, DimMode::Overlay);
/// // Slider 100 is always exactly full hardware.
/// assert_eq!(map_user_level(100, &cfg).hardware_pct, Some(100));
/// // Below the anchor the panel is at hardware zero and the overlay dims.
/// let dim = map_user_level(10, &cfg);
/// assert_eq!(dim.hardware_pct, Some(0));
/// assert!(dim.overlay_alpha > 0.0);
/// ```
#[must_use]
pub fn map_user_level(user_pct: u8, cfg: &ContinuumConfig) -> ContinuumOutput {
    let p_u8 = user_pct.min(100);
    let p = f32::from(p_u8);

    let Some(floor_pct) = cfg.hardware_floor else {
        // Software-only: overlay/gamma spans the whole slider and the mapping is
        // direct — perceived brightness equals the slider position.
        let alpha_full = 1.0 - p / 100.0;
        return finish(None, alpha_full, cfg.mode);
    };
    let floor_u8 = floor_pct.min(100);
    let m = f32::from(cfg.min_perceived_pct.min(MAX_MIN_PERCEIVED));
    let transition = pos(f32::from(floor_u8), m); // slider position of the floor (line B)

    if p >= transition {
        // Above the transition: pure hardware. Invert `pos` to recover the
        // hardware value for this slider position.
        let hardware = if p_u8 >= 100 {
            // Structural endpoint: the top of the slider is always full hardware.
            100
        } else {
            // `100 - m >= 5` (m is clamped to <= 95), so the division is safe.
            round_clamp((p - m) * 100.0 / (100.0 - m), floor_u8, 100)
        };
        ContinuumOutput {
            hardware_pct: Some(hardware),
            overlay_alpha: 0.0,
            gamma: None,
        }
    } else {
        // Below the transition: pin the hardware at the floor and dim in software
        // so perceived brightness still equals the slider. `transition` is > 0
        // here (`p < transition` and `p >= 0` ⇒ `transition > 0`), so the
        // division is well-defined.
        let alpha_full = 1.0 - p / transition;
        finish(Some(floor_u8), alpha_full, cfg.mode)
    }
}

/// Round `x` to the nearest integer and clamp it into `lo..=hi` as a `u8`.
fn round_clamp(x: f32, lo: u8, hi: u8) -> u8 {
    let r = x.round().clamp(f32::from(lo), f32::from(hi));
    // RATIONALE: `r` is in `lo..=hi` ⊆ `0..=100` and integral after `round()`, so
    // the cast cannot truncate a meaningful fraction, lose a sign, or overflow.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = r as u8;
    v
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
/// This is `pos`: an external change (physical buttons, another app) that drove
/// the panel to `hw_pct` reflects to the slider position `pos(hw_pct)`. It is
/// deliberately **not** clamped to the floor — the floor is a *write* policy
/// (how low Duja drives), not a *read* policy, so a reading below the floor
/// reflects truthfully between lines A and B rather than being snapped up.
///
/// On a software-only display (`hardware_floor == None`) there is no hardware
/// channel to reflect; the reading is passed through clamped to `0..=100`.
#[must_use]
pub fn reverse_map(hw_pct: u8, cfg: &ContinuumConfig) -> u8 {
    let hw = hw_pct.min(100);
    // Software-only: no hardware channel to reflect; pass through clamped.
    if cfg.hardware_floor.is_none() {
        return hw;
    }
    let m = f32::from(cfg.min_perceived_pct.min(MAX_MIN_PERCEIVED));
    round_clamp(pos(f32::from(hw), m), 0, 100)
}

/// The slider marker geometry for `cfg` (see [`SliderGeometry`]).
#[must_use]
pub fn geometry(cfg: &ContinuumConfig) -> SliderGeometry {
    let Some(floor_pct) = cfg.hardware_floor else {
        return SliderGeometry {
            hw_zero: None,
            transition: None,
            min_usable: 0.0,
        };
    };
    let m = f32::from(cfg.min_perceived_pct.min(MAX_MIN_PERCEIVED));
    let transition = pos(f32::from(floor_pct.min(100)), m) / 100.0;
    SliderGeometry {
        hw_zero: Some(m / 100.0),
        transition: Some(transition),
        min_usable: if cfg.mode == DimMode::Off {
            transition
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DimMode;
    use proptest::prelude::*;

    /// Effective perceived luminance in `[0, 1]` from a mapping output: the
    /// hardware value's slider position attenuated by overlay alpha or gamma.
    /// The design invariant is `perceived(map_user_level(p, cfg), cfg) == p/100`
    /// (exact until the `MAX_ALPHA` clamp flattens the darkest tail).
    fn perceived(out: &ContinuumOutput, cfg: ContinuumConfig) -> f32 {
        let base = match out.hardware_pct {
            Some(h) => {
                pos(
                    f32::from(h),
                    f32::from(cfg.min_perceived_pct.min(MAX_MIN_PERCEIVED)),
                ) / 100.0
            }
            None => 1.0,
        };
        let soft = out.gamma.unwrap_or(1.0 - out.overlay_alpha);
        base * soft
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
        (proptest::option::of(0u8..=100u8), 0u8..=95u8, any_mode()).prop_map(
            |(hardware_floor, min_perceived_pct, mode)| ContinuumConfig {
                hardware_floor,
                min_perceived_pct,
                mode,
            },
        )
    }

    /// `(m, floor, user)` where the user level is at or above the transition
    /// `B = pos(floor)` — i.e. the pure-hardware side of the slider.
    fn hardware_side_user() -> impl Strategy<Value = (u8, u8, u8)> {
        (0u8..=95u8, 0u8..=100u8).prop_flat_map(|(m, floor)| {
            let b = f32::from(m) + (100.0 - f32::from(m)) * f32::from(floor) / 100.0;
            // RATIONALE: `b` is in `0.0..=100.0`; its ceil lands in range and is
            // integral, so the cast cannot truncate, lose a sign, or overflow.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let lo = (b.ceil() as u8).min(100);
            (Just(m), Just(floor), lo..=100u8)
        })
    }

    // --- geometry ---

    #[test]
    fn geometry_markers_at_floor_zero_coincide() {
        let g = geometry(&ContinuumConfig::hardware(0, 25, DimMode::Overlay));
        assert!(close(g.hw_zero.unwrap(), 0.25));
        assert!(close(g.transition.unwrap(), 0.25));
        assert!(close(g.min_usable, 0.0));
    }

    #[test]
    fn geometry_transition_is_pos_of_floor() {
        // floor 20, m 25 ⇒ B = 25 + 75·0.2 = 40.
        let g = geometry(&ContinuumConfig::hardware(20, 25, DimMode::Overlay));
        assert!(close(g.hw_zero.unwrap(), 0.25));
        assert!(close(g.transition.unwrap(), 0.40));
    }

    #[test]
    fn geometry_software_only_has_no_markers() {
        let g = geometry(&ContinuumConfig::software_only(DimMode::Overlay));
        assert_eq!(g.hw_zero, None);
        assert_eq!(g.transition, None);
        assert!(close(g.min_usable, 0.0));
    }

    #[test]
    fn geometry_min_usable_only_when_dim_off() {
        // Off ⇒ can't go below the transition; Overlay ⇒ can reach full dark.
        let off = geometry(&ContinuumConfig::hardware(20, 25, DimMode::Off));
        assert!(close(off.min_usable, 0.40));
        let overlay = geometry(&ContinuumConfig::hardware(20, 25, DimMode::Overlay));
        assert!(close(overlay.min_usable, 0.0));
    }

    // --- forward mapping ---

    #[test]
    fn slider_100_is_hardware_100_exact() {
        for m in [0u8, 25, 60, 95] {
            for floor in [0u8, 30, 100] {
                let out =
                    map_user_level(100, &ContinuumConfig::hardware(floor, m, DimMode::Overlay));
                assert_eq!(out.hardware_pct, Some(100), "m={m} floor={floor}");
                assert!(close(out.overlay_alpha, 0.0));
                assert_eq!(out.gamma, None);
            }
        }
    }

    #[test]
    fn slider_position_is_perceived_above_transition() {
        // floor 50, m 25 ⇒ B = 62.5; slider 75 ⇒ hardware = round((75-25)·100/75) = 67.
        let cfg = ContinuumConfig::hardware(50, 25, DimMode::Overlay);
        let out = map_user_level(75, &cfg);
        assert_eq!(out.hardware_pct, Some(67));
        assert!(close(out.overlay_alpha, 0.0));
        assert!((perceived(&out, cfg) - 0.75).abs() < 0.01);
    }

    #[test]
    fn below_transition_pins_floor_and_alpha_is_linear_perceived() {
        // floor 0, m 25 ⇒ B = 25; slider 10 ⇒ hardware pinned 0, alpha = 1 - 10/25 = 0.6.
        let cfg = ContinuumConfig::hardware(0, 25, DimMode::Overlay);
        let out = map_user_level(10, &cfg);
        assert_eq!(out.hardware_pct, Some(0));
        assert!(close(out.overlay_alpha, 0.6));
        assert_eq!(out.gamma, None);
        assert!(close(perceived(&out, cfg), 0.10));
    }

    #[test]
    fn floor_zero_still_has_software_zone() {
        // The regression this whole change enables: at floor 0 (the default),
        // slider positions below the perceptual anchor `m` still dim via overlay
        // — v1 had no sub-floor zone at floor 0 (hence the old 20%-seed hack).
        let cfg = ContinuumConfig::hardware(0, 25, DimMode::Overlay);
        let out = map_user_level(10, &cfg);
        assert_eq!(out.hardware_pct, Some(0));
        assert!(
            out.overlay_alpha > 0.0,
            "floor-0 must still engage overlay below the anchor"
        );
        assert!(close(out.overlay_alpha, 0.6));
    }

    #[test]
    fn floor_change_does_not_move_slider_geometry() {
        // The scale (`pos`) depends only on `m`, never the floor: above both
        // floors' transitions the same slider drives the same hardware value.
        let low = ContinuumConfig::hardware(10, 25, DimMode::Overlay);
        let high = ContinuumConfig::hardware(50, 25, DimMode::Overlay);
        assert_eq!(
            map_user_level(75, &low).hardware_pct,
            map_user_level(75, &high).hardware_pct
        );
        // And line A (hardware zero) is identical regardless of floor.
        assert_eq!(geometry(&low).hw_zero, geometry(&high).hw_zero);
    }

    #[test]
    fn m_zero_floor_zero_degenerates_to_v1_identity() {
        // m=0, floor=0 ⇒ B=0, so the mapping is v1's pure linear hardware
        // identity (slider == hardware, no overlay ever).
        let cfg = ContinuumConfig::hardware(0, 0, DimMode::Overlay);
        for p in [0u8, 1, 37, 50, 99, 100] {
            let out = map_user_level(p, &cfg);
            assert_eq!(out.hardware_pct, Some(p), "p={p}");
            assert!(close(out.overlay_alpha, 0.0), "p={p}");
            assert_eq!(out.gamma, None);
        }
    }

    #[test]
    fn floor_100_is_all_software_below_100() {
        // floor 100, m 25 ⇒ B = pos(100) = 100: every position below 100 pins
        // hardware high and dims via overlay.
        let cfg = ContinuumConfig::hardware(100, 25, DimMode::Overlay);
        let out = map_user_level(50, &cfg);
        assert_eq!(out.hardware_pct, Some(100));
        assert!(close(out.overlay_alpha, 0.5)); // 1 - 50/100
        let full = map_user_level(100, &cfg);
        assert_eq!(full.hardware_pct, Some(100));
        assert!(close(full.overlay_alpha, 0.0));
    }

    #[test]
    fn off_mode_pins_floor_without_software() {
        // floor 50, m 25 ⇒ B = 62.5; slider 20 < B with Off ⇒ hardware pinned, no dim.
        let out = map_user_level(20, &ContinuumConfig::hardware(50, 25, DimMode::Off));
        assert_eq!(out.hardware_pct, Some(50));
        assert!(close(out.overlay_alpha, 0.0));
        assert_eq!(out.gamma, None);
    }

    #[test]
    fn gamma_mode_reduces_gamma_below_transition() {
        let out = map_user_level(0, &ContinuumConfig::hardware(50, 25, DimMode::Gamma));
        assert!(close(out.overlay_alpha, 0.0));
        match out.gamma {
            Some(g) => assert!(close(g, 1.0 - MAX_ALPHA)),
            None => panic!("expected gamma to be engaged"),
        }
    }

    #[test]
    fn zero_reaches_max_alpha() {
        let cfg = ContinuumConfig::hardware(50, 25, DimMode::Overlay);
        let out = map_user_level(0, &cfg);
        assert!(close(out.overlay_alpha, MAX_ALPHA));
        assert_eq!(out.hardware_pct, Some(50));
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
    fn out_of_range_user_is_clamped() {
        let out = map_user_level(200, &ContinuumConfig::hardware(50, 25, DimMode::Overlay));
        assert_eq!(out.hardware_pct, Some(100));
        assert!(close(out.overlay_alpha, 0.0));
    }

    // --- reverse mapping ---

    #[test]
    fn reverse_map_is_pos_of_reading() {
        let cfg = ContinuumConfig::hardware(50, 25, DimMode::Overlay);
        // Hardware zero reflects to line A = m; hardware max to slider 100.
        assert_eq!(reverse_map(0, &cfg), 25);
        assert_eq!(reverse_map(100, &cfg), 100);
        // Hardware 40 with m 25 ⇒ pos = 25 + 75·0.4 = 55.
        assert_eq!(
            reverse_map(40, &ContinuumConfig::hardware(0, 25, DimMode::Overlay)),
            55
        );
    }

    #[test]
    fn reverse_map_does_not_clamp_below_floor() {
        // A reading below Duja's floor reflects truthfully (between A and B),
        // never snapped up. floor 50, m 25, reading 20 ⇒ pos = 40.
        let cfg = ContinuumConfig::hardware(50, 25, DimMode::Overlay);
        assert_eq!(reverse_map(20, &cfg), 40);
        // 40 is below the transition B = pos(50) = 62.5, proving no floor clamp.
        assert!(40.0 < pos(50.0, 25.0));
    }

    #[test]
    fn reverse_map_endpoints_exact() {
        for (floor, m) in [(0u8, 25u8), (50, 10), (100, 60), (0, 0)] {
            let cfg = ContinuumConfig::hardware(floor, m, DimMode::Overlay);
            assert_eq!(reverse_map(100, &cfg), 100);
            assert_eq!(reverse_map(0, &cfg), m);
        }
    }

    #[test]
    fn reverse_map_passes_through_on_software_only() {
        let cfg = ContinuumConfig::software_only(DimMode::Overlay);
        assert_eq!(reverse_map(0, &cfg), 0);
        assert_eq!(reverse_map(42, &cfg), 42);
        assert_eq!(reverse_map(255, &cfg), 100);
    }

    // --- property tests (plan §4.2) ---

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Perceived output never decreases as the user level rises.
        #[test]
        fn continuum_monotonic_over_full_range(cfg in any_cfg()) {
            let levels: Vec<f32> = (0u8..=100)
                .map(|u| perceived(&map_user_level(u, &cfg), cfg))
                .collect();
            for pair in levels.windows(2) {
                if let [a, b] = pair {
                    prop_assert!(*b >= *a - 1e-4, "non-monotonic {a} -> {b} for {cfg:?}");
                }
            }
        }

        /// No visible jump in perceived output across the hardware/software
        /// handoff at `B = pos(floor)`.
        #[test]
        fn continuum_continuous_at_transition(m in 0u8..=95, floor in 1u8..=100, mode in any_mode()) {
            let cfg = ContinuumConfig::hardware(floor, m, mode);
            let b = f32::from(m) + (100.0 - f32::from(m)) * f32::from(floor) / 100.0;
            // RATIONALE: `b` in `0.0..=100.0`; floor()/ceil() land in range and are
            // integral, so the casts cannot truncate, lose a sign, or overflow.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let below = b.floor() as u8;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let above = (b.ceil() as u8).min(100);
            let pb = perceived(&map_user_level(below, &cfg), cfg);
            let pa = perceived(&map_user_level(above, &cfg), cfg);
            prop_assert!((pa - pb).abs() < 0.03, "jump at B={b}: {pb} -> {pa} for {cfg:?}");
        }

        /// 100% is exactly full brightness for every configuration.
        #[test]
        fn continuum_endpoints_exact(cfg in any_cfg()) {
            let out = map_user_level(100, &cfg);
            prop_assert!(out.overlay_alpha.abs() <= f32::EPSILON);
            prop_assert!((perceived(&out, cfg) - 1.0).abs() < 1e-4);
            if cfg.hardware_floor.is_some() {
                prop_assert_eq!(out.hardware_pct, Some(100));
            } else {
                prop_assert_eq!(out.hardware_pct, None);
            }
        }

        /// The darkest overlay level is exactly MAX_ALPHA (hardware pinned).
        #[test]
        fn continuum_dark_endpoint_is_max_alpha(m in 0u8..=95, floor in 1u8..=100) {
            let cfg = ContinuumConfig::hardware(floor, m, DimMode::Overlay);
            let out = map_user_level(0, &cfg);
            prop_assert!((out.overlay_alpha - MAX_ALPHA).abs() <= f32::EPSILON);
            prop_assert_eq!(out.hardware_pct, Some(floor));
        }

        /// Above the transition, mapping to hardware then reflecting it back
        /// reproduces the slider within ±1 (u8 quantization through `pos` and its
        /// inverse).
        #[test]
        fn continuum_reverse_map_roundtrip((m, floor, user) in hardware_side_user(), mode in any_mode()) {
            let cfg = ContinuumConfig::hardware(floor, m, mode);
            let hw = map_user_level(user, &cfg).hardware_pct.expect("hardware display");
            let back = reverse_map(hw, &cfg);
            prop_assert!(back.abs_diff(user) <= 1, "user={user} hw={hw} back={back} for {cfg:?}");
        }

        /// Software-only displays never touch hardware.
        #[test]
        fn continuum_degenerate_hw_range(user in 0u8..=100, mode in any_mode()) {
            let cfg = ContinuumConfig::software_only(mode);
            prop_assert_eq!(map_user_level(user, &cfg).hardware_pct, None);
        }
    }
}

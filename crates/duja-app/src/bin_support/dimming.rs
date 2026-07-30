//! The pure continuum → dimmer planner.
//!
//! Given the set of displays with their current *user* levels, a per-display
//! [`ContinuumConfig`], and a bounds lookup, this produces the two declarative
//! outputs a level change fans out to:
//!
//! - **hardware levels** — the hardware percentage per display, pinned at the
//!   floor below it (fed to the engine via `EngineCommand::SetUserLevel`, which
//!   scales it onto the probed range);
//! - **overlay/gamma commands** — one [`DimCommand`] per display that has known
//!   bounds (physical pixels on Windows, points on macOS — see
//!   [`DisplayBounds`]), carrying the overlay alpha and (opt-in) gamma from the
//!   continuum. The batch is the *full* desired dimmer state: a display at
//!   alpha 0 is included so [`Dimmer::apply`](duja_core::dimmer::Dimmer::apply)
//!   removes any stale overlay. A display with no known bounds is omitted — it
//!   cannot be overlaid, a documented limitation. That is any panel from the OS
//!   panel backend: a Windows WMI panel (no monitor rect is plumbed) and equally a
//!   macOS `DisplayServices` panel (no bounds are stamped for it yet, though its
//!   `CGDirectDisplayID` makes them cheap to obtain — see `docs/debt.md`). A
//!   Windows DDC-fallback internal panel does carry bounds and is dimmable.
//!
//! The module is OS-free and fully unit-tested; the app's notification loop
//! calls it and hands the batch to the real `Dimmer`.
//!
//! # The platform gamma minimum
//!
//! A gamma factor the OS will refuse is worse than useless: the ramp write fails,
//! the display is left undimmed, and — because the gamma path sets
//! `overlay_alpha` to 0 — nothing else is dimming it either, so the user's slider
//! does nothing. Windows refuses any ramp deviating too far from the identity, so
//! `duja_dimmer::min_gamma_factor()` there is `0.5` while the continuum reaches
//! down to `GAMMA_FLOOR` (`0.3`).
//!
//! [`plan`] therefore takes that minimum and, for any display whose mapped gamma
//! factor falls below it, re-maps the level in [`DimMode::Overlay`] — realising
//! the same requested level through the mechanism ADR-0003 makes primary, instead
//! of asking the OS for a ramp it is documented to reject. The two are equal
//! *levels* by construction (the continuum defines `gamma == 1 - alpha`, and a
//! Windows layered overlay blends in the same encoded space a ramp scales), but
//! they are **not** the same coverage — see `docs/debt.md`.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use duja_core::continuum::{ContinuumConfig, ContinuumOutput, map_user_level};
use duja_core::dimmer::{DimCommand, DisplayBounds, clamp_gamma};
use duja_core::id::StableDisplayId;
use duja_core::model::{DimMode, DisplayKind};

/// One display's input to the planner: its identity, class, and current *user*
/// slider level (`0..=100`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayInput {
    /// Resolved display id (slot-suffixed for twins).
    pub(crate) id: StableDisplayId,
    /// Physical class (provenance).
    pub(crate) kind: DisplayKind,
    /// Whether the display is dimmed purely in software (no working hardware
    /// brightness) — the runtime flag `continuum_for` routes on to pick the
    /// software-only vs hardware continuum.
    pub(crate) software_only: bool,
    /// The user's slider level, `0..=100`.
    pub(crate) user_pct: u8,
}

/// The declarative outputs of one planning pass.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DimPlan {
    /// Overlay/gamma commands — the full desired dimmer state.
    pub(crate) commands: Vec<DimCommand>,
    /// Hardware percentage to drive per display (continuum-floored).
    pub(crate) hardware: Vec<(StableDisplayId, u8)>,
}

/// Plan the hardware levels and overlay commands for every display.
///
/// `cfg_for` yields the (already HDR-guarded) [`ContinuumConfig`] for a display;
/// `bounds_for` yields its bounds in the platform's unit (physical pixels on
/// Windows, points on macOS — see [`DisplayBounds`]), or `None` when they are
/// unknown. The planner is unit-agnostic: it copies the bounds into the
/// [`DimCommand`] untouched, and the platform dimmer consumes its own unit.
///
/// `min_gamma` is the lowest gamma factor this platform's OS will accept
/// (`duja_dimmer::min_gamma_factor()`); a display whose mapped factor falls below
/// it is planned as an overlay instead. See the [module docs](self).
pub(crate) fn plan(
    displays: &[DisplayInput],
    cfg_for: impl Fn(&DisplayInput) -> ContinuumConfig,
    bounds_for: impl Fn(&StableDisplayId) -> Option<DisplayBounds>,
    min_gamma: f32,
) -> DimPlan {
    let mut commands = Vec::new();
    let mut hardware = Vec::new();

    for display in displays {
        let cfg = cfg_for(display);
        let out = reachable_output(display.user_pct, cfg, min_gamma);

        if let Some(hw) = out.hardware_pct {
            hardware.push((display.id.clone(), hw));
        }
        if let Some(bounds) = bounds_for(&display.id) {
            commands.push(DimCommand::new(
                display.id.clone(),
                bounds,
                out.overlay_alpha,
                out.gamma,
            ));
        }
    }

    DimPlan { commands, hardware }
}

/// Map `user_pct` under `cfg`, substituting an overlay for a gamma factor the
/// platform cannot accept.
///
/// The same shape as the HDR guard one level up (`settings::effective_mode` forces
/// [`DimMode::Overlay`] when gamma is unsafe), with a second reason and a
/// finer grain: HDR is a property of the display, whereas reachability is a
/// property of the *requested factor*, so it can only be decided here, after the
/// mapping. A gamma-mode display keeps its ramp for the part of the sub-floor zone
/// the OS will accept and gets an overlay below that.
fn reachable_output(user_pct: u8, cfg: ContinuumConfig, min_gamma: f32) -> ContinuumOutput {
    let out = map_user_level(user_pct, &cfg);
    // Compare the factor that would actually reach the OS: `DimCommand::new`
    // clamps to `GAMMA_FLOOR` on the way out, so a raw factor below the floor is
    // never what gets written. Off Windows `min_gamma == GAMMA_FLOOR`, which makes
    // this condition unsatisfiable and the whole path inert.
    if out.gamma.map(clamp_gamma).is_some_and(|f| f < min_gamma) {
        // Re-map rather than deriving `alpha = 1 - gamma` by hand: the continuum
        // owns the level↔alpha relation, and asking it for the Overlay answer to
        // the same question is exactly what `dim_mode = "overlay"` would have
        // produced for this slider position.
        return map_user_level(
            user_pct,
            &ContinuumConfig {
                mode: DimMode::Overlay,
                ..cfg
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bin_support::settings::continuum_for;
    use duja_core::config::{DimMode as ConfigDimMode, MonitorConfig};
    use duja_core::continuum::{MAX_ALPHA, geometry};
    use duja_core::dimmer::GAMMA_FLOOR;

    /// An OS that accepts every factor the continuum can produce — i.e. no
    /// platform limit beyond Duja's own [`GAMMA_FLOOR`], which is what
    /// `duja_dimmer::min_gamma_factor()` reports off Windows. Tests that predate
    /// the platform-minimum fallback pass this so their subject is unchanged.
    const NO_GAMMA_LIMIT: f32 = GAMMA_FLOOR;

    /// Windows' measured minimum (`duja_dimmer::MIN_ACCEPTED_GAMMA`), written as a
    /// literal so the fallback's behaviour is pinned on **every** CI lane rather
    /// than only where the constant happens to have this value.
    const WINDOWS_MIN_GAMMA: f32 = 0.5;

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
    }

    fn bounds() -> DisplayBounds {
        DisplayBounds::new(0, 0, 1920, 1080)
    }

    fn input(serial: &str, kind: DisplayKind, user_pct: u8) -> DisplayInput {
        DisplayInput {
            id: id(serial),
            kind,
            software_only: false,
            user_pct,
        }
    }

    fn monitor(floor: u8, mode: ConfigDimMode) -> MonitorConfig {
        MonitorConfig {
            hw_floor_pct: floor,
            dim_mode: mode,
            ..MonitorConfig::default()
        }
    }

    #[test]
    fn slider_below_floor_engages_overlay() {
        let displays = [input("A", DisplayKind::ExternalDdc, 0)];
        let mon = monitor(30, ConfigDimMode::Overlay);
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        // Hardware pinned at the floor, not driven to zero.
        assert_eq!(plan.hardware, vec![(id("A"), 30)]);
        // A visible overlay at full strength.
        let cmd = plan.commands.first().expect("one command");
        assert!(cmd.has_overlay());
        assert!((cmd.overlay_alpha - MAX_ALPHA).abs() < 1e-6);
        assert_eq!(cmd.gamma, None);
    }

    #[test]
    fn above_floor_has_no_overlay() {
        let displays = [input("A", DisplayKind::ExternalDdc, 80)];
        let mon = monitor(30, ConfigDimMode::Overlay); // default anchor 25
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        // Above the transition B = pos(30) = 47.5, slider 80 maps to hardware
        // round((80-25)·100/75) = 73 (perceptual inverse), no overlay.
        assert_eq!(plan.hardware, vec![(id("A"), 73)]);
        // Still emitted (declarative full state) but with no visible overlay.
        let cmd = plan.commands.first().expect("one command");
        assert!(!cmd.has_overlay());
    }

    #[test]
    fn floor_zero_toggle_engages_overlay_below_min_perceived() {
        // The seed-hack replacement: with the default floor 0 and dimming on, a
        // slider below the perceptual anchor still engages the overlay. v1 had no
        // sub-floor zone at floor 0 (hence the deleted 20%-seed hack); v2's
        // perceptual anchor gives floor-0 displays a real software zone.
        let displays = [input("A", DisplayKind::ExternalDdc, 10)];
        let mon = monitor(0, ConfigDimMode::Overlay); // floor 0, default anchor 25
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        // Hardware pinned at the floor 0; the overlay supplies the sub-anchor dim.
        assert_eq!(plan.hardware, vec![(id("A"), 0)]);
        let cmd = plan.commands.first().expect("one command");
        assert!(
            cmd.has_overlay(),
            "floor-0 dimming must engage the overlay below the perceptual anchor"
        );
    }

    #[test]
    fn hdr_display_never_gets_gamma() {
        // Configured for gamma, but HDR/unknown forces overlay: no command in
        // the batch may carry a gamma factor.
        let displays = [input("A", DisplayKind::ExternalDdc, 0)];
        let mon = monitor(50, ConfigDimMode::Gamma);
        let plan = plan(
            &displays,
            |_| {
                continuum_for(
                    DisplayKind::ExternalDdc,
                    false,
                    &mon,
                    /* gamma_allowed */ false,
                )
            },
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        assert!(plan.commands.iter().all(|c| c.gamma.is_none()));
        // And the sub-floor dim is realised as an overlay instead.
        assert!(plan.commands.iter().any(DimCommand::has_overlay));
    }

    #[test]
    fn gamma_config_keeps_gamma_when_allowed() {
        let displays = [input("A", DisplayKind::ExternalDdc, 0)];
        let mon = monitor(50, ConfigDimMode::Gamma);
        let plan = plan(
            &displays,
            |_| {
                continuum_for(
                    DisplayKind::ExternalDdc,
                    false,
                    &mon,
                    /* gamma_allowed */ true,
                )
            },
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        // Gamma engaged: a gamma factor present, overlay alpha zero.
        let cmd = plan.commands.first().expect("one command");
        assert!(cmd.gamma.is_some());
        assert!(!cmd.has_overlay());
    }

    // --- The platform gamma minimum ----------------------------------------
    //
    // The reported defect: with `dim_mode = "gamma"` the continuum puts the whole
    // sub-floor dim in the gamma factor and leaves `overlay_alpha` at 0. Windows
    // refuses a ramp below factor 0.5, so below that point the ramp write failed
    // and **nothing** dimmed — 349 warnings in one user's log and a slider that
    // did nothing. These pin the planner's half of the cure.

    #[test]
    fn a_gamma_factor_below_the_platform_minimum_becomes_an_overlay() {
        // floor 50, anchor 25 ⇒ transition B = 62.5. Slider 0 asks the continuum
        // for gamma 0.12 (clamped to GAMMA_FLOOR 0.3 on the way out) — far below
        // the 0.5 Windows accepts. The plan must carry an overlay instead.
        let displays = [input("A", DisplayKind::ExternalDdc, 0)];
        let mon = monitor(50, ConfigDimMode::Gamma);
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            WINDOWS_MIN_GAMMA,
        );
        let cmd = plan.commands.first().expect("one command");
        assert_eq!(
            cmd.gamma, None,
            "a factor the OS refuses must not be planned as a gamma ramp"
        );
        assert!(
            cmd.has_overlay(),
            "the sub-floor dim must be realised by the overlay instead"
        );
        assert!((cmd.overlay_alpha - MAX_ALPHA).abs() < 1e-6);
        // The hardware half of the plan is untouched: still pinned at the floor.
        assert_eq!(plan.hardware, vec![(id("A"), 50)]);
    }

    #[test]
    fn a_gamma_factor_the_platform_accepts_is_still_a_gamma_ramp() {
        // The fallback is per requested factor, not per display: gamma keeps the
        // part of the sub-floor zone the OS will take. B = 62.5, so slider 32 asks
        // for 32/62.5 = 0.512 — above the minimum, and it stays a ramp.
        let mon = monitor(50, ConfigDimMode::Gamma);
        let accepted = plan(
            &[input("A", DisplayKind::ExternalDdc, 32)],
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            WINDOWS_MIN_GAMMA,
        );
        let cmd = accepted.commands.first().expect("one command");
        let factor = cmd
            .gamma
            .expect("gamma survives above the platform minimum");
        assert!(factor >= WINDOWS_MIN_GAMMA, "planned factor {factor}");
        assert!(!cmd.has_overlay(), "gamma mode drives no overlay");

        // One step lower crosses the boundary and switches mechanism: 31/62.5 =
        // 0.496 < 0.5.
        let refused = plan(
            &[input("A", DisplayKind::ExternalDdc, 31)],
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            WINDOWS_MIN_GAMMA,
        );
        let cmd = refused.commands.first().expect("one command");
        assert_eq!(cmd.gamma, None);
        assert!(cmd.has_overlay());
    }

    #[test]
    fn the_overlay_substitute_delivers_the_level_the_ramp_was_asked_for() {
        // "Not failing" is not the bar — the display must end up dimmed to the
        // level the user asked for. The continuum defines the two channels as
        // equivalent (`gamma == 1 - alpha`), so the substituted overlay's
        // `1 - alpha` must equal the factor the gamma path asked for.
        let mon = monitor(50, ConfigDimMode::Gamma);
        for user_pct in [0u8, 5, 15, 25, 31] {
            let cfg = continuum_for(DisplayKind::ExternalDdc, false, &mon, true);
            let wanted = map_user_level(user_pct, &cfg)
                .gamma
                .expect("gamma mode below the transition asks for a factor");
            let plan = plan(
                &[input("A", DisplayKind::ExternalDdc, user_pct)],
                |_| cfg,
                |_| Some(bounds()),
                WINDOWS_MIN_GAMMA,
            );
            let cmd = plan.commands.first().expect("one command");
            let delivered = 1.0 - cmd.overlay_alpha;
            assert!(
                (delivered - wanted).abs() < 1e-6,
                "slider {user_pct}: overlay delivers {delivered}, the ramp wanted {wanted}"
            );
        }
    }

    #[test]
    fn no_slider_position_asks_for_a_ramp_the_platform_will_refuse() {
        // The defect stated as an invariant over the whole slider: below the
        // hardware/software handoff, every position must be dimmed by something
        // this platform can actually deliver — an overlay, or a gamma factor at or
        // above the OS minimum. A plan carrying `gamma: Some(0.3)` with alpha 0 is
        // a display that does not dim at all.
        let mon = monitor(50, ConfigDimMode::Gamma);
        let cfg = continuum_for(DisplayKind::ExternalDdc, false, &mon, true);
        let transition = geometry(&cfg).transition.expect("hardware display") * 100.0;
        for user_pct in 0..=100u8 {
            if f32::from(user_pct) >= transition {
                continue; // above the handoff the hardware alone carries the level
            }
            let plan = plan(
                &[input("A", DisplayKind::ExternalDdc, user_pct)],
                |_| cfg,
                |_| Some(bounds()),
                WINDOWS_MIN_GAMMA,
            );
            let cmd = plan.commands.first().expect("one command");
            let ramp_is_deliverable = cmd.gamma.is_some_and(|f| f >= WINDOWS_MIN_GAMMA);
            assert!(
                ramp_is_deliverable || cmd.has_overlay(),
                "slider {user_pct} plans gamma {:?} + alpha {}: the OS refuses that ramp and \
                 nothing else dims the display",
                cmd.gamma,
                cmd.overlay_alpha
            );
        }
    }

    #[test]
    fn a_platform_without_a_gamma_limit_keeps_every_gamma_factor() {
        // Off Windows `min_gamma_factor()` is GAMMA_FLOOR, and `clamp_gamma` bounds
        // every factor at the floor — so the condition can never fire and the
        // fallback is inert. Guards against the substitution leaking onto macOS,
        // where CGSetDisplayTransferByFormula imposes no such limit.
        let mon = monitor(50, ConfigDimMode::Gamma);
        for user_pct in [0u8, 10, 31, 50] {
            let plan = plan(
                &[input("A", DisplayKind::ExternalDdc, user_pct)],
                |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
                |_| Some(bounds()),
                NO_GAMMA_LIMIT,
            );
            let cmd = plan.commands.first().expect("one command");
            assert!(
                cmd.gamma.is_some(),
                "slider {user_pct} must stay on the gamma path where the OS allows it"
            );
            assert!(!cmd.has_overlay());
        }
    }

    #[test]
    fn display_without_bounds_gets_no_command_but_still_hardware() {
        let displays = [input("A", DisplayKind::InternalPanel, 10)];
        let mon = monitor(40, ConfigDimMode::Overlay);
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::InternalPanel, false, &mon, true),
            |_| None, // bounds unknown
            NO_GAMMA_LIMIT,
        );
        assert!(plan.commands.is_empty());
        assert_eq!(plan.hardware, vec![(id("A"), 40)]);
    }

    #[test]
    fn one_group_input_yields_one_command_collapsing_the_mirror_double_overlay() {
        // #66: two mirrored panels used to feed TWO DisplayInputs at IDENTICAL
        // bounds, so the planner emitted TWO overlay commands stacked on the same
        // shared pixels (the double-dim). The planner is 1:1 input→command by
        // design and cannot dedupe that — so the fix collapses the mirror to ONE
        // group input (the anchor) upstream, yielding exactly one overlay per
        // surface.
        let mon = monitor(30, ConfigDimMode::Overlay);
        // The pre-fix reality: two ids at the same bounds ⇒ two overlays.
        let both = [
            input("A", DisplayKind::ExternalDdc, 0),
            input("B", DisplayKind::ExternalDdc, 0),
        ];
        let two = plan(
            &both,
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        assert_eq!(
            two.commands.len(),
            2,
            "two ids at identical bounds ⇒ two stacked overlays (the bug)"
        );
        // The fix: one merged group input ⇒ exactly one overlay for the surface.
        let one = [input("A", DisplayKind::ExternalDdc, 0)];
        let single = plan(
            &one,
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        assert_eq!(
            single.commands.len(),
            1,
            "one group input ⇒ exactly one overlay per shared surface"
        );
    }

    #[test]
    fn software_only_display_has_no_hardware_entry() {
        // A software-only display — flagged at runtime, on any physical kind — has
        // no hardware entry: the whole slider is software overlay. Route the
        // continuum on the input's flag, exactly as the tray does.
        let displays = [DisplayInput {
            id: id("A"),
            kind: DisplayKind::InternalPanel,
            software_only: true,
            user_pct: 0,
        }];
        let mon = monitor(0, ConfigDimMode::Overlay);
        let plan = plan(
            &displays,
            |d| continuum_for(d.kind, d.software_only, &mon, true),
            |_| Some(bounds()),
            NO_GAMMA_LIMIT,
        );
        assert!(plan.hardware.is_empty());
        assert!(plan.commands.first().is_some_and(DimCommand::has_overlay));
    }
}

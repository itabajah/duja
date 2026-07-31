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
//!
//! Because the substitution is invisible (the level is right; the surfaces it
//! covers are not), the settings window discloses it. That caption needs the same
//! number, so [`gamma_cap_pct`] lives here too: it is the minimum expressed as a
//! percentage, and `None` — no cap — is simultaneously the answer to "how far
//! down does gamma reach" and to "is there anything to say about it". `duja-ui`
//! cannot derive either for itself; it depends on neither `duja-dimmer` nor
//! `duja-platform`, which is why the figure was a hardcoded `50`, shown on every
//! platform, until `#103`.

// RATIONALE: these pure modules are consumed only by the tray assembly (Windows and macOS),
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use duja_core::continuum::{ContinuumConfig, ContinuumOutput, map_user_level};
use duja_core::dimmer::{DimCommand, DisplayBounds, GAMMA_FLOOR, clamp_gamma};
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

/// [`plan`] with **this platform's** gamma minimum supplied.
///
/// This is what the tray calls, and the seam exists so the *choice* of minimum is
/// pinned by a test instead of living at an `AppState` call site no test can reach:
/// [`plan`]'s own tests drive the parameter directly with a literal, and
/// `plan_for_platform_uses_the_dimmer_crates_gamma_minimum` pins that this wrapper
/// passes `duja_dimmer::min_gamma_factor()` rather than, say, `GAMMA_FLOOR` — the
/// substitution that would silently restore the whole defect.
///
/// It works only because `duja-dimmer` is an **unconditional** dependency of
/// `duja-app`, so this cross-platform module can reach the platform verdict itself.
pub(crate) fn plan_for_platform(
    displays: &[DisplayInput],
    cfg_for: impl Fn(&DisplayInput) -> ContinuumConfig,
    bounds_for: impl Fn(&StableDisplayId) -> Option<DisplayBounds>,
) -> DimPlan {
    plan(
        displays,
        cfg_for,
        bounds_for,
        duja_dimmer::min_gamma_factor(),
    )
}

/// How far down the gamma channel reaches on an OS whose minimum is `min_gamma`,
/// as a percentage — or `None` when there is nothing to disclose.
///
/// This is the settings window's caption, expressed as a number instead of a
/// sentence. `None` means the OS accepts every factor the continuum can produce,
/// so [`reachable_output`]'s substitution is unreachable and a caption about it
/// would describe a thing that never happens.
///
/// The percentage is the gamma **factor**, which under the continuum is the
/// perceived-brightness fraction the channel is being asked for
/// (`gamma == 1 - alpha`) — so `Some(50)` reads as "gamma dims to at most 50%",
/// which is the shipped copy.
///
/// Robust rather than trusting: `min_gamma` below the floor, or `NaN`, both
/// answer `None`. Neither is reachable through
/// [`gamma_cap_pct_for_platform`] — `duja-dimmer` pins
/// `min_gamma_factor() >= GAMMA_FLOOR` on every lane — but this function is the
/// one that turns a number into a user-facing claim, and the failure it would
/// otherwise have (a `0%` cap, or a claim that gamma reaches lower than Duja
/// itself allows) is a lie rather than a glitch.
pub(crate) fn gamma_cap_pct(min_gamma: f32) -> Option<u8> {
    // NaN first: `NaN <= x` is false, so without this it would fall through to the
    // cast, and `NaN as u8` is 0 — a caption claiming gamma dims to at most 0%.
    if min_gamma.is_nan() || min_gamma <= GAMMA_FLOOR {
        return None;
    }
    let pct = (min_gamma * 100.0).round().clamp(0.0, 100.0);
    // RATIONALE (cast_possible_truncation, cast_sign_loss): `pct` is integral after
    // `round()` and clamped into `0.0..=100.0`, so the cast is exact and in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = pct as u8;
    Some(out)
}

/// [`gamma_cap_pct`] with **this platform's** gamma minimum supplied.
///
/// The same seam as [`plan_for_platform`], for the same reason: the *choice* of
/// minimum is pinned by a test instead of living at a call site no test can reach.
/// `duja-ui` cannot make this call itself — it depends on neither `duja-dimmer`
/// nor `duja-platform`, which is why the figure used to be a hardcoded `50` in
/// `settings.slint` shown on every platform.
pub(crate) fn gamma_cap_pct_for_platform() -> Option<u8> {
    gamma_cap_pct(duja_dimmer::min_gamma_factor())
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
    fn plan_for_platform_uses_the_dimmer_crates_gamma_minimum() {
        // The wiring, pinned where it can be: the whole fix depends on the minimum
        // being the *platform's* rather than `GAMMA_FLOOR`, and this is the only
        // layer that can observe which one was used. Substituting `GAMMA_FLOOR` in
        // `plan_for_platform` reds the Windows arm below.
        let mon = monitor(50, ConfigDimMode::Gamma);
        let displays = [input("A", DisplayKind::ExternalDdc, 0)];
        let plan = plan_for_platform(
            &displays,
            |_| continuum_for(DisplayKind::ExternalDdc, false, &mon, true),
            |_| Some(bounds()),
        );
        let cmd = plan.commands.first().expect("one command");

        #[cfg(windows)]
        {
            assert_eq!(
                cmd.gamma, None,
                "on Windows slider 0 asks for a factor the OS refuses, so the plan \
                 must carry an overlay — `plan_for_platform` did not use MIN_ACCEPTED_GAMMA"
            );
            assert!(cmd.has_overlay());
        }
        #[cfg(not(windows))]
        {
            assert!(
                cmd.gamma.is_some(),
                "off Windows the OS imposes no limit, so the ramp must survive"
            );
            assert!(!cmd.has_overlay());
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

    #[test]
    fn an_os_that_accepts_the_whole_range_has_no_cap_to_disclose() {
        // The caption's gate. An OS with no limit beyond `GAMMA_FLOOR` never makes
        // `reachable_output` substitute an overlay, so a caption saying it does
        // would describe a thing that cannot happen — which is what shipped, on
        // every platform, before this.
        assert_eq!(gamma_cap_pct(NO_GAMMA_LIMIT), None);
        // Strictly below the floor is the same answer for a different reason: the
        // limit is not the OS's, it is Duja's own, and the OS has nothing to add.
        assert_eq!(gamma_cap_pct(0.1), None);
    }

    #[test]
    fn a_capped_os_discloses_the_factor_as_a_percentage() {
        // The number the caption interpolates. 50 is what the hardcoded string
        // said, so a Windows user sees the identical sentence they saw before.
        assert_eq!(gamma_cap_pct(WINDOWS_MIN_GAMMA), Some(50));
        // Not Windows-specific arithmetic: any tighter OS reports its own figure,
        // rounded to the nearest percent rather than truncated (0.578 -> 58, which
        // a truncating implementation would call 57 — understating the cap, i.e.
        // promising a reach the OS does not give).
        assert_eq!(gamma_cap_pct(0.578), Some(58));
        assert_eq!(gamma_cap_pct(1.0), Some(100));
    }

    #[test]
    fn a_nonsensical_minimum_disclaims_rather_than_claiming_zero() {
        // Unreachable through `gamma_cap_pct_for_platform` — `duja-dimmer` pins
        // `min_gamma_factor() >= GAMMA_FLOOR` on every lane — but this is the
        // function that turns a float into a sentence shown to a user, and the
        // untended failure is a claim ("gamma dims to at most 0%"), not a glitch.
        // `NaN as u8` is 0, and `NaN <= GAMMA_FLOOR` is false, so without the
        // explicit NaN arm this is exactly what would be printed.
        assert_eq!(gamma_cap_pct(f32::NAN), None);
        assert_eq!(gamma_cap_pct(f32::NEG_INFINITY), None);
        // Above 1.0 is not a cap on dimming at all (a factor over 1 brightens),
        // but it must still not overflow the `u8` the UI carries.
        assert_eq!(gamma_cap_pct(f32::INFINITY), Some(100));
        assert_eq!(gamma_cap_pct(4.0), Some(100));
    }

    #[test]
    fn gamma_cap_pct_for_platform_uses_the_dimmer_crates_gamma_minimum() {
        // The wiring, pinned the same way `plan_for_platform`'s is: the caption is
        // only correct per-platform because the figure comes from the dimmer crate
        // rather than a literal. Substituting `GAMMA_FLOOR` reds the Windows arm.
        #[cfg(windows)]
        assert_eq!(
            gamma_cap_pct_for_platform(),
            Some(50),
            "Windows caps the ramp at MIN_ACCEPTED_GAMMA, and the caption says so"
        );
        #[cfg(not(windows))]
        assert_eq!(
            gamma_cap_pct_for_platform(),
            None,
            "off Windows the OS imposes no cap, so the caption must not appear"
        );
    }
}

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
//!   pixel bounds, carrying the overlay alpha and (opt-in) gamma from the
//!   continuum. The batch is the *full* desired dimmer state: a display at
//!   alpha 0 is included so [`Dimmer::apply`](duja_core::dimmer::Dimmer::apply)
//!   removes any stale overlay. A display with no known bounds (e.g. an internal
//!   panel we could not locate a monitor rect for) is omitted — it cannot be
//!   overlaid, a documented P4 limitation.
//!
//! The module is OS-free and fully unit-tested; the app's notification loop
//! calls it and hands the batch to the real `Dimmer`.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use duja_core::continuum::{ContinuumConfig, map_user_level};
use duja_core::dimmer::{DimCommand, DisplayBounds};
use duja_core::id::StableDisplayId;
use duja_core::model::DisplayKind;

/// One display's input to the planner: its identity, class, and current *user*
/// slider level (`0..=100`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayInput {
    /// Resolved display id (slot-suffixed for twins).
    pub(crate) id: StableDisplayId,
    /// Backend class (selects hardware vs software-only continuum).
    pub(crate) kind: DisplayKind,
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
/// `bounds_for` yields its pixel bounds, or `None` when they are unknown.
pub(crate) fn plan(
    displays: &[DisplayInput],
    cfg_for: impl Fn(&DisplayInput) -> ContinuumConfig,
    bounds_for: impl Fn(&StableDisplayId) -> Option<DisplayBounds>,
) -> DimPlan {
    let mut commands = Vec::new();
    let mut hardware = Vec::new();

    for display in displays {
        let cfg = cfg_for(display);
        let out = map_user_level(display.user_pct, &cfg);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bin_support::settings::continuum_for;
    use duja_core::config::{DimMode as ConfigDimMode, MonitorConfig};
    use duja_core::continuum::MAX_ALPHA;

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
            |_| continuum_for(DisplayKind::ExternalDdc, &mon, true),
            |_| Some(bounds()),
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
        let mon = monitor(30, ConfigDimMode::Overlay);
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::ExternalDdc, &mon, true),
            |_| Some(bounds()),
        );
        assert_eq!(plan.hardware, vec![(id("A"), 80)]);
        // Still emitted (declarative full state) but with no visible overlay.
        let cmd = plan.commands.first().expect("one command");
        assert!(!cmd.has_overlay());
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
                    &mon,
                    /* gamma_allowed */ false,
                )
            },
            |_| Some(bounds()),
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
                    &mon,
                    /* gamma_allowed */ true,
                )
            },
            |_| Some(bounds()),
        );
        // Gamma engaged: a gamma factor present, overlay alpha zero.
        let cmd = plan.commands.first().expect("one command");
        assert!(cmd.gamma.is_some());
        assert!(!cmd.has_overlay());
    }

    #[test]
    fn display_without_bounds_gets_no_command_but_still_hardware() {
        let displays = [input("A", DisplayKind::InternalPanel, 10)];
        let mon = monitor(40, ConfigDimMode::Overlay);
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::InternalPanel, &mon, true),
            |_| None, // bounds unknown
        );
        assert!(plan.commands.is_empty());
        assert_eq!(plan.hardware, vec![(id("A"), 40)]);
    }

    #[test]
    fn software_only_display_has_no_hardware_entry() {
        let displays = [input("A", DisplayKind::SoftwareOnly, 0)];
        let mon = monitor(0, ConfigDimMode::Overlay);
        let plan = plan(
            &displays,
            |_| continuum_for(DisplayKind::SoftwareOnly, &mon, true),
            |_| Some(bounds()),
        );
        assert!(plan.hardware.is_empty());
        assert!(plan.commands.first().is_some_and(DimCommand::has_overlay));
    }
}

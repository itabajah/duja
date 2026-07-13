//! Turning the typed [`Config`] into the per-display [`ContinuumConfig`] the
//! dimming planner consumes, plus the HDR gamma guard and theme mapping.
//!
//! # Floor semantics
//!
//! A hardware-backed display (DDC or internal panel) always gets a
//! [`ContinuumConfig::hardware`] with the configured `hw_floor_pct` — including
//! `0`, which the continuum treats as "drive the full hardware range, no overlay
//! until the very bottom". Only a [`DisplayKind::SoftwareOnly`] display (no
//! hardware backlight at all) maps to [`ContinuumConfig::software_only`], where
//! the overlay spans the whole slider.
//!
//! # HDR gamma guard
//!
//! Gamma ramps are meaningless under HDR, so a display configured for
//! [`DimMode::Gamma`] is silently downgraded to [`DimMode::Overlay`] whenever
//! the HDR probe does not *positively* confirm gamma is safe (HDR-active **or**
//! an indeterminate probe both force overlay). The probe result is captured once
//! per enumeration and passed in as `gamma_allowed`.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use duja_core::config::{Config, MonitorConfig, Theme as ConfigTheme};
use duja_core::continuum::ContinuumConfig;
use duja_core::model::{DimMode, DisplayKind};
use duja_ui::Theme as UiTheme;

/// Resolve the [`ContinuumConfig`] for one display from its per-monitor settings.
///
/// `gamma_allowed` is the once-per-enumeration HDR verdict: `false` forces every
/// [`DimMode::Gamma`] display onto the overlay path.
pub(crate) fn continuum_for(
    kind: DisplayKind,
    monitor: &MonitorConfig,
    gamma_allowed: bool,
) -> ContinuumConfig {
    let mode = effective_mode(monitor.dim_mode.into(), gamma_allowed);
    match kind {
        DisplayKind::SoftwareOnly => ContinuumConfig::software_only(mode),
        DisplayKind::ExternalDdc | DisplayKind::InternalPanel => {
            ContinuumConfig::hardware(monitor.hw_floor_pct, monitor.min_perceived_pct, mode)
        }
    }
}

/// The dim mode actually used after applying the HDR gamma guard.
///
/// [`DimMode::Gamma`] survives only when `gamma_allowed` is `true`; otherwise it
/// becomes [`DimMode::Overlay`]. Every other mode passes through unchanged.
pub(crate) fn effective_mode(mode: DimMode, gamma_allowed: bool) -> DimMode {
    match mode {
        DimMode::Gamma if !gamma_allowed => DimMode::Overlay,
        other => other,
    }
}

/// The per-monitor settings for `id`, or the schema defaults if the file has no
/// entry for it.
pub(crate) fn monitor_config(config: &Config, id: &str) -> MonitorConfig {
    config.monitors.get(id).cloned().unwrap_or_default()
}

/// The lowest perceived slider level reachable under `cfg`: the transition (line
/// B) when software dimming is off ([`DimMode::Off`], below which nothing dims),
/// else 0 (overlay/gamma can reach full dark). Used to re-clamp a stored level
/// after a mode/floor change so the thumb is never stranded below the slider's
/// new minimum.
pub(crate) fn min_reachable_pct(cfg: ContinuumConfig) -> u8 {
    let fraction = duja_core::continuum::geometry(&cfg).min_usable;
    // RATIONALE: `min_usable` ∈ [0.0, 1.0] ⇒ the product ∈ [0.0, 100.0] and is
    // integral after `round()`, so the cast cannot truncate, lose a sign, or
    // overflow — clippy's cast lints cannot see the numeric bounds.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pct = (fraction * 100.0).round() as u8;
    pct
}

/// Map the config's [`ConfigTheme`] preference onto the flyout's [`UiTheme`].
///
/// `System` resolves to the OS light/dark preference when `os_dark` is known
/// (`Some`), else defaults to dark (the flyout's Fluent-dark default; see the
/// deviation note — OS theme detection is best-effort in P4).
pub(crate) fn ui_theme(pref: ConfigTheme, os_dark: Option<bool>) -> UiTheme {
    match pref {
        ConfigTheme::Light => UiTheme::Light,
        ConfigTheme::Dark => UiTheme::Dark,
        ConfigTheme::System => match os_dark {
            Some(false) => UiTheme::Light,
            // Unknown or dark → dark.
            _ => UiTheme::Dark,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::config::DimMode as ConfigDimMode;

    fn monitor(floor: u8, mode: ConfigDimMode) -> MonitorConfig {
        MonitorConfig {
            hw_floor_pct: floor,
            dim_mode: mode,
            ..MonitorConfig::default()
        }
    }

    #[test]
    fn hardware_display_uses_configured_floor() {
        let cfg = continuum_for(
            DisplayKind::ExternalDdc,
            &monitor(20, ConfigDimMode::Overlay),
            true,
        );
        assert_eq!(cfg.hardware_floor, Some(20));
    }

    #[test]
    fn software_only_display_has_no_floor() {
        let cfg = continuum_for(
            DisplayKind::SoftwareOnly,
            &monitor(20, ConfigDimMode::Overlay),
            true,
        );
        assert_eq!(cfg.hardware_floor, None);
    }

    #[test]
    fn gamma_survives_when_allowed() {
        assert_eq!(effective_mode(DimMode::Gamma, true), DimMode::Gamma);
    }

    #[test]
    fn gamma_downgrades_to_overlay_under_hdr_or_unknown() {
        assert_eq!(effective_mode(DimMode::Gamma, false), DimMode::Overlay);
    }

    #[test]
    fn non_gamma_modes_pass_through() {
        assert_eq!(effective_mode(DimMode::Overlay, false), DimMode::Overlay);
        assert_eq!(effective_mode(DimMode::Off, false), DimMode::Off);
    }

    #[test]
    fn gamma_config_downgraded_end_to_end() {
        let cfg = continuum_for(
            DisplayKind::InternalPanel,
            &monitor(10, ConfigDimMode::Gamma),
            false,
        );
        assert_eq!(cfg.mode, DimMode::Overlay);
    }

    #[test]
    fn missing_monitor_entry_yields_defaults() {
        let config = Config::default();
        let m = monitor_config(&config, "GSM-5B09-unknown");
        assert_eq!(m, MonitorConfig::default());
    }

    #[test]
    fn min_reachable_pct_is_the_transition_only_when_dimming_off() {
        use duja_core::continuum::ContinuumConfig;
        // Dimming OFF ⇒ the slider bottoms out at line B (the transition).
        // floor 0, anchor 25 ⇒ B = 25; floor 20, anchor 25 ⇒ B = pos(20) = 40.
        assert_eq!(
            min_reachable_pct(ContinuumConfig::hardware(0, 25, DimMode::Off)),
            25
        );
        assert_eq!(
            min_reachable_pct(ContinuumConfig::hardware(20, 25, DimMode::Off)),
            40
        );
        // Overlay / Gamma can reach full dark ⇒ 0; software-only ⇒ 0.
        assert_eq!(
            min_reachable_pct(ContinuumConfig::hardware(0, 25, DimMode::Overlay)),
            0
        );
        assert_eq!(
            min_reachable_pct(ContinuumConfig::hardware(20, 25, DimMode::Gamma)),
            0
        );
        assert_eq!(
            min_reachable_pct(ContinuumConfig::software_only(DimMode::Off)),
            0
        );
    }

    #[test]
    fn theme_mapping_covers_every_case() {
        assert_eq!(ui_theme(ConfigTheme::Light, None), UiTheme::Light);
        assert_eq!(ui_theme(ConfigTheme::Dark, Some(false)), UiTheme::Dark);
        assert_eq!(ui_theme(ConfigTheme::System, Some(false)), UiTheme::Light);
        assert_eq!(ui_theme(ConfigTheme::System, Some(true)), UiTheme::Dark);
        assert_eq!(ui_theme(ConfigTheme::System, None), UiTheme::Dark);
    }
}

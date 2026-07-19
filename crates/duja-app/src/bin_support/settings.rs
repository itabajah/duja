//! Turning the typed [`Config`] into the per-display [`ContinuumConfig`] the
//! dimming planner consumes, plus the HDR gamma guard and theme mapping.
//!
//! # Floor semantics
//!
//! A hardware-backed display (any [`DisplayKind`], DDC or internal panel) gets a
//! [`ContinuumConfig::hardware`] with the configured `hw_floor_pct` — including
//! `0`, which the continuum treats as "drive the full hardware range, no overlay
//! until the very bottom". A display flagged `software_only` at runtime (no working
//! hardware backlight at all) maps instead to [`ContinuumConfig::software_only`],
//! where the overlay spans the whole slider. This selection is by the **flag**
//! alone, never the kind (#67).
//!
//! # HDR gamma guard
//!
//! Gamma ramps are meaningless under HDR, so a display configured for
//! [`DimMode::Gamma`] is silently downgraded to [`DimMode::Overlay`] whenever
//! the HDR probe does not *positively* confirm gamma is safe (HDR-active **or**
//! an indeterminate probe both force overlay). The probe result is refreshed on
//! each enumeration — the app re-probes the live HDR state (throttled off the
//! slider-drag hot path) and passes it in as `gamma_allowed` — so a display that
//! goes HDR mid-session stops receiving a bypassed gamma ramp.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use duja_core::config::{Config, DimMode as ConfigDimMode, MonitorConfig, Theme as ConfigTheme};
use duja_core::continuum::ContinuumConfig;
use duja_core::model::{DimMode, DisplayKind};
use duja_ui::Theme as UiTheme;

/// Resolve the [`ContinuumConfig`] for one display from its per-monitor settings.
///
/// Selection is by `software_only` alone: a `software_only` display (no working
/// hardware brightness) gets the full-slider [`ContinuumConfig::software_only`]
/// continuum; every hardware-backed display gets [`ContinuumConfig::hardware`] with
/// its configured floor. The physical `kind` is accepted (every caller already
/// holds it, and it keeps the door open for kind-specific policy) but does **not**
/// select the continuum — that decoupling is the point of #67: a software-only
/// internal panel must still dim in software.
///
/// `gamma_allowed` is the live HDR verdict, re-probed by the app on each
/// enumeration: `false` forces every [`DimMode::Gamma`] display onto the overlay
/// path.
pub(crate) fn continuum_for(
    kind: DisplayKind,
    software_only: bool,
    monitor: &MonitorConfig,
    gamma_allowed: bool,
) -> ContinuumConfig {
    // `kind` is intentionally not consulted: the continuum is chosen by the runtime
    // `software_only` flag, so a software-only display of ANY physical kind dims in
    // software (#67). Bind it explicitly so the decoupling is visible and lint-clean.
    let _ = kind;
    let mode = effective_mode(monitor.dim_mode.into(), gamma_allowed);
    if software_only {
        // A software-only display has no hardware channel, so `Off` (no software
        // dimming) would leave it with NO brightness control at all — a full-slider
        // overlay that never engages while the forced-on pill claims otherwise. Force
        // the EFFECTIVE mode `Off -> Overlay` (persisted `dim_mode` is untouched, so
        // the original returns if the display later clears `software_only`). `Gamma`
        // is a valid backlight-free dim, so it is kept.
        let mode = if mode == DimMode::Off {
            DimMode::Overlay
        } else {
            mode
        };
        ContinuumConfig::software_only(mode)
    } else {
        ContinuumConfig::hardware(monitor.hw_floor_pct, monitor.min_perceived_pct, mode)
    }
}

/// Whether the flyout's "Software dimming" pill reads as engaged for a display.
///
/// A software-only display always dims in software — the overlay is its only
/// channel, and [`continuum_for`] forces its effective mode `Off → Overlay` — so
/// the pill must read `on` regardless of the persisted `dim_mode`. Otherwise it
/// tracks the configured mode (engaged for anything but `Off`).
pub(crate) fn dimming_on(software_only: bool, dim_mode: ConfigDimMode) -> bool {
    software_only || dim_mode != ConfigDimMode::Off
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
            false,
            &monitor(20, ConfigDimMode::Overlay),
            true,
        );
        assert_eq!(cfg.hardware_floor, Some(20));
    }

    #[test]
    fn software_only_flag_selects_the_software_continuum_regardless_of_kind() {
        // The #67 decouple: routing is by the FLAG, not the kind. An internal-panel
        // OR external display with software_only == true both get the floorless
        // full-slider software continuum. (RED if continuum_for routed on `kind`,
        // which — with no SoftwareOnly kind left — would send both down the hardware
        // path and yield Some(floor).)
        for kind in [DisplayKind::InternalPanel, DisplayKind::ExternalDdc] {
            let cfg = continuum_for(kind, true, &monitor(20, ConfigDimMode::Overlay), true);
            assert_eq!(
                cfg.hardware_floor, None,
                "software_only ⇒ no hardware floor"
            );
        }
        // ...and the SAME InternalPanel with software_only == false IS hardware.
        let hw = continuum_for(
            DisplayKind::InternalPanel,
            false,
            &monitor(20, ConfigDimMode::Overlay),
            true,
        );
        assert_eq!(hw.hardware_floor, Some(20));
    }

    #[test]
    fn software_only_off_dims_via_overlay_and_reads_the_pill_on() {
        // Fix 2 (#67 follow-up): an External DDC display set to dim_mode=Off that
        // loses DDC at runtime is flagged software_only. Its forced-on, locked pill
        // must be truthful — the effective continuum forces Off -> Overlay so the
        // slider actually dims (overlay_alpha > 0), and the row reports dimming_on.
        // (RED against the old behavior: continuum_for gave software_only(Off) with
        // overlay_alpha 0 everywhere, and dimming_on ignored software_only -> false.)
        let cfg = continuum_for(
            DisplayKind::ExternalDdc,
            true,
            &monitor(20, ConfigDimMode::Off),
            true,
        );
        assert_eq!(
            cfg.mode,
            DimMode::Overlay,
            "a software-only display's Off must become Overlay"
        );
        let out = duja_core::continuum::map_user_level(0, &cfg);
        assert!(
            out.overlay_alpha > 0.0,
            "the slider must actually dim (overlay engages), not sit at alpha 0"
        );
        assert!(
            dimming_on(true, ConfigDimMode::Off),
            "a software-only row's pill must read on"
        );

        // Gamma is a valid backlight-free dim, so a selected Gamma mode survives.
        let g = continuum_for(
            DisplayKind::InternalPanel,
            true,
            &monitor(0, ConfigDimMode::Gamma),
            true,
        );
        assert_eq!(g.mode, DimMode::Gamma);

        // A HARDWARE display's Off is UNCHANGED: still hardware, mode Off, pill off.
        let hw = continuum_for(
            DisplayKind::ExternalDdc,
            false,
            &monitor(20, ConfigDimMode::Off),
            true,
        );
        assert_eq!(hw.mode, DimMode::Off);
        assert_eq!(hw.hardware_floor, Some(20));
        assert!(!dimming_on(false, ConfigDimMode::Off));
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
            false,
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

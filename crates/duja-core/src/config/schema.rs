//! The typed configuration schema (version 1).
//!
//! These serde structs are the *typed view* of the config file. They mirror the
//! TOML surface described in the plan (§7):
//!
//! ```toml
//! schema_version = 1
//!
//! [general]
//! autostart = true
//! update_check = false   # opt-in; no network by default
//! theme = "system"
//!
//! [hotkeys]
//! "brightness-up" = "Ctrl+Alt+Up"
//!
//! [monitors."GSM-5B09-312NTAB1C234"]
//! name = "Left LG"
//! hw_floor_pct = 10
//! dim_mode = "overlay"
//! min_write_gap_ms = 100
//! sync_group = "desk"
//! excluded = false
//!
//! [monitors."GSM-5B09-312NTAB1C234".inputs]
//! hdmi1 = 17
//! dp1 = 15
//! ```
//!
//! Every field carries a sensible default (`#[serde(default)]` discipline), so
//! an empty file — or any file missing individual keys — deserializes to a fully
//! populated [`Config`]. Unknown keys are *ignored* by this typed view (serde's
//! default) and *preserved* by the format-preserving document layer
//! ([`ConfigDocument`](crate::config::ConfigDocument)); together they give
//! forward compatibility.
//!
//! Maps use [`BTreeMap`] rather than a hash map so serialization order is
//! deterministic — important for stable files, snapshot tests and the
//! round-trip property test.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::migrate::CURRENT_VERSION;

/// How sub-hardware-floor dimming is realised for a display — the serde-facing
/// mirror of [`crate::model::DimMode`].
///
/// # Why a mirror instead of reusing [`crate::model::DimMode`] directly
///
/// [`crate::model::DimMode`] is part of the **frozen** wave-1 domain model and
/// carries no serde derives. Rather than touch that frozen file (which other
/// backends build on in parallel — a serde derive there would also drag `serde`
/// into every consumer of `model`), this module keeps its own serde-deriving
/// mirror and converts at the boundary via the [`From`] impls below. The
/// conversions are *exhaustive* `match`es, so if `model::DimMode` ever grows a
/// variant this file fails to compile until it is handled — the mirror can
/// never silently drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DimMode {
    /// A translucent black overlay window (default; GPU-cheap, HDR-safe).
    #[default]
    Overlay,
    /// A reduced gamma ramp (opt-in; disabled under HDR).
    Gamma,
    /// No software dimming — clamp at the hardware floor.
    Off,
}

impl From<crate::model::DimMode> for DimMode {
    fn from(mode: crate::model::DimMode) -> Self {
        match mode {
            crate::model::DimMode::Overlay => DimMode::Overlay,
            crate::model::DimMode::Gamma => DimMode::Gamma,
            crate::model::DimMode::Off => DimMode::Off,
        }
    }
}

impl From<DimMode> for crate::model::DimMode {
    fn from(mode: DimMode) -> Self {
        match mode {
            DimMode::Overlay => crate::model::DimMode::Overlay,
            DimMode::Gamma => crate::model::DimMode::Gamma,
            DimMode::Off => crate::model::DimMode::Off,
        }
    }
}

/// The UI theme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the operating system's light/dark setting (default).
    #[default]
    System,
    /// Force the light theme.
    Light,
    /// Force the dark theme.
    Dark,
}

/// The accent colour the UI (and both icons) are painted in.
///
/// There is no "black" or "white": an accent paints the slider fill, the toggles
/// and the focus rings, so a black one would vanish on the dark theme and a white
/// one on the light theme. [`Onyx`](Accent::Onyx) is the monochrome option, and it
/// adapts — near-black on light, near-white on dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    /// A warm coral red (default — the original).
    #[default]
    Ruby,
    /// Deep bronze on light, warm amber on dark.
    Gold,
    /// A deep green; mint on dark.
    Emerald,
    /// Navy on light, lifted to azure on dark.
    Sapphire,
    /// Adaptive monochrome.
    Onyx,
}

/// Application-wide settings (the `[general]` table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Launch Duja automatically at login.
    pub autostart: bool,
    /// Check GitHub for a newer release on startup.
    ///
    /// **Opt-in** and off by default: Duja makes no network request unless the
    /// user enables this (plan §6.3).
    pub update_check: bool,
    /// Which UI theme to use.
    pub theme: Theme,
    /// Which accent colour to paint the UI and icons in.
    pub accent: Accent,
}

impl Default for General {
    fn default() -> Self {
        General {
            autostart: true,
            update_check: false,
            theme: Theme::System,
            accent: Accent::Ruby,
        }
    }
}

/// Per-monitor settings (a `[monitors."<StableDisplayId>"]` table).
///
/// Every monitor Duja has seen may have an entry, keyed in the parent map by
/// its [`StableDisplayId`](crate::id::StableDisplayId) *string*. All fields are
/// optional in the file and fall back to the defaults below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    /// A user-chosen display name; `None` falls back to the EDID/OS name.
    pub name: Option<String>,
    /// The lowest hardware brightness percentage Duja will drive before handing
    /// off to software dimming. `0` means "no artificial floor" — drive the
    /// panel across its full hardware range.
    pub hw_floor_pct: u8,
    /// Perceived brightness (%) the panel shows at hardware zero — the anchor of
    /// the perceptual slider scale (see
    /// [`continuum`](crate::continuum)). Lets the software/hardware split feel
    /// natural on any panel; tuned per display in settings.
    pub min_perceived_pct: u8,
    /// How to dim below [`hw_floor_pct`](Self::hw_floor_pct).
    pub dim_mode: DimMode,
    /// Minimum delay between consecutive hardware writes to this display, in
    /// milliseconds (quirk-overridable; some panels need 200 ms+).
    pub min_write_gap_ms: u64,
    /// The sync group this display belongs to; displays in the same group move
    /// together. `None` means the display is independent.
    pub sync_group: Option<String>,
    /// Percentage-point offset applied to the group master when this display
    /// moves with its [`sync_group`](Self::sync_group) (the
    /// [`SyncGroups::fan_out`](crate::sync::SyncGroups::fan_out) semantics).
    /// Meaningless without a group; persisted so offsets survive restarts.
    pub sync_offset: i8,
    /// Whether Duja should ignore this display entirely.
    pub excluded: bool,
    /// Named DDC input sources and their VCP `0x60` codes (e.g. `hdmi1 = 17`).
    ///
    /// Declared last so serialization emits this sub-table after the scalar
    /// keys, keeping the output valid TOML.
    pub inputs: BTreeMap<String, u16>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        MonitorConfig {
            name: None,
            hw_floor_pct: 0,
            min_perceived_pct: 25,
            dim_mode: DimMode::Overlay,
            min_write_gap_ms: 100,
            sync_group: None,
            sync_offset: 0,
            excluded: false,
            inputs: BTreeMap::new(),
        }
    }
}

/// The complete, typed configuration document (schema version 1).
///
/// `schema_version` is declared first so it serializes ahead of the table-valued
/// fields, keeping the emitted TOML valid (scalars must precede table headers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The schema version this document conforms to.
    pub schema_version: u32,
    /// Application-wide settings.
    pub general: General,
    /// Free-form action → key-chord bindings (e.g. `"brightness-up" =
    /// "Ctrl+Alt+Up"`). Interpretation lives with the hotkey subsystem; the
    /// config layer treats both sides as opaque strings.
    pub hotkeys: BTreeMap<String, String>,
    /// Per-monitor settings, keyed by [`StableDisplayId`](crate::id::StableDisplayId)
    /// string.
    pub monitors: BTreeMap<String, MonitorConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schema_version: CURRENT_VERSION,
            general: General::default(),
            hotkeys: BTreeMap::new(),
            monitors: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn empty_toml_yields_full_defaults() {
        // The defaults-completeness guarantee: an empty file is a valid config.
        let cfg: Config = toml_edit::de::from_str("").expect("empty is valid");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn defaults_match_the_plan() {
        let cfg = Config::default();
        assert_eq!(cfg.schema_version, CURRENT_VERSION);
        assert!(cfg.general.autostart, "autostart defaults on");
        assert!(!cfg.general.update_check, "update check is opt-in (off)");
        assert_eq!(cfg.general.theme, Theme::System);
        assert_eq!(cfg.general.accent, Accent::Ruby);
        assert!(cfg.hotkeys.is_empty());
        assert!(cfg.monitors.is_empty());
    }

    #[test]
    fn monitor_defaults_match_the_plan() {
        let m = MonitorConfig::default();
        assert_eq!(m.name, None);
        assert_eq!(m.hw_floor_pct, 0);
        assert_eq!(m.dim_mode, DimMode::Overlay);
        assert_eq!(m.min_write_gap_ms, 100);
        assert_eq!(m.sync_group, None);
        assert_eq!(m.sync_offset, 0);
        assert!(!m.excluded);
        assert!(m.inputs.is_empty());
    }

    #[test]
    fn partial_section_fills_missing_keys_with_defaults() {
        // Only autostart is set; update_check, theme and accent must default.
        let cfg: Config = toml_edit::de::from_str("[general]\nautostart = false\n")
            .expect("valid partial config");
        assert!(!cfg.general.autostart);
        assert!(!cfg.general.update_check);
        assert_eq!(cfg.general.theme, Theme::System);
        assert_eq!(cfg.general.accent, Accent::Ruby);
    }

    #[test]
    fn accent_absent_defaults_to_ruby() {
        // Proves the "no migration needed" claim: a config written before the
        // accent existed still loads, and lands on the colour it was painted in.
        let cfg: Config =
            toml_edit::de::from_str("[general]\ntheme = \"dark\"\n").expect("pre-accent config");
        assert_eq!(cfg.general.theme, Theme::Dark);
        assert_eq!(cfg.general.accent, Accent::Ruby);
    }

    #[test]
    fn accent_round_trips_each_variant() {
        for (accent, token) in [
            (Accent::Ruby, "ruby"),
            (Accent::Gold, "gold"),
            (Accent::Emerald, "emerald"),
            (Accent::Sapphire, "sapphire"),
            (Accent::Onyx, "onyx"),
        ] {
            let toml = toml_edit::ser::to_string(&General {
                accent,
                ..General::default()
            })
            .expect("ser");
            assert!(toml.contains(&format!("accent = \"{token}\"")), "{toml}");
            let back: General = toml_edit::de::from_str(&toml).expect("de");
            assert_eq!(back.accent, accent);
        }
    }

    #[test]
    fn unknown_keys_are_ignored_by_the_typed_view() {
        // Forward compat: a future section deserializes without error.
        let cfg: Config =
            toml_edit::de::from_str("[future_section]\nx = 1\n").expect("unknown keys tolerated");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn dim_mode_serializes_lowercase() {
        let toml = toml_edit::ser::to_string(&MonitorConfig {
            dim_mode: DimMode::Gamma,
            ..MonitorConfig::default()
        })
        .expect("serialize");
        assert!(toml.contains("dim_mode = \"gamma\""), "{toml}");
    }

    #[test]
    fn theme_round_trips_each_variant() {
        for (theme, token) in [
            (Theme::System, "system"),
            (Theme::Light, "light"),
            (Theme::Dark, "dark"),
        ] {
            let toml = toml_edit::ser::to_string(&General {
                theme,
                ..General::default()
            })
            .expect("ser");
            assert!(toml.contains(&format!("theme = \"{token}\"")), "{toml}");
            let back: General = toml_edit::de::from_str(&toml).expect("de");
            assert_eq!(back.theme, theme);
        }
    }

    #[test]
    fn dim_mode_mirrors_model_both_ways() {
        // Every model variant maps to the config mirror and back unchanged.
        for model in [
            crate::model::DimMode::Overlay,
            crate::model::DimMode::Gamma,
            crate::model::DimMode::Off,
        ] {
            let mirror: DimMode = model.into();
            let back: crate::model::DimMode = mirror.into();
            assert_eq!(model, back);
        }
        // And the config default lines up with the model default.
        assert_eq!(
            DimMode::default(),
            DimMode::from(crate::model::DimMode::default())
        );
    }

    // --- round-trip property (plan §4.1: config_roundtrip_arbitrary) ---

    fn any_dim_mode() -> impl Strategy<Value = DimMode> {
        prop_oneof![
            Just(DimMode::Overlay),
            Just(DimMode::Gamma),
            Just(DimMode::Off)
        ]
    }

    fn any_theme() -> impl Strategy<Value = Theme> {
        prop_oneof![Just(Theme::System), Just(Theme::Light), Just(Theme::Dark)]
    }

    /// TOML-safe key: non-empty; the charset covers realistic display ids
    /// (`GSM-5B09-#h1a2b3c4d`) and action names, including chars that force key
    /// quoting (`#`).
    fn key_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9_#-]{1,16}").expect("valid regex")
    }

    /// TOML-safe scalar text; may be empty, and includes chars that force value
    /// quoting (spaces, dots).
    fn text_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9 ._+#/-]{0,24}").expect("valid regex")
    }

    fn any_accent() -> impl Strategy<Value = Accent> {
        prop_oneof![
            Just(Accent::Ruby),
            Just(Accent::Gold),
            Just(Accent::Emerald),
            Just(Accent::Sapphire),
            Just(Accent::Onyx),
        ]
    }

    fn any_general() -> impl Strategy<Value = General> {
        (any::<bool>(), any::<bool>(), any_theme(), any_accent()).prop_map(
            |(autostart, update_check, theme, accent)| General {
                autostart,
                update_check,
                theme,
                accent,
            },
        )
    }

    fn any_monitor() -> impl Strategy<Value = MonitorConfig> {
        (
            proptest::option::of(text_strategy()),
            0u8..=100,
            0u8..=100,
            any_dim_mode(),
            0u64..=100_000,
            proptest::option::of(text_strategy()),
            any::<i8>(),
            any::<bool>(),
            proptest::collection::btree_map(key_strategy(), any::<u16>(), 0..4),
        )
            .prop_map(
                |(
                    name,
                    hw_floor_pct,
                    min_perceived_pct,
                    dim_mode,
                    min_write_gap_ms,
                    sync_group,
                    sync_offset,
                    excluded,
                    inputs,
                )| {
                    MonitorConfig {
                        name,
                        hw_floor_pct,
                        min_perceived_pct,
                        dim_mode,
                        min_write_gap_ms,
                        sync_group,
                        sync_offset,
                        excluded,
                        inputs,
                    }
                },
            )
    }

    fn any_config() -> impl Strategy<Value = Config> {
        (
            any::<u32>(),
            any_general(),
            proptest::collection::btree_map(key_strategy(), text_strategy(), 0..4),
            proptest::collection::btree_map(key_strategy(), any_monitor(), 0..4),
        )
            .prop_map(|(schema_version, general, hotkeys, monitors)| Config {
                schema_version,
                general,
                hotkeys,
                monitors,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// Any valid config survives a TOML serialize -> deserialize round-trip
        /// unchanged.
        #[test]
        fn config_roundtrip_arbitrary(cfg in any_config()) {
            let toml = toml_edit::ser::to_string(&cfg).expect("serialize");
            let back: Config = toml_edit::de::from_str(&toml).expect("deserialize");
            prop_assert_eq!(cfg, back, "round-trip mismatch via:\n{}", toml);
        }
    }
}

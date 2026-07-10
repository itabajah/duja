//! Applying a [`SettingsCommand`] to persisted config, plus the small type
//! mappings between the UI's settings vocabulary and the config/UI enums.
//!
//! The tray wiring owns the stateful side effects (the `Autostart` trait, the
//! engine sender, the dimming re-plan); this module isolates the **pure**,
//! testable part: which config key a command writes, and how the UI's
//! [`ThemeChoice`]/[`DimMode`] map onto the config and flyout theme enums.
//!
//! Config writes go through the format-preserving [`ConfigDocument`] so user
//! comments and unknown keys survive (plan §7). [`persist_config_change`] loads
//! the document from disk, applies exactly the touched key, and writes it back
//! atomically.

// RATIONALE: consumed only by the Windows tray assembly; the pure mappings stay
// cross-platform so their tests run on every CI OS.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

use duja_core::config::{ConfigDocument, DimMode as ConfigDimMode, Theme as ConfigTheme};
use duja_core::model::DimMode;
use duja_ui::{SettingsCommand, ThemeChoice};

/// Apply the config-affecting part of `command` to `doc`.
///
/// Returns `true` when the command changed a config key (the caller then
/// persists), or `false` for commands with no config footprint
/// ([`SettingsCommand::CheckUpdates`], [`SettingsCommand::SetInput`], which are
/// handled entirely through side effects).
pub(crate) fn apply_to_document(doc: &mut ConfigDocument, command: &SettingsCommand) -> bool {
    match command {
        SettingsCommand::SetAutostart(on) => {
            doc.set_autostart(*on);
            true
        }
        SettingsCommand::SetTheme(choice) => {
            doc.set_theme(theme_to_config(*choice));
            true
        }
        SettingsCommand::SetUpdateCheck(on) => {
            doc.set_update_check(*on);
            true
        }
        SettingsCommand::SetMonitorFloor { id, pct } => {
            doc.set_monitor_hw_floor_pct(id.as_str(), *pct);
            true
        }
        SettingsCommand::SetMonitorDimMode { id, mode } => {
            doc.set_monitor_dim_mode(id.as_str(), dim_mode_to_config(*mode));
            true
        }
        SettingsCommand::CheckUpdates
        | SettingsCommand::OpenReleasesPage
        | SettingsCommand::SetInput { .. } => false,
    }
}

/// Load the config document from `path`, apply `command`, and save it back.
///
/// A no-op (returns `Ok(false)`) for commands with no config footprint. On a
/// config-affecting command, returns `Ok(true)` after a successful atomic write.
///
/// # Errors
/// Propagates any load/parse/write error from the config layer.
pub(crate) fn persist_config_change(
    path: &Path,
    command: &SettingsCommand,
) -> Result<bool, duja_core::config::ConfigError> {
    let mut doc = ConfigDocument::load(path)?;
    if !apply_to_document(&mut doc, command) {
        return Ok(false);
    }
    doc.save(path)?;
    Ok(true)
}

/// Map a UI [`ThemeChoice`] onto the config theme enum.
pub(crate) fn theme_to_config(choice: ThemeChoice) -> ConfigTheme {
    match choice {
        ThemeChoice::Auto => ConfigTheme::System,
        ThemeChoice::Light => ConfigTheme::Light,
        ThemeChoice::Dark => ConfigTheme::Dark,
    }
}

/// Map a config theme enum onto the UI [`ThemeChoice`] (to seed the selector).
pub(crate) fn theme_to_choice(theme: ConfigTheme) -> ThemeChoice {
    match theme {
        ConfigTheme::System => ThemeChoice::Auto,
        ConfigTheme::Light => ThemeChoice::Light,
        ConfigTheme::Dark => ThemeChoice::Dark,
    }
}

/// Map a domain [`DimMode`] onto the config mirror (via the existing `From`).
fn dim_mode_to_config(mode: DimMode) -> ConfigDimMode {
    mode.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::id::StableDisplayId;

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
    }

    #[test]
    fn theme_mappings_round_trip() {
        for choice in [ThemeChoice::Auto, ThemeChoice::Light, ThemeChoice::Dark] {
            assert_eq!(theme_to_choice(theme_to_config(choice)), choice);
        }
    }

    #[test]
    fn autostart_and_update_check_write_general_keys() {
        let mut doc = ConfigDocument::defaults();
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::SetAutostart(false)
        ));
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::SetUpdateCheck(true)
        ));
        let cfg = doc.config().expect("typed");
        assert!(!cfg.general.autostart);
        assert!(cfg.general.update_check);
    }

    #[test]
    fn monitor_floor_and_dim_mode_write_per_monitor_keys() {
        let mut doc = ConfigDocument::defaults();
        let display = id("A");
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::SetMonitorFloor {
                id: display.clone(),
                pct: 15,
            }
        ));
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::SetMonitorDimMode {
                id: display.clone(),
                mode: DimMode::Gamma,
            }
        ));
        let cfg = doc.config().expect("typed");
        let monitor = cfg.monitors.get(display.as_str()).expect("entry");
        assert_eq!(monitor.hw_floor_pct, 15);
        assert_eq!(monitor.dim_mode, ConfigDimMode::Gamma);
    }

    #[test]
    fn engine_only_commands_touch_no_config() {
        let mut doc = ConfigDocument::defaults();
        assert!(!apply_to_document(&mut doc, &SettingsCommand::CheckUpdates));
        assert!(!apply_to_document(
            &mut doc,
            &SettingsCommand::SetInput {
                id: id("A"),
                value: 0x11,
            }
        ));
    }

    #[test]
    fn persist_preserves_comments_and_unknown_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep me\nschema_version = 1\n\n[general]\nautostart = true\n\n[future]\nx = 1\n",
        )
        .expect("seed");

        let changed =
            persist_config_change(&path, &SettingsCommand::SetUpdateCheck(true)).expect("persist");
        assert!(changed);

        let saved = std::fs::read_to_string(&path).expect("read");
        assert!(saved.contains("# keep me"), "{saved}");
        assert!(saved.contains("[future]"), "{saved}");
        assert!(saved.contains("update_check = true"), "{saved}");
        // The untouched key survived.
        assert!(saved.contains("autostart = true"), "{saved}");
    }

    #[test]
    fn persist_is_noop_for_engine_only_commands() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        // No file yet; a no-op command must not create or fail.
        let changed = persist_config_change(&path, &SettingsCommand::CheckUpdates).expect("noop");
        assert!(!changed);
        assert!(!path.exists(), "no-op must not write a file");
    }
}

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

use std::path::Path;

use duja_core::config::{
    Accent as ConfigAccent, ConfigDocument, DimMode as ConfigDimMode, Theme as ConfigTheme,
};
use duja_core::model::DimMode;
use duja_ui::{AccentChoice, SettingsCommand, ThemeChoice};

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
        SettingsCommand::SetAccent(choice) => {
            doc.set_accent(accent_to_config(*choice));
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
        SettingsCommand::SetMonitorMinPerceived { id, pct } => {
            doc.set_monitor_min_perceived_pct(id.as_str(), *pct);
            true
        }
        SettingsCommand::SetHotkey {
            action_key,
            binding,
        } => {
            doc.set_hotkey(action_key, binding);
            true
        }
        SettingsCommand::ClearHotkey { action_key } => doc.remove_hotkey(action_key),
        SettingsCommand::CheckUpdates
        | SettingsCommand::OpenReleasesPage
        | SettingsCommand::SetInput { .. } => false,
    }
}

/// Whether `command` can change a config key at all.
///
/// Decided from the command alone, so a caller can answer it **without opening
/// the file**. That is the whole point: [`persist_config_change`] used to
/// `ConfigDocument::load(path)?` first and check the footprint afterwards, so an
/// unreadable `config.toml` made *every* command return `Err` - including
/// `OpenReleasesPage`, which then reported a failed settings save because the
/// user had clicked a link.
///
/// The list is duplicated from [`apply_to_document`]'s last arm, which is a place
/// for drift. Two `cfg(test)` items stop it, so neither is linked here:
/// `the_footprint_predicate_agrees_with_what_the_document_does` checks the two
/// against each other for every variant, and `exhaustive_check` makes a new
/// variant fail the test build until somebody says which side it falls on.
pub(crate) const fn touches_config(command: &SettingsCommand) -> bool {
    !matches!(
        command,
        SettingsCommand::CheckUpdates
            | SettingsCommand::OpenReleasesPage
            | SettingsCommand::SetInput { .. }
    )
}

/// Load the config document from `path`, apply `command`, and save it back.
///
/// A no-op (returns `Ok(false)`) for commands with no config footprint, and it
/// answers that **before** touching the file - see [`touches_config`].
///
/// # Errors
/// Propagates any load/parse/write error from the config layer, for a command
/// that has something to write.
pub(crate) fn persist_config_change(
    path: &Path,
    command: &SettingsCommand,
) -> Result<bool, duja_core::config::ConfigError> {
    if !touches_config(command) {
        return Ok(false);
    }
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

/// Map a UI [`AccentChoice`] onto the config accent enum.
pub(crate) fn accent_to_config(choice: AccentChoice) -> ConfigAccent {
    match choice {
        AccentChoice::Ruby => ConfigAccent::Ruby,
        AccentChoice::Gold => ConfigAccent::Gold,
        AccentChoice::Emerald => ConfigAccent::Emerald,
        AccentChoice::Sapphire => ConfigAccent::Sapphire,
        AccentChoice::Onyx => ConfigAccent::Onyx,
    }
}

/// Map a config accent enum onto the UI [`AccentChoice`] (to seed the selector and
/// the palette on startup).
pub(crate) fn accent_to_choice(accent: ConfigAccent) -> AccentChoice {
    match accent {
        ConfigAccent::Ruby => AccentChoice::Ruby,
        ConfigAccent::Gold => AccentChoice::Gold,
        ConfigAccent::Emerald => AccentChoice::Emerald,
        ConfigAccent::Sapphire => AccentChoice::Sapphire,
        ConfigAccent::Onyx => AccentChoice::Onyx,
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

    /// Every `SettingsCommand`, so the two footprint lists cannot drift apart.
    ///
    /// Restated rather than derived: a test that asked `apply_to_document` what
    /// `touches_config` should say would be comparing a thing with itself.
    ///
    /// **This list is a promise, not a constraint**, and an earlier version of
    /// this doc claimed the opposite - that a new variant missing from it would
    /// fail to compile. A `vec![]` literal cannot do that, and a review measured
    /// it: deleting an entry left all eleven tests green. What *does* fail to
    /// compile is [`exhaustive_check`] below, which is the constraint this needs
    /// and is why it exists at all.
    fn every_command() -> Vec<SettingsCommand> {
        vec![
            SettingsCommand::SetAutostart(true),
            SettingsCommand::SetTheme(ThemeChoice::Dark),
            SettingsCommand::SetAccent(AccentChoice::default()),
            SettingsCommand::SetUpdateCheck(true),
            SettingsCommand::SetMonitorFloor {
                id: id("A"),
                pct: 10,
            },
            SettingsCommand::SetMonitorDimMode {
                id: id("A"),
                mode: DimMode::Overlay,
            },
            SettingsCommand::SetMonitorMinPerceived {
                id: id("A"),
                pct: 10,
            },
            SettingsCommand::SetHotkey {
                action_key: "brightness_up".to_owned(),
                binding: "ctrl+alt+up".to_owned(),
            },
            SettingsCommand::ClearHotkey {
                action_key: "brightness_up".to_owned(),
            },
            SettingsCommand::CheckUpdates,
            SettingsCommand::OpenReleasesPage,
            SettingsCommand::SetInput {
                id: id("A"),
                value: 0x11,
            },
        ]
    }

    /// A `match` with no wildcard, so a new `SettingsCommand` variant stops the
    /// **test build** until somebody decides which side of the footprint line it
    /// falls on - and adds it to [`every_command`], which nothing else can force.
    ///
    /// The arms do nothing; the exhaustiveness is the whole point. Without it a
    /// new no-footprint variant would default to "touches config", open the file,
    /// and raise the false banner this predicate exists to prevent, with the
    /// drift test green.
    #[expect(
        clippy::match_same_arms,
        reason = "the arms are deliberately identical; the exhaustiveness is what is being asserted"
    )]
    fn exhaustive_check(command: &SettingsCommand) {
        match command {
            SettingsCommand::SetAutostart(_)
            | SettingsCommand::SetTheme(_)
            | SettingsCommand::SetAccent(_)
            | SettingsCommand::SetUpdateCheck(_)
            | SettingsCommand::SetMonitorFloor { .. }
            | SettingsCommand::SetMonitorDimMode { .. }
            | SettingsCommand::SetMonitorMinPerceived { .. }
            | SettingsCommand::SetHotkey { .. }
            | SettingsCommand::ClearHotkey { .. } => {}
            SettingsCommand::CheckUpdates
            | SettingsCommand::OpenReleasesPage
            | SettingsCommand::SetInput { .. } => {}
        }
    }

    /// [`touches_config`] and [`apply_to_document`] must agree about every
    /// command, because the first is now what decides whether the file is opened
    /// at all.
    ///
    /// They disagreeing is how a no-footprint command would start reporting a
    /// failed save again, which is the defect this predicate was extracted to
    /// fix: `persist_config_change` loaded the document *first*, so an unreadable
    /// `config.toml` made `OpenReleasesPage` return `Err` and the settings window
    /// told the user their click had failed to save something.
    ///
    /// `ClearHotkey` is the interesting row and the reason this is not an
    /// equality: it *can* touch config and, against a document with nothing
    /// bound, does not. So the predicate is the weaker claim - "might" - and the
    /// assertion is one-directional.
    #[test]
    fn the_footprint_predicate_agrees_with_what_the_document_does() {
        for command in every_command() {
            exhaustive_check(&command);
            let mut doc = ConfigDocument::parse("").expect("an empty document parses");
            let wrote = apply_to_document(&mut doc, &command);
            assert!(
                !wrote || touches_config(&command),
                "{command:?} changed a key that `touches_config` says it cannot, so the file would never be opened for it"
            );
            if !touches_config(&command) {
                assert!(
                    !wrote,
                    "{command:?} is declared footprint-free and wrote anyway"
                );
            }
        }
    }

    #[test]
    fn theme_mappings_round_trip() {
        for choice in [ThemeChoice::Auto, ThemeChoice::Light, ThemeChoice::Dark] {
            assert_eq!(theme_to_choice(theme_to_config(choice)), choice);
        }
    }

    #[test]
    fn accent_mappings_round_trip() {
        for choice in duja_ui::ACCENT_ORDER {
            assert_eq!(accent_to_choice(accent_to_config(choice)), choice);
        }
    }

    #[test]
    fn set_accent_command_writes_the_document() {
        let mut doc = ConfigDocument::defaults();
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::SetAccent(AccentChoice::Emerald)
        ));
        assert_eq!(
            doc.config().expect("typed").general.accent,
            ConfigAccent::Emerald
        );
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
    fn set_monitor_min_perceived_writes_the_anchor_key() {
        let mut doc = ConfigDocument::defaults();
        let display = id("A");
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::SetMonitorMinPerceived {
                id: display.clone(),
                pct: 35,
            }
        ));
        let cfg = doc.config().expect("typed");
        assert_eq!(
            cfg.monitors
                .get(display.as_str())
                .expect("entry")
                .min_perceived_pct,
            35
        );
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
    fn set_and_clear_hotkey_write_and_remove_binding() {
        let mut doc = ConfigDocument::defaults();
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::SetHotkey {
                action_key: "brightness_up".to_owned(),
                binding: "Ctrl+Alt+UP".to_owned(),
            }
        ));
        assert_eq!(
            doc.config()
                .expect("typed")
                .hotkeys
                .get("brightness_up")
                .map(String::as_str),
            Some("Ctrl+Alt+UP")
        );
        // Clearing an existing binding reports a change and removes it.
        assert!(apply_to_document(
            &mut doc,
            &SettingsCommand::ClearHotkey {
                action_key: "brightness_up".to_owned(),
            }
        ));
        assert!(doc.config().expect("typed").hotkeys.is_empty());
        // Clearing an absent binding is a no-op (no config change).
        assert!(!apply_to_document(
            &mut doc,
            &SettingsCommand::ClearHotkey {
                action_key: "brightness_up".to_owned(),
            }
        ));
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

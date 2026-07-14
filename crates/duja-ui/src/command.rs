//! The command vocabulary a view-model emits back to the shell.
//!
//! A [`UiCommand`] is the **only** thing that crosses out of a view-model in
//! response to a user action: plain data, no Slint and no engine types. Wave 2
//! (`duja-app` assembly) maps each variant onto an
//! `EngineCommand` — `SetLevel` ⇒ `EngineCommand::SetUserLevel`, `Refresh` ⇒
//! `EngineCommand::RefreshNow` — so the UI never depends on the engine crate.

use duja_core::id::StableDisplayId;
use duja_core::model::DimMode;

use crate::accent::AccentChoice;

/// A user-driven intent produced by a view-model, for the shell to forward.
///
/// The set is deliberately tiny: everything the P4 flyout can *do* to the
/// engine is either "set one display's level" or "re-enumerate now". State that
/// only affects presentation (link toggle, theme) never becomes a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Set the unified user brightness level (0..=100) for one display.
    ///
    /// The percent is already clamped to `0..=100` by the emitting view-model.
    SetLevel {
        /// The display to adjust.
        id: StableDisplayId,
        /// Desired level in percent, guaranteed `0..=100`.
        pct: u8,
    },
    /// Run one enumeration pass immediately (the refresh affordance).
    Refresh,
    /// Open the settings window (the flyout's gear button).
    OpenSettings,
    /// Enable or disable software dimming for one display (the flyout's
    /// per-row dimming toggle).
    ///
    /// `on` maps to the display's configured dim mode (default overlay) and
    /// `off` maps to [`DimMode::Off`]; the app persists the change and re-plans
    /// the dimmer batch.
    SetDimmingEnabled {
        /// The display to adjust.
        id: StableDisplayId,
        /// Whether software dimming should be engaged below the hardware floor.
        on: bool,
    },
}

/// The theme preference offered by the settings window.
///
/// A UI-level choice distinct from the flyout's resolved [`Theme`](crate::Theme)
/// (which is only ever `Light`/`Dark`): `Auto` follows the OS. The app maps this
/// onto the config's theme enum and re-resolves the rendered palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    /// Follow the operating system's light/dark preference.
    Auto,
    /// Force the light palette.
    Light,
    /// Force the dark palette.
    Dark,
}

/// A command emitted by the settings view-model, for the app to apply.
///
/// Like [`UiCommand`], these are plain data — no Slint, no engine, no config
/// types beyond the shared [`StableDisplayId`]/[`DimMode`] domain values. The
/// tray wiring maps each variant onto a config write (through the
/// format-preserving document layer) and/or an engine command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsCommand {
    /// Enable or disable launch at login (applied through the platform
    /// `Autostart` trait and mirrored into the config).
    SetAutostart(bool),
    /// Change the theme preference.
    SetTheme(ThemeChoice),
    /// Change the accent colour the whole app is painted in (and the icons).
    SetAccent(AccentChoice),
    /// Turn the opt-in update check on or off (config only; no network).
    SetUpdateCheck(bool),
    /// Run the update check now (the manual "Check for updates" action).
    CheckUpdates,
    /// Open the GitHub releases page in the browser (shown only once an update
    /// is available — Duja never downloads anything itself).
    OpenReleasesPage,
    /// Set a display's hardware-floor percentage (0..=50).
    SetMonitorFloor {
        /// The display to adjust.
        id: StableDisplayId,
        /// The new floor, guaranteed `0..=50`.
        pct: u8,
    },
    /// Set a display's sub-floor dim mode.
    SetMonitorDimMode {
        /// The display to adjust.
        id: StableDisplayId,
        /// The chosen mode (a gamma choice is dropped by the caller when the
        /// display is under the HDR guard; the VM never offers it there).
        mode: DimMode,
    },
    /// Set a display's perceptual scale anchor (`min_perceived_pct`): the
    /// perceived brightness the panel shows at hardware zero. Tunes how the
    /// slider splits software vs hardware so it feels natural per panel.
    SetMonitorMinPerceived {
        /// The display to adjust.
        id: StableDisplayId,
        /// The new anchor, guaranteed `5..=60` by the emitting view-model.
        pct: u8,
    },
    /// Switch a display's active input source (raw MCCS `0x60` code).
    SetInput {
        /// The display to switch.
        id: StableDisplayId,
        /// The raw input-source code to select.
        value: u8,
    },
    /// Bind (or rebind) a global hotkey for an action to an accelerator string.
    ///
    /// `action_key` is the config-table key for the action (e.g.
    /// `"brightness_up"`); `binding` is the accelerator as captured (e.g.
    /// `"Ctrl+Alt+Up"`). The app parses and validates the binding, persists it,
    /// and re-registers the live hotkeys.
    SetHotkey {
        /// The config-table key of the action to bind.
        action_key: String,
        /// The accelerator string to bind it to.
        binding: String,
    },
    /// Clear the global hotkey bound to an action (`action_key` is its
    /// config-table key). The app removes the binding and re-registers.
    ClearHotkey {
        /// The config-table key of the action to unbind.
        action_key: String,
    },
}

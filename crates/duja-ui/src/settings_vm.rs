//! The settings view-model: config + runtime state in, presentation rows and
//! [`SettingsCommand`]s out.
//!
//! Like [`FlyoutVm`](crate::FlyoutVm), [`SettingsVm`] is pure Rust with **zero
//! Slint types in its signatures** (the architecture rule, plan §4.4). It is fed
//! the typed [`Config`], the connected [`DisplaySnapshot`]s, the once-per-run HDR
//! gamma verdict, the live autostart state, and the resolved hotkey list; it
//! exposes ready-to-render sections and turns every widget action into a
//! [`SettingsCommand`] the app maps onto a config write and/or engine command.
//!
//! # What lives here vs. the app
//!
//! Everything the *core* crate can express is built here (per-monitor sections
//! from [`Config`] + snapshots, input labels via [`input_source`]). Two inputs
//! are computed app-side and handed in because they need crates the UI does not
//! depend on: the **actual** autostart state (the platform `Autostart` trait) and
//! the **resolved hotkey rows + conflicts** (the app's hotkey parser).

use duja_core::config::{Config, MonitorConfig};
use duja_core::id::StableDisplayId;
use duja_core::input_source;
use duja_core::model::{DimMode, DisplaySnapshot};

use crate::accent::{ACCENT_ORDER, AccentChoice};
use crate::command::{SettingsCommand, ThemeChoice};

/// The largest hardware floor the settings slider offers (percentage points).
///
/// Above this the display would spend most of its slider range below the
/// hardware floor, which is not a useful configuration; 50 % is the plan's cap.
pub const MAX_FLOOR_PCT: u8 = 50;

/// The inclusive range the perceptual-anchor (`min_perceived_pct`) slider offers.
///
/// Below 5 % the panel would be claimed near-black at hardware zero (unrealistic);
/// above 60 % the hardware section would shrink too far. The core clamps more
/// loosely (`..=95`); this is the sensible UI band.
pub const MIN_PERCEIVED_RANGE: (u8, u8) = (5, 60);

/// The dim-mode options a per-monitor selector offers, in display order.
///
/// Fixed so the selector index maps deterministically onto a [`DimMode`].
pub const DIM_MODE_ORDER: [DimMode; 3] = [DimMode::Overlay, DimMode::Gamma, DimMode::Off];

/// The theme options the general selector offers, in display order.
pub const THEME_ORDER: [ThemeChoice; 3] =
    [ThemeChoice::Auto, ThemeChoice::Light, ThemeChoice::Dark];

/// The result line shown beside the manual "Check for updates" action.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateStatus {
    /// The update check is off (opt-in toggle disabled).
    #[default]
    Disabled,
    /// Enabled but not yet run this session.
    Idle,
    /// A check is in flight.
    Checking,
    /// The running build is current.
    UpToDate,
    /// A newer release is available; carries its tag.
    Available {
        /// The newer release's tag (e.g. `v1.2.0`).
        version: String,
    },
    /// The last check could not be completed.
    Failed,
}

/// One selectable input source: its raw MCCS code and a human label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputChoice {
    /// The raw VCP `0x60` value.
    pub code: u8,
    /// The display label (slug if known, else hex).
    pub label: String,
}

/// One per-monitor settings section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorSection {
    /// Durable identity, used to address the emitted commands.
    pub id: StableDisplayId,
    /// The display name (user override or EDID/OS name).
    pub name: String,
    /// Current hardware-floor percentage, `0..=`[`MAX_FLOOR_PCT`].
    pub floor_pct: u8,
    /// Current perceptual-scale anchor (perceived brightness at hardware zero),
    /// clamped to [`MIN_PERCEIVED_RANGE`].
    pub min_perceived_pct: u8,
    /// Current sub-floor dim mode.
    pub dim_mode: DimMode,
    /// Whether gamma is offered: `false` under the HDR guard, where the gamma
    /// option is shown disabled with a tooltip and a selection is rejected.
    pub gamma_available: bool,
    /// The allowed input sources; empty when input switching is unsupported (the
    /// dropdown is then hidden).
    pub inputs: Vec<InputChoice>,
    /// The selector index of the input the user last picked this session, or
    /// `None` if none has been chosen yet.
    ///
    /// A [`DisplaySnapshot`] carries no active-input readback, so this starts
    /// `None` (rendered as an *empty* dropdown) rather than a misleading `0` that
    /// would falsely claim the first allowed input is the live one.
    /// [`select_monitor_input`](SettingsVm::select_monitor_input) records the
    /// user's choice here so the dropdown sticks on it and arrow-key navigation
    /// continues from it, instead of snapping back to index 0.
    pub selected_input_index: Option<usize>,
}

impl MonitorSection {
    /// The selector index of the current [`dim_mode`](Self::dim_mode).
    #[must_use]
    pub fn dim_mode_index(&self) -> usize {
        DIM_MODE_ORDER
            .iter()
            .position(|m| *m == self.dim_mode)
            .unwrap_or(0)
    }
}

/// One configured-hotkey row (now editable: record / clear).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyRow {
    /// The config-table key for this action (e.g. `brightness_up`), used to
    /// address the emitted [`SettingsCommand::SetHotkey`] /
    /// [`SettingsCommand::ClearHotkey`].
    pub action_key: String,
    /// The action label (e.g. `Brightness up`).
    pub action_label: String,
    /// The bound accelerator, as written (e.g. `Ctrl+Alt+Up`); empty when unbound.
    pub binding: String,
    /// Whether this binding collides with another (shown as a warning).
    pub conflicted: bool,
    /// Whether the OS refused to register this binding (already owned by another
    /// app); shown as unavailable feedback in the row.
    pub unavailable: bool,
}

/// The modifier state of a captured key chord (bundled so the capture API does
/// not take a fistful of bools).
// RATIONALE: a keyboard modifier set is inherently four independent booleans
// (Ctrl/Alt/Shift/Super); a bitflag would only re-encode the same four bits and
// obscure the field-per-modifier the Slint capture boundary fills in directly.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureModifiers {
    /// Control held.
    pub ctrl: bool,
    /// Alt / Option held.
    pub alt: bool,
    /// Shift held.
    pub shift: bool,
    /// Super / Windows / Command held.
    pub meta: bool,
}

/// Assemble an accelerator string from a captured key chord.
///
/// `key` is the (already Slint-normalized) canonical key token, or `None` while
/// only modifiers are held — a chord with no key yet is *pending*, so this
/// returns `None` (nothing to bind). Modifiers are emitted in the canonical
/// order (`Ctrl`, `Alt`, `Shift`, `Super`) the app's parser prints, so the
/// produced string parses back to the same accelerator.
///
/// # Examples
/// ```
/// use duja_ui::settings_vm::{accelerator_string, CaptureModifiers};
/// let ctrl_alt = CaptureModifiers { ctrl: true, alt: true, ..Default::default() };
/// assert_eq!(
///     accelerator_string(ctrl_alt, Some("Up")).as_deref(),
///     Some("Ctrl+Alt+UP"),
/// );
/// // Only modifiers held so far: still recording, nothing to bind.
/// let ctrl = CaptureModifiers { ctrl: true, ..Default::default() };
/// assert_eq!(accelerator_string(ctrl, None), None);
/// ```
#[must_use]
pub fn accelerator_string(mods: CaptureModifiers, key: Option<&str>) -> Option<String> {
    let token = key.map(str::trim).filter(|t| !t.is_empty())?;
    let mut out = String::new();
    for (held, name) in [
        (mods.ctrl, "Ctrl"),
        (mods.alt, "Alt"),
        (mods.shift, "Shift"),
        (mods.meta, "Super"),
    ] {
        if held {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&token.to_ascii_uppercase());
    Some(out)
}

/// The settings view-model.
// RATIONALE: four independent boolean flags — autostart on / supported, update
// check on, and the resolved-dark palette — not a state machine an enum would
// model (a bitflag would only re-encode the same four bits). Same shape as
// `CaptureModifiers`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsVm {
    autostart_on: bool,
    autostart_supported: bool,
    theme: ThemeChoice,
    dark: bool,
    /// The accent the palette is painted in. Resolved against `dark` by the shell,
    /// so a theme change re-pushes the right variants for free.
    accent: AccentChoice,
    update_check_on: bool,
    update_status: UpdateStatus,
    monitors: Vec<MonitorSection>,
    hotkeys: Vec<HotkeyRow>,
}

impl Default for SettingsVm {
    fn default() -> Self {
        SettingsVm::new()
    }
}

impl SettingsVm {
    /// An empty settings view-model (autostart off/supported, auto theme, update
    /// check off, no monitors, no hotkeys).
    #[must_use]
    pub fn new() -> Self {
        SettingsVm {
            autostart_on: false,
            autostart_supported: true,
            theme: ThemeChoice::Auto,
            dark: true,
            accent: AccentChoice::default(),
            update_check_on: false,
            update_status: UpdateStatus::Disabled,
            monitors: Vec::new(),
            hotkeys: Vec::new(),
        }
    }

    // --- population (called by the app when it opens/refreshes settings) ---

    /// Set the general-section state.
    ///
    /// `autostart_on` is the **actual** launch-at-login state (from the platform
    /// `Autostart` trait), which may differ from the config mirror; passing the
    /// live value keeps the toggle honest. `autostart_supported` is `false` where
    /// the platform has no autostart backend, disabling the toggle.
    ///
    /// `dark` is the **resolved** palette (`true` = dark) the app computes from
    /// the theme preference and the OS setting — the same value it pushes to the
    /// flyout — so the settings window renders the identical light/dark palette.
    /// It is distinct from `theme` (the raw Auto/Light/Dark *preference* the
    /// selector shows), which cannot resolve `Auto` without the OS state.
    // RATIONALE: seeds the four independent general-section flags in one call
    // (autostart on / supported, update-check on, resolved-dark); grouping them
    // into a struct would only move the same four bools behind a name.
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn set_general(
        &mut self,
        autostart_on: bool,
        autostart_supported: bool,
        theme: ThemeChoice,
        accent: AccentChoice,
        update_check_on: bool,
        dark: bool,
    ) {
        self.autostart_on = autostart_on;
        self.autostart_supported = autostart_supported;
        self.theme = theme;
        self.accent = accent;
        self.dark = dark;
        self.update_check_on = update_check_on;
        // Keep the status consistent with the toggle: turning the check off
        // shows "disabled"; turning it on (from disabled) shows "idle".
        if !update_check_on {
            self.update_status = UpdateStatus::Disabled;
        } else if self.update_status == UpdateStatus::Disabled {
            self.update_status = UpdateStatus::Idle;
        }
    }

    /// Rebuild the per-monitor sections from fresh snapshots and the config.
    ///
    /// `gamma_allowed` is the once-per-run HDR verdict: `false` marks every
    /// section's gamma option unavailable. Sections follow the snapshot order.
    pub fn set_displays(
        &mut self,
        snapshots: &[DisplaySnapshot],
        config: &Config,
        gamma_allowed: bool,
    ) {
        self.monitors = snapshots
            .iter()
            .map(|snap| {
                build_section(
                    snap,
                    &monitor_config(config, snap.id.as_str()),
                    gamma_allowed,
                )
            })
            .collect();
    }

    /// Set the read-only hotkey rows (built app-side by the hotkey resolver).
    pub fn set_hotkeys(&mut self, hotkeys: Vec<HotkeyRow>) {
        self.hotkeys = hotkeys;
    }

    /// Set the update-check result line (called after a check completes, or to
    /// reflect a status change).
    pub fn set_update_status(&mut self, status: UpdateStatus) {
        self.update_status = status;
    }

    // --- accessors (rendered by the shell) ---

    /// Whether launch-at-login is currently on.
    #[must_use]
    pub fn autostart_on(&self) -> bool {
        self.autostart_on
    }

    /// Whether the platform supports autostart (else the toggle is disabled).
    #[must_use]
    pub fn autostart_supported(&self) -> bool {
        self.autostart_supported
    }

    /// The current theme preference.
    #[must_use]
    pub fn theme(&self) -> ThemeChoice {
        self.theme
    }

    /// The selector index of the current theme.
    #[must_use]
    pub fn theme_index(&self) -> usize {
        THEME_ORDER
            .iter()
            .position(|t| *t == self.theme)
            .unwrap_or(0)
    }

    /// The current accent colour.
    #[must_use]
    pub fn accent(&self) -> AccentChoice {
        self.accent
    }

    /// The selector index of the current accent.
    #[must_use]
    pub fn accent_index(&self) -> usize {
        ACCENT_ORDER
            .iter()
            .position(|a| *a == self.accent)
            .unwrap_or(0)
    }

    /// The resolved palette to render: `true` for the dark palette. Distinct from
    /// [`theme`](Self::theme) (the raw Auto/Light/Dark *preference* the selector
    /// shows) — the app resolves `Auto` against the OS and passes the result in
    /// via [`set_general`](Self::set_general) so the settings window's palette
    /// tracks the flyout's.
    #[must_use]
    pub fn dark(&self) -> bool {
        self.dark
    }

    /// Whether the opt-in update check is on.
    #[must_use]
    pub fn update_check_on(&self) -> bool {
        self.update_check_on
    }

    /// The current update-check result line.
    #[must_use]
    pub fn update_status(&self) -> &UpdateStatus {
        &self.update_status
    }

    /// Whether an update is available (drives the "Open releases page" action).
    #[must_use]
    pub fn update_available(&self) -> bool {
        matches!(self.update_status, UpdateStatus::Available { .. })
    }

    /// The per-monitor sections, in snapshot order.
    #[must_use]
    pub fn monitors(&self) -> &[MonitorSection] {
        &self.monitors
    }

    /// The read-only hotkey rows.
    #[must_use]
    pub fn hotkeys(&self) -> &[HotkeyRow] {
        &self.hotkeys
    }

    // --- actions (emit a command; update local state optimistically) ---

    /// Toggle launch-at-login. Returns `None` when autostart is unsupported (the
    /// toggle is inert there).
    pub fn toggle_autostart(&mut self, on: bool) -> Option<SettingsCommand> {
        if !self.autostart_supported {
            return None;
        }
        self.autostart_on = on;
        Some(SettingsCommand::SetAutostart(on))
    }

    /// Choose the theme by selector index (into [`THEME_ORDER`]). Out-of-range
    /// indices are ignored.
    pub fn select_theme(&mut self, index: usize) -> Option<SettingsCommand> {
        let choice = *THEME_ORDER.get(index)?;
        self.theme = choice;
        Some(SettingsCommand::SetTheme(choice))
    }

    /// Pick an accent colour by selector index. Out of range is ignored.
    ///
    /// Mirrors [`select_theme`](Self::select_theme): the local state updates
    /// optimistically and the app persists the returned command.
    pub fn select_accent(&mut self, index: usize) -> Option<SettingsCommand> {
        let choice = *ACCENT_ORDER.get(index)?;
        self.accent = choice;
        Some(SettingsCommand::SetAccent(choice))
    }

    /// Toggle the opt-in update check.
    pub fn toggle_update_check(&mut self, on: bool) -> SettingsCommand {
        self.update_check_on = on;
        self.update_status = if on {
            UpdateStatus::Idle
        } else {
            UpdateStatus::Disabled
        };
        SettingsCommand::SetUpdateCheck(on)
    }

    /// Request a manual update check. Returns `None` (and does nothing) when the
    /// check is off — the button is disabled there. Moves the status to
    /// [`UpdateStatus::Checking`].
    pub fn request_update_check(&mut self) -> Option<SettingsCommand> {
        if !self.update_check_on {
            return None;
        }
        self.update_status = UpdateStatus::Checking;
        Some(SettingsCommand::CheckUpdates)
    }

    /// The command for the "Open releases page" action.
    #[must_use]
    pub fn open_releases_page(&self) -> SettingsCommand {
        SettingsCommand::OpenReleasesPage
    }

    /// Set a monitor's hardware floor (clamped to `0..=`[`MAX_FLOOR_PCT`]) by
    /// section index. Out-of-range indices are ignored.
    pub fn set_monitor_floor(&mut self, monitor_index: usize, pct: u8) -> Option<SettingsCommand> {
        let section = self.monitors.get_mut(monitor_index)?;
        let clamped = pct.min(MAX_FLOOR_PCT);
        section.floor_pct = clamped;
        Some(SettingsCommand::SetMonitorFloor {
            id: section.id.clone(),
            pct: clamped,
        })
    }

    /// Set a monitor's perceptual anchor (`min_perceived_pct`), clamped to
    /// [`MIN_PERCEIVED_RANGE`], by section index. Out-of-range indices are ignored.
    pub fn set_monitor_min_perceived(
        &mut self,
        monitor_index: usize,
        pct: u8,
    ) -> Option<SettingsCommand> {
        let section = self.monitors.get_mut(monitor_index)?;
        let clamped = pct.clamp(MIN_PERCEIVED_RANGE.0, MIN_PERCEIVED_RANGE.1);
        section.min_perceived_pct = clamped;
        Some(SettingsCommand::SetMonitorMinPerceived {
            id: section.id.clone(),
            pct: clamped,
        })
    }

    /// Choose a monitor's dim mode by section index and option index (into
    /// [`DIM_MODE_ORDER`]). A gamma choice on a section where gamma is
    /// unavailable is rejected (returns `None`, leaving the mode unchanged).
    pub fn select_monitor_dim_mode(
        &mut self,
        monitor_index: usize,
        option_index: usize,
    ) -> Option<SettingsCommand> {
        let mode = *DIM_MODE_ORDER.get(option_index)?;
        let section = self.monitors.get_mut(monitor_index)?;
        if mode == DimMode::Gamma && !section.gamma_available {
            return None;
        }
        section.dim_mode = mode;
        Some(SettingsCommand::SetMonitorDimMode {
            id: section.id.clone(),
            mode,
        })
    }

    /// Switch a monitor's input by section index and input-option index,
    /// recording the choice as the section's
    /// [`selected_input_index`](MonitorSection::selected_input_index) so the
    /// dropdown reflects it. Ignored for an out-of-range monitor or input index.
    pub fn select_monitor_input(
        &mut self,
        monitor_index: usize,
        input_index: usize,
    ) -> Option<SettingsCommand> {
        let section = self.monitors.get_mut(monitor_index)?;
        let code = section.inputs.get(input_index)?.code;
        section.selected_input_index = Some(input_index);
        Some(SettingsCommand::SetInput {
            id: section.id.clone(),
            value: code,
        })
    }

    /// Capture a recorded key chord for the hotkey row at `row_index`, returning
    /// the bind command.
    ///
    /// Returns `None` (still *pending*) when only modifiers are held (`key ==
    /// None`) or the index is out of range — the recorder keeps listening. When a
    /// key is present, emits a [`SettingsCommand::SetHotkey`] addressed to the
    /// row's action; the app parses, validates, persists, and re-registers.
    #[must_use]
    pub fn capture_hotkey(
        &self,
        row_index: usize,
        mods: CaptureModifiers,
        key: Option<&str>,
    ) -> Option<SettingsCommand> {
        let row = self.hotkeys.get(row_index)?;
        let binding = accelerator_string(mods, key)?;
        Some(SettingsCommand::SetHotkey {
            action_key: row.action_key.clone(),
            binding,
        })
    }

    /// Clear the binding for the hotkey row at `row_index`. Returns `None` for an
    /// out-of-range index.
    #[must_use]
    pub fn clear_hotkey(&self, row_index: usize) -> Option<SettingsCommand> {
        let row = self.hotkeys.get(row_index)?;
        Some(SettingsCommand::ClearHotkey {
            action_key: row.action_key.clone(),
        })
    }
}

/// The per-monitor settings for `id`, or the schema defaults if the file has no
/// entry for it.
fn monitor_config(config: &Config, id: &str) -> MonitorConfig {
    config.monitors.get(id).cloned().unwrap_or_default()
}

/// Build one section from a snapshot and its resolved monitor config.
fn build_section(
    snap: &DisplaySnapshot,
    monitor: &MonitorConfig,
    gamma_allowed: bool,
) -> MonitorSection {
    let inputs = snap
        .capabilities
        .allowed_inputs
        .iter()
        .map(|&code| InputChoice {
            code,
            label: input_source::label(code),
        })
        .collect();
    MonitorSection {
        id: snap.id.clone(),
        name: monitor.name.clone().unwrap_or_else(|| snap.name.clone()),
        floor_pct: monitor.hw_floor_pct.min(MAX_FLOOR_PCT),
        min_perceived_pct: monitor
            .min_perceived_pct
            .clamp(MIN_PERCEIVED_RANGE.0, MIN_PERCEIVED_RANGE.1),
        dim_mode: monitor.dim_mode.into(),
        gamma_available: gamma_allowed,
        inputs,
        // No active-input readback exists in a snapshot; start unselected.
        selected_input_index: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::config::{DimMode as ConfigDimMode, MonitorConfig};
    use duja_core::model::{Capabilities, DisplayKind};
    use std::collections::BTreeSet;

    fn snap(serial: &str, name: &str, inputs: Vec<u8>) -> DisplaySnapshot {
        DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap(),
            name: name.to_owned(),
            kind: DisplayKind::ExternalDdc,
            user_level_pct: 50,
            capabilities: Capabilities {
                features: BTreeSet::new(),
                hardware_range: true,
                raw_capabilities: None,
                allowed_inputs: inputs,
            },
        }
    }

    fn config_with_monitor(serial: &str, floor: u8, mode: ConfigDimMode) -> Config {
        let mut config = Config::default();
        let id = StableDisplayId::from_parts("GSM", 0x0001, Some(serial))
            .unwrap()
            .as_str()
            .to_owned();
        config.monitors.insert(
            id,
            MonitorConfig {
                hw_floor_pct: floor,
                dim_mode: mode,
                ..MonitorConfig::default()
            },
        );
        config
    }

    #[test]
    fn empty_vm_defaults() {
        let vm = SettingsVm::new();
        assert!(!vm.autostart_on());
        assert!(vm.autostart_supported());
        assert_eq!(vm.theme(), ThemeChoice::Auto);
        assert!(!vm.update_check_on());
        assert_eq!(vm.update_status(), &UpdateStatus::Disabled);
        assert!(vm.monitors().is_empty());
        assert!(vm.hotkeys().is_empty());
    }

    #[test]
    fn set_general_seeds_state_and_status() {
        let mut vm = SettingsVm::new();
        // A fresh VM defaults to the dark palette (matches Palette.dark's default).
        assert!(vm.dark());
        vm.set_general(
            true,
            true,
            ThemeChoice::Light,
            AccentChoice::Ruby,
            true,
            false,
        );
        assert!(vm.autostart_on());
        assert_eq!(vm.theme(), ThemeChoice::Light);
        // The resolved palette is carried independently of the raw preference.
        assert!(!vm.dark());
        assert!(vm.update_check_on());
        // Enabling the check from disabled moves to idle.
        assert_eq!(vm.update_status(), &UpdateStatus::Idle);
        // Disabling shows disabled again; a dark resolution is carried through.
        vm.set_general(
            true,
            true,
            ThemeChoice::Dark,
            AccentChoice::Ruby,
            false,
            true,
        );
        assert!(vm.dark());
        assert_eq!(vm.update_status(), &UpdateStatus::Disabled);
    }

    #[test]
    fn toggle_autostart_emits_and_updates() {
        let mut vm = SettingsVm::new();
        assert_eq!(
            vm.toggle_autostart(true),
            Some(SettingsCommand::SetAutostart(true))
        );
        assert!(vm.autostart_on());
    }

    #[test]
    fn toggle_autostart_inert_when_unsupported() {
        let mut vm = SettingsVm::new();
        vm.set_general(
            false,
            false,
            ThemeChoice::Auto,
            AccentChoice::Ruby,
            false,
            true,
        );
        assert_eq!(vm.toggle_autostart(true), None);
        assert!(!vm.autostart_on());
    }

    #[test]
    fn select_theme_maps_index_to_choice() {
        let mut vm = SettingsVm::new();
        assert_eq!(
            vm.select_theme(1),
            Some(SettingsCommand::SetTheme(ThemeChoice::Light))
        );
        assert_eq!(vm.theme(), ThemeChoice::Light);
        assert_eq!(vm.theme_index(), 1);
        // Out-of-range index is ignored.
        assert_eq!(vm.select_theme(9), None);
        assert_eq!(vm.theme(), ThemeChoice::Light);
    }

    #[test]
    fn select_accent_maps_through_accent_order() {
        let mut vm = SettingsVm::new();
        // The default is ruby, so the selector opens on row 0.
        assert_eq!(vm.accent(), AccentChoice::Ruby);
        assert_eq!(vm.accent_index(), 0);

        // Every row must round-trip: index -> choice -> index.
        for (index, expected) in ACCENT_ORDER.into_iter().enumerate() {
            assert_eq!(
                vm.select_accent(index),
                Some(SettingsCommand::SetAccent(expected))
            );
            assert_eq!(vm.accent(), expected);
            assert_eq!(vm.accent_index(), index);
        }
    }

    #[test]
    fn select_accent_ignores_an_out_of_range_index() {
        let mut vm = SettingsVm::new();
        assert_eq!(vm.select_accent(ACCENT_ORDER.len()), None);
        assert_eq!(vm.accent(), AccentChoice::Ruby, "state is unchanged");
    }

    #[test]
    fn update_check_toggle_and_manual_check_flow() {
        let mut vm = SettingsVm::new();
        // Off → manual check is disabled.
        assert_eq!(vm.request_update_check(), None);
        // Turn it on.
        assert_eq!(
            vm.toggle_update_check(true),
            SettingsCommand::SetUpdateCheck(true)
        );
        assert_eq!(vm.update_status(), &UpdateStatus::Idle);
        // Manual check moves to Checking and emits.
        assert_eq!(
            vm.request_update_check(),
            Some(SettingsCommand::CheckUpdates)
        );
        assert_eq!(vm.update_status(), &UpdateStatus::Checking);
        // Completing the check updates the status line.
        vm.set_update_status(UpdateStatus::Available {
            version: "v9.9.9".to_owned(),
        });
        assert_eq!(
            vm.update_status(),
            &UpdateStatus::Available {
                version: "v9.9.9".to_owned()
            }
        );
        // Turning it off resets to disabled.
        assert_eq!(
            vm.toggle_update_check(false),
            SettingsCommand::SetUpdateCheck(false)
        );
        assert_eq!(vm.update_status(), &UpdateStatus::Disabled);
    }

    #[test]
    fn set_displays_builds_sections_from_config() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 20, ConfigDimMode::Gamma);
        vm.set_displays(&[snap("A", "Left", vec![0x11, 0x0F])], &config, true);
        let section = vm.monitors().first().expect("one section");
        assert_eq!(section.name, "Left");
        assert_eq!(section.floor_pct, 20);
        assert_eq!(section.dim_mode, DimMode::Gamma);
        assert!(section.gamma_available);
        assert_eq!(
            section.inputs,
            vec![
                InputChoice {
                    code: 0x11,
                    label: "hdmi1".to_owned()
                },
                InputChoice {
                    code: 0x0F,
                    label: "dp1".to_owned()
                },
            ]
        );
        assert_eq!(section.dim_mode_index(), 1);
    }

    #[test]
    fn floor_is_clamped_to_max() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 90, ConfigDimMode::Overlay);
        vm.set_displays(&[snap("A", "Left", vec![])], &config, true);
        // The over-max config value is clamped on build.
        assert_eq!(
            vm.monitors().first().map(|s| s.floor_pct),
            Some(MAX_FLOOR_PCT)
        );
    }

    #[test]
    fn set_monitor_floor_clamps_and_emits() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 0, ConfigDimMode::Overlay);
        vm.set_displays(&[snap("A", "Left", vec![])], &config, true);
        let id = snap("A", "Left", vec![]).id;
        assert_eq!(
            vm.set_monitor_floor(0, 80),
            Some(SettingsCommand::SetMonitorFloor {
                id: id.clone(),
                pct: MAX_FLOOR_PCT
            })
        );
        assert_eq!(
            vm.monitors().first().map(|s| s.floor_pct),
            Some(MAX_FLOOR_PCT)
        );
        assert_eq!(vm.set_monitor_floor(9, 10), None);
    }

    #[test]
    fn set_monitor_min_perceived_clamps_and_emits() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 0, ConfigDimMode::Overlay);
        vm.set_displays(&[snap("A", "Left", vec![])], &config, true);
        let id = snap("A", "Left", vec![]).id;
        // The default anchor is the schema's 25.
        assert_eq!(vm.monitors().first().map(|s| s.min_perceived_pct), Some(25));
        // Above the UI band ⇒ clamped to the max (60).
        assert_eq!(
            vm.set_monitor_min_perceived(0, 90),
            Some(SettingsCommand::SetMonitorMinPerceived {
                id: id.clone(),
                pct: MIN_PERCEIVED_RANGE.1,
            })
        );
        assert_eq!(
            vm.monitors().first().map(|s| s.min_perceived_pct),
            Some(MIN_PERCEIVED_RANGE.1)
        );
        // Below the band ⇒ clamped to the min (5).
        assert_eq!(
            vm.set_monitor_min_perceived(0, 1),
            Some(SettingsCommand::SetMonitorMinPerceived {
                id,
                pct: MIN_PERCEIVED_RANGE.0,
            })
        );
        // Out-of-range index ⇒ no command.
        assert_eq!(vm.set_monitor_min_perceived(9, 30), None);
    }

    #[test]
    fn dim_mode_selection_emits_the_chosen_mode() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 0, ConfigDimMode::Overlay);
        vm.set_displays(&[snap("A", "Left", vec![])], &config, true);
        let id = snap("A", "Left", vec![]).id;
        // Index 2 = Off.
        assert_eq!(
            vm.select_monitor_dim_mode(0, 2),
            Some(SettingsCommand::SetMonitorDimMode {
                id,
                mode: DimMode::Off
            })
        );
        assert_eq!(
            vm.monitors().first().map(|s| s.dim_mode),
            Some(DimMode::Off)
        );
    }

    #[test]
    fn gamma_selection_rejected_when_unavailable() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 0, ConfigDimMode::Overlay);
        // gamma_allowed = false ⇒ HDR guard.
        vm.set_displays(&[snap("A", "Left", vec![])], &config, false);
        assert!(!vm.monitors().first().unwrap().gamma_available);
        // Index 1 = Gamma → rejected, mode unchanged.
        assert_eq!(vm.select_monitor_dim_mode(0, 1), None);
        assert_eq!(
            vm.monitors().first().map(|s| s.dim_mode),
            Some(DimMode::Overlay)
        );
    }

    #[test]
    fn input_selection_emits_raw_code() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 0, ConfigDimMode::Overlay);
        vm.set_displays(&[snap("A", "Left", vec![0x11, 0x0F])], &config, true);
        let id = snap("A", "Left", vec![]).id;
        assert_eq!(
            vm.select_monitor_input(0, 1),
            Some(SettingsCommand::SetInput { id, value: 0x0F })
        );
        // Out-of-range input index is ignored.
        assert_eq!(vm.select_monitor_input(0, 9), None);
    }

    #[test]
    fn input_selection_records_the_selected_index() {
        let mut vm = SettingsVm::new();
        let config = config_with_monitor("A", 0, ConfigDimMode::Overlay);
        vm.set_displays(&[snap("A", "Left", vec![0x11, 0x0F])], &config, true);
        // A snapshot carries no active-input readback, so the section starts with
        // no selection — the dropdown renders empty rather than a misleading 0.
        assert_eq!(
            vm.monitors().first().and_then(|s| s.selected_input_index),
            None
        );
        // Picking an input records it so the dropdown sticks on the choice; the
        // `.slint` current-index was hardcoded to 0 before, so it never held.
        let _ = vm.select_monitor_input(0, 1);
        assert_eq!(
            vm.monitors().first().and_then(|s| s.selected_input_index),
            Some(1)
        );
        // An out-of-range pick leaves the recorded selection untouched.
        let _ = vm.select_monitor_input(0, 9);
        assert_eq!(
            vm.monitors().first().and_then(|s| s.selected_input_index),
            Some(1)
        );
    }

    fn hotkey_row(action_key: &str, binding: &str) -> HotkeyRow {
        HotkeyRow {
            action_key: action_key.to_owned(),
            action_label: action_key.to_owned(),
            binding: binding.to_owned(),
            conflicted: false,
            unavailable: false,
        }
    }

    #[test]
    fn hotkeys_are_stored_and_exposed() {
        let mut vm = SettingsVm::new();
        vm.set_hotkeys(vec![
            hotkey_row("brightness_up", "Ctrl+Alt+Up"),
            HotkeyRow {
                conflicted: true,
                ..hotkey_row("toggle_flyout", "Ctrl+Alt+B")
            },
        ]);
        assert_eq!(vm.hotkeys().len(), 2);
        assert!(vm.hotkeys().get(1).is_some_and(|r| r.conflicted));
    }

    // RATIONALE: a terse test constructor mirroring `CaptureModifiers`' four
    // modifier fields; the bool-per-modifier shape is the point here.
    #[allow(clippy::fn_params_excessive_bools)]
    fn mods(ctrl: bool, alt: bool, shift: bool, meta: bool) -> CaptureModifiers {
        CaptureModifiers {
            ctrl,
            alt,
            shift,
            meta,
        }
    }

    #[test]
    fn accelerator_string_assembles_in_canonical_order() {
        assert_eq!(
            accelerator_string(mods(true, true, false, false), Some("Up")).as_deref(),
            Some("Ctrl+Alt+UP")
        );
        assert_eq!(
            accelerator_string(mods(false, false, true, true), Some("f9")).as_deref(),
            Some("Shift+Super+F9")
        );
        // A bare key with no modifiers is a structurally valid accelerator.
        assert_eq!(
            accelerator_string(mods(false, false, false, false), Some("B")).as_deref(),
            Some("B")
        );
    }

    #[test]
    fn accelerator_string_is_pending_without_a_key() {
        // Modifiers-only (still recording) and an empty token both yield None.
        assert_eq!(
            accelerator_string(mods(true, false, false, false), None),
            None
        );
        assert_eq!(
            accelerator_string(mods(true, true, false, false), Some("  ")),
            None
        );
    }

    #[test]
    fn capture_hotkey_emits_sethotkey_for_the_row_action() {
        let mut vm = SettingsVm::new();
        vm.set_hotkeys(vec![hotkey_row("brightness_up", "")]);
        assert_eq!(
            vm.capture_hotkey(0, mods(true, true, false, false), Some("Up")),
            Some(SettingsCommand::SetHotkey {
                action_key: "brightness_up".to_owned(),
                binding: "Ctrl+Alt+UP".to_owned(),
            })
        );
        // Modifiers-only is pending (no command); out-of-range is ignored.
        assert_eq!(
            vm.capture_hotkey(0, mods(true, false, false, false), None),
            None
        );
        assert_eq!(
            vm.capture_hotkey(9, mods(true, true, false, false), Some("Up")),
            None
        );
    }

    #[test]
    fn clear_hotkey_emits_clear_for_the_row_action() {
        let mut vm = SettingsVm::new();
        vm.set_hotkeys(vec![hotkey_row("toggle_flyout", "Ctrl+Alt+B")]);
        assert_eq!(
            vm.clear_hotkey(0),
            Some(SettingsCommand::ClearHotkey {
                action_key: "toggle_flyout".to_owned(),
            })
        );
        assert_eq!(vm.clear_hotkey(9), None);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(SettingsVm::default(), SettingsVm::new());
    }
}

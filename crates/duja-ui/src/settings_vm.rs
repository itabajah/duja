//! The settings view-model skeleton (P4 scope).
//!
//! [`SettingsVm`] is a **structural placeholder**: it lays out the rows wave 2
//! will bind — an autostart toggle and a dim-mode selector — with honest,
//! inert state. It deliberately emits no commands and wires no real behaviour,
//! so nothing here pretends to work before the settings backend lands (P5).
//! It exists now only so the `.slint` settings surface has a stable shape to
//! render against and the architecture boundary is established.

/// Which setting a [`SettingsRow`] represents, so wave 2 can bind by key rather
/// than by list position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKey {
    /// Launch Duja at login.
    Autostart,
    /// Sub-floor dimming strategy (`overlay` / `gamma` / `off`).
    DimMode,
}

/// The control a settings row renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingControl {
    /// An on/off switch with its current position.
    Toggle {
        /// Whether the toggle is currently on.
        on: bool,
    },
    /// A single-choice selector: the option labels and the selected index.
    Selector {
        /// The choices, in display order.
        options: Vec<String>,
        /// Index into `options` of the current choice.
        selected: usize,
    },
}

/// One settings row: a stable key, a label, and its control state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsRow {
    /// Which setting this row drives.
    pub key: SettingKey,
    /// The user-visible label (English key; translated in the `.slint` layer).
    pub label: String,
    /// The control and its current state.
    pub control: SettingControl,
}

/// The settings view-model: a fixed list of placeholder rows for P4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsVm {
    rows: Vec<SettingsRow>,
}

impl Default for SettingsVm {
    fn default() -> Self {
        SettingsVm::new()
    }
}

impl SettingsVm {
    /// Build the P4 placeholder settings list: an autostart toggle (off) and a
    /// dim-mode selector defaulting to overlay.
    #[must_use]
    pub fn new() -> Self {
        SettingsVm {
            rows: vec![
                SettingsRow {
                    key: SettingKey::Autostart,
                    label: "Start with Windows".to_owned(),
                    control: SettingControl::Toggle { on: false },
                },
                SettingsRow {
                    key: SettingKey::DimMode,
                    label: "Dim mode".to_owned(),
                    control: SettingControl::Selector {
                        options: vec!["Overlay".to_owned(), "Gamma".to_owned(), "Off".to_owned()],
                        selected: 0,
                    },
                },
            ],
        }
    }

    /// The settings rows, in display order.
    #[must_use]
    pub fn rows(&self) -> &[SettingsRow] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_has_autostart_and_dim_mode_rows() {
        let vm = SettingsVm::new();
        let keys: Vec<SettingKey> = vm.rows().iter().map(|r| r.key).collect();
        assert_eq!(keys, vec![SettingKey::Autostart, SettingKey::DimMode]);
    }

    #[test]
    fn autostart_defaults_off() {
        let vm = SettingsVm::new();
        let row = vm.rows().first().unwrap();
        assert_eq!(row.control, SettingControl::Toggle { on: false });
    }

    #[test]
    fn dim_mode_selector_lists_overlay_gamma_off() {
        let vm = SettingsVm::new();
        let row = vm.rows().get(1).unwrap();
        match &row.control {
            SettingControl::Selector { options, selected } => {
                assert_eq!(options, &["Overlay", "Gamma", "Off"]);
                assert_eq!(*selected, 0);
            }
            SettingControl::Toggle { .. } => panic!("dim-mode should be a selector"),
        }
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(SettingsVm::default(), SettingsVm::new());
    }
}

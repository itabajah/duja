//! The flyout view-model: snapshots in, presentation rows + commands out.
//!
//! [`FlyoutVm`] is pure Rust with **zero Slint types in its signatures** (the
//! architecture rule, plan §4.4): it is fed [`DisplaySnapshot`]s from engine
//! notifications, exposes an ordered list of [`FlyoutRow`]s the `.slint` layer
//! renders verbatim, and turns user actions into [`UiCommand`]s. All UI logic
//! lives here so it is exhaustively unit-testable; the shell is a thin mapping
//! skin.
//!
//! # Ordering
//!
//! Rows are sorted by [`StableDisplayId`] on every
//! [`set_displays`](FlyoutVm::set_displays). Because the id is durable across
//! replug, this order is **stable across refreshes**: the same set of displays
//! always yields the same row order, so a monitor never jumps position under
//! the user's cursor when an unrelated enumeration arrives.
//!
//! # Unresponsive displays
//!
//! A display the watchdog marks unresponsive is *greyed*: its row stays in the
//! list at its last known level (the engine keeps recording levels for
//! hot-plug restore), but its slider is disabled and dragging it emits nothing.
//! The unresponsive set is tracked independently of the snapshot list, so a
//! `set_displays` refresh preserves the greyed state.

use std::collections::BTreeSet;

use duja_core::id::StableDisplayId;
use duja_core::model::{DisplayKind, DisplaySnapshot};

use crate::command::UiCommand;

/// Which palette the flyout renders in.
///
/// Pure presentation state: switching the theme mutates only this field and is
/// never a [`UiCommand`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Light palette.
    Light,
    /// Dark palette (the flyout's default; the ADR-0009 Fluent dark look).
    #[default]
    Dark,
}

/// One monitor's row, as the `.slint` list renders it.
///
/// Every field is presentation-ready: the shell copies them straight into the
/// Slint model without further logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlyoutRow {
    /// Durable identity, used to address [`UiCommand::SetLevel`] and to sort.
    pub id: StableDisplayId,
    /// Human-readable monitor name (from EDID; not translatable).
    pub display_name: String,
    /// Current unified user level, `0..=100`.
    pub level_pct: u8,
    /// Short label for the control class (e.g. `External`, `Built-in`).
    pub kind_label: String,
    /// Whether the row is dimmed because the display is unresponsive.
    pub greyed: bool,
    /// Whether the slider accepts input (the inverse of [`greyed`](Self::greyed)
    /// for P4; kept as its own field so future capability gating can diverge).
    pub slider_enabled: bool,
}

/// The flyout's view-model: ordered rows, a link-all toggle, and a theme.
#[derive(Debug, Clone)]
pub struct FlyoutVm {
    rows: Vec<FlyoutRow>,
    unresponsive: BTreeSet<StableDisplayId>,
    link_all: bool,
    theme: Theme,
}

impl Default for FlyoutVm {
    fn default() -> Self {
        FlyoutVm::new()
    }
}

impl FlyoutVm {
    /// Create an empty flyout view-model in the default (dark) theme.
    #[must_use]
    pub fn new() -> Self {
        FlyoutVm {
            rows: Vec::new(),
            unresponsive: BTreeSet::new(),
            link_all: false,
            theme: Theme::default(),
        }
    }

    /// Replace the display set from a fresh batch of engine snapshots.
    ///
    /// Rows are rebuilt sorted by [`StableDisplayId`] (stable across calls). The
    /// greyed/unresponsive state is preserved: a display still in the
    /// unresponsive set stays greyed and slider-disabled at its snapshot level.
    pub fn set_displays(&mut self, snapshots: Vec<DisplaySnapshot>) {
        let mut snapshots = snapshots;
        snapshots.sort_by(|a, b| a.id.cmp(&b.id));
        self.rows = snapshots
            .into_iter()
            .map(|snap| {
                let greyed = self.unresponsive.contains(&snap.id);
                FlyoutRow {
                    id: snap.id,
                    display_name: snap.name,
                    level_pct: snap.user_level_pct.min(100),
                    kind_label: kind_label(snap.kind).to_owned(),
                    greyed,
                    slider_enabled: !greyed,
                }
            })
            .collect();
    }

    /// Mark a display responsive or unresponsive, updating its row in place.
    ///
    /// Tracked independently of [`set_displays`](Self::set_displays) so the
    /// greyed state survives snapshot refreshes. A no-op if no row matches.
    pub fn set_unresponsive(&mut self, id: &StableDisplayId, unresponsive: bool) {
        if unresponsive {
            self.unresponsive.insert(id.clone());
        } else {
            self.unresponsive.remove(id);
        }
        for row in &mut self.rows {
            if &row.id == id {
                row.greyed = unresponsive;
                row.slider_enabled = !unresponsive;
            }
        }
    }

    /// Handle a slider drag on the row at `row_index` to `pct` (clamped to
    /// `0..=100`), returning the resulting commands.
    ///
    /// A greyed row (or an out-of-range index) changes nothing and emits an
    /// empty vector. When link-all is on, the same absolute percent is applied
    /// to *every* non-greyed row and one [`UiCommand::SetLevel`] is emitted per
    /// such row (P4 uses absolute, not relative, linking). Otherwise only the
    /// touched row is updated and emitted.
    pub fn slider_changed(&mut self, row_index: usize, pct: u8) -> Vec<UiCommand> {
        let pct = pct.min(100);
        let touchable = matches!(self.rows.get(row_index), Some(row) if !row.greyed);
        if !touchable {
            return Vec::new();
        }

        let mut commands = Vec::new();
        if self.link_all {
            for row in &mut self.rows {
                if row.greyed {
                    continue;
                }
                row.level_pct = pct;
                commands.push(UiCommand::SetLevel {
                    id: row.id.clone(),
                    pct,
                });
            }
        } else if let Some(row) = self.rows.get_mut(row_index) {
            row.level_pct = pct;
            commands.push(UiCommand::SetLevel {
                id: row.id.clone(),
                pct,
            });
        }
        commands
    }

    /// Set the link-all toggle. Pure presentation state; emits no command.
    pub fn link_toggled(&mut self, on: bool) {
        self.link_all = on;
    }

    /// The command for the refresh affordance (re-enumerate now).
    #[must_use]
    pub fn refresh_requested(&self) -> UiCommand {
        UiCommand::Refresh
    }

    /// Switch the theme. Pure presentation state; emits no command.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// The current row list, in stable id order.
    #[must_use]
    pub fn rows(&self) -> &[FlyoutRow] {
        &self.rows
    }

    /// Whether the link-all toggle is on.
    #[must_use]
    pub fn link_all(&self) -> bool {
        self.link_all
    }

    /// The current theme.
    #[must_use]
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Whether there are no displays to show (drives the empty-state label).
    #[must_use]
    pub fn no_displays(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Map a [`DisplayKind`] to its short row label.
///
/// The label is an English key here; the *static* chrome of the flyout is
/// translated in the `.slint` layer via `@tr`. A fully-localised kind label
/// would have the view-model surface the [`DisplayKind`] and the presentation
/// layer translate it — deferred, since P4 ships English-only strings.
fn kind_label(kind: DisplayKind) -> &'static str {
    match kind {
        DisplayKind::ExternalDdc => "External",
        DisplayKind::InternalPanel => "Built-in",
        DisplayKind::SoftwareOnly => "Software",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::model::Capabilities;

    /// Build a snapshot with a synthetic serial-string id so ordering is
    /// predictable (ids sort by their `MFG-PROD-SERIAL` string).
    fn snap(serial: &str, name: &str, level: u8, kind: DisplayKind) -> DisplaySnapshot {
        let id = StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap();
        DisplaySnapshot {
            id,
            name: name.to_owned(),
            kind,
            user_level_pct: level,
            capabilities: Capabilities::default(),
        }
    }

    fn id_of(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
    }

    #[test]
    fn empty_vm_reports_no_displays() {
        let vm = FlyoutVm::new();
        assert!(vm.no_displays());
        assert!(vm.rows().is_empty());
        assert!(!vm.link_all());
        assert_eq!(vm.theme(), Theme::Dark);
    }

    #[test]
    fn set_displays_sorts_rows_by_id() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("C", "Right", 30, DisplayKind::ExternalDdc),
            snap("A", "Left", 40, DisplayKind::InternalPanel),
            snap("B", "Middle", 50, DisplayKind::SoftwareOnly),
        ]);
        let names: Vec<&str> = vm.rows().iter().map(|r| r.display_name.as_str()).collect();
        assert_eq!(names, vec!["Left", "Middle", "Right"]);
        assert!(!vm.no_displays());
    }

    #[test]
    fn row_order_is_stable_across_refresh() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Left", 40, DisplayKind::ExternalDdc),
            snap("B", "Right", 50, DisplayKind::ExternalDdc),
        ]);
        // Same displays arriving in a different input order.
        vm.set_displays(vec![
            snap("B", "Right", 55, DisplayKind::ExternalDdc),
            snap("A", "Left", 45, DisplayKind::ExternalDdc),
        ]);
        let ids: Vec<&str> = vm.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec![id_of("A").as_str(), id_of("B").as_str()]);
        // Levels updated in place, order unchanged.
        assert_eq!(vm.rows().first().map(|r| r.level_pct), Some(45));
    }

    #[test]
    fn kind_labels_map_each_variant() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Ext", 10, DisplayKind::ExternalDdc),
            snap("B", "Int", 10, DisplayKind::InternalPanel),
            snap("C", "Sw", 10, DisplayKind::SoftwareOnly),
        ]);
        let labels: Vec<&str> = vm.rows().iter().map(|r| r.kind_label.as_str()).collect();
        assert_eq!(labels, vec!["External", "Built-in", "Software"]);
    }

    #[test]
    fn slider_change_emits_single_setlevel_when_unlinked() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Left", 40, DisplayKind::ExternalDdc),
            snap("B", "Right", 50, DisplayKind::ExternalDdc),
        ]);
        let cmds = vm.slider_changed(0, 75);
        assert_eq!(
            cmds,
            vec![UiCommand::SetLevel {
                id: id_of("A"),
                pct: 75,
            }]
        );
        assert_eq!(vm.rows().first().map(|r| r.level_pct), Some(75));
        // The untouched row is unchanged.
        assert_eq!(vm.rows().get(1).map(|r| r.level_pct), Some(50));
    }

    #[test]
    fn slider_change_clamps_percent() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Left", 40, DisplayKind::ExternalDdc)]);
        let cmds = vm.slider_changed(0, 250);
        assert_eq!(
            cmds,
            vec![UiCommand::SetLevel {
                id: id_of("A"),
                pct: 100,
            }]
        );
        assert_eq!(vm.rows().first().map(|r| r.level_pct), Some(100));
    }

    #[test]
    fn link_all_fans_out_to_every_non_greyed_row() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Left", 40, DisplayKind::ExternalDdc),
            snap("B", "Right", 50, DisplayKind::ExternalDdc),
            snap("C", "Third", 60, DisplayKind::ExternalDdc),
        ]);
        vm.link_toggled(true);
        let cmds = vm.slider_changed(1, 33);
        assert_eq!(
            cmds,
            vec![
                UiCommand::SetLevel {
                    id: id_of("A"),
                    pct: 33,
                },
                UiCommand::SetLevel {
                    id: id_of("B"),
                    pct: 33,
                },
                UiCommand::SetLevel {
                    id: id_of("C"),
                    pct: 33,
                },
            ]
        );
        // Every row now shows the same absolute level.
        assert!(vm.rows().iter().all(|r| r.level_pct == 33));
    }

    #[test]
    fn link_all_skips_greyed_rows_in_fan_out() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Left", 40, DisplayKind::ExternalDdc),
            snap("B", "Right", 50, DisplayKind::ExternalDdc),
        ]);
        vm.set_unresponsive(&id_of("B"), true);
        vm.link_toggled(true);
        let cmds = vm.slider_changed(0, 20);
        assert_eq!(
            cmds,
            vec![UiCommand::SetLevel {
                id: id_of("A"),
                pct: 20,
            }]
        );
        // The greyed row keeps its old level and emits nothing.
        assert_eq!(vm.rows().get(1).map(|r| r.level_pct), Some(50));
    }

    #[test]
    fn greyed_row_emits_nothing_and_keeps_value() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Left", 40, DisplayKind::ExternalDdc)]);
        vm.set_unresponsive(&id_of("A"), true);
        assert!(
            vm.rows()
                .first()
                .is_some_and(|r| r.greyed && !r.slider_enabled)
        );
        let cmds = vm.slider_changed(0, 90);
        assert!(cmds.is_empty());
        assert_eq!(vm.rows().first().map(|r| r.level_pct), Some(40));
    }

    #[test]
    fn out_of_range_index_is_ignored() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Left", 40, DisplayKind::ExternalDdc)]);
        assert!(vm.slider_changed(9, 50).is_empty());
    }

    #[test]
    fn unresponsive_state_survives_refresh() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Left", 40, DisplayKind::ExternalDdc)]);
        vm.set_unresponsive(&id_of("A"), true);
        // A fresh snapshot batch must not clear the greyed flag.
        vm.set_displays(vec![snap("A", "Left", 42, DisplayKind::ExternalDdc)]);
        let row = vm.rows().first().unwrap();
        assert!(row.greyed);
        assert!(!row.slider_enabled);
        assert_eq!(row.level_pct, 42);
    }

    #[test]
    fn clearing_unresponsive_re_enables_slider() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Left", 40, DisplayKind::ExternalDdc)]);
        vm.set_unresponsive(&id_of("A"), true);
        vm.set_unresponsive(&id_of("A"), false);
        let row = vm.rows().first().unwrap();
        assert!(!row.greyed);
        assert!(row.slider_enabled);
        assert!(!vm.slider_changed(0, 77).is_empty());
    }

    #[test]
    fn link_toggle_is_pure_state() {
        let mut vm = FlyoutVm::new();
        assert!(!vm.link_all());
        vm.link_toggled(true);
        assert!(vm.link_all());
        vm.link_toggled(false);
        assert!(!vm.link_all());
    }

    #[test]
    fn theme_switch_is_pure_state() {
        let mut vm = FlyoutVm::new();
        assert_eq!(vm.theme(), Theme::Dark);
        vm.set_theme(Theme::Light);
        assert_eq!(vm.theme(), Theme::Light);
        vm.set_theme(Theme::Dark);
        assert_eq!(vm.theme(), Theme::Dark);
    }

    #[test]
    fn refresh_requested_emits_refresh() {
        let vm = FlyoutVm::new();
        assert_eq!(vm.refresh_requested(), UiCommand::Refresh);
    }
}

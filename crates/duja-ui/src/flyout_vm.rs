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

use std::collections::{BTreeMap, BTreeSet};

use duja_core::continuum::{self, ContinuumConfig, SliderGeometry};
use duja_core::id::StableDisplayId;
use duja_core::model::{DimMode, DisplayKind, DisplaySnapshot};

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

/// A display's dimming configuration, as the app resolves it from the continuum
/// config and feeds into the flyout for the toggle + floor marker.
///
/// Kept minimal (no Slint, no continuum machinery) so the marker mapping is
/// unit-testable in isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DimmingInfo {
    /// The hardware brightness floor percentage, or `None` for a software-only
    /// display (no hardware backlight, so no handoff marker).
    pub hardware_floor: Option<u8>,
    /// The perceptual-scale anchor (`min_perceived_pct`): the perceived brightness
    /// the panel shows at hardware zero. Sets where the handoff marker sits.
    pub min_perceived_pct: u8,
    /// Whether software dimming is currently engaged (the configured dim mode is
    /// not `Off`).
    pub dimming_on: bool,
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
    /// Whether software dimming is engaged for this display (drives the toggle).
    pub dimming_on: bool,
    /// The hardware brightness floor percentage, or `None` for a software-only
    /// display. Feeds the slider's handoff marker via [`slider_geometry`](Self::slider_geometry).
    pub hardware_floor_pct: Option<u8>,
    /// The perceptual-scale anchor for this display (see [`DimmingInfo`]).
    pub min_perceived_pct: u8,
}

impl FlyoutRow {
    /// Whether this display has a hardware floor (and therefore a handoff marker
    /// on its slider). `false` for software-only displays.
    #[must_use]
    pub fn has_hardware_floor(&self) -> bool {
        self.hardware_floor_pct.is_some()
    }

    /// The continuum config that determines this row's slider geometry. The dim
    /// mode is `Overlay` when dimming is on (the sub-floor zone is reachable) and
    /// `Off` when it is not (the slider bottoms out at the transition).
    fn continuum_cfg(&self) -> ContinuumConfig {
        let mode = if self.dimming_on {
            DimMode::Overlay
        } else {
            DimMode::Off
        };
        match self.hardware_floor_pct {
            Some(floor) => ContinuumConfig::hardware(floor, self.min_perceived_pct, mode),
            None => ContinuumConfig::software_only(mode),
        }
    }

    /// The slider marker geometry (line A/B fractions + minimum usable fraction)
    /// the `.slint` layer renders. Delegates to [`continuum::geometry`].
    #[must_use]
    pub fn slider_geometry(&self) -> SliderGeometry {
        continuum::geometry(&self.continuum_cfg())
    }

    /// The handoff fraction on the `0.0..=1.0` track: where hardware hands off to
    /// software dimming (line B). `0.0` for a software-only display (no marker).
    #[must_use]
    pub fn transition_fraction(&self) -> f32 {
        self.slider_geometry().transition.unwrap_or(0.0)
    }

    /// The hardware-zero fraction on the `0.0..=1.0` track (line A, the perceptual
    /// anchor). `0.0` for a software-only display (no marker).
    #[must_use]
    pub fn hw_zero_fraction(&self) -> f32 {
        self.slider_geometry().hw_zero.unwrap_or(0.0)
    }

    /// Whether lines A and B coincide (a floor of 0), so the UI draws just one.
    /// `true` for a software-only display (no distinct markers either way).
    #[must_use]
    pub fn markers_coincide(&self) -> bool {
        let geometry = self.slider_geometry();
        match (geometry.hw_zero, geometry.transition) {
            (Some(a), Some(b)) => (a - b).abs() < 0.005,
            _ => true,
        }
    }
}

/// The flyout's view-model: ordered rows, a link-all toggle, and a theme.
#[derive(Debug, Clone)]
pub struct FlyoutVm {
    rows: Vec<FlyoutRow>,
    unresponsive: BTreeSet<StableDisplayId>,
    /// Per-display dimming config (floor + on/off), tracked independently of the
    /// snapshot list so it survives a `set_displays` refresh — the same pattern
    /// as [`unresponsive`](Self::unresponsive).
    dimming: BTreeMap<StableDisplayId, DimmingInfo>,
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
            dimming: BTreeMap::new(),
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
                let dim = self.dimming.get(&snap.id).copied().unwrap_or_default();
                FlyoutRow {
                    id: snap.id,
                    display_name: snap.name,
                    level_pct: snap.user_level_pct.min(100),
                    kind_label: kind_label(snap.kind).to_owned(),
                    greyed,
                    slider_enabled: !greyed,
                    dimming_on: dim.dimming_on,
                    hardware_floor_pct: dim.hardware_floor,
                    min_perceived_pct: dim.min_perceived_pct,
                }
            })
            .collect();
    }

    /// Replace the per-display dimming config (floor + on/off), patching each
    /// matching row in place.
    ///
    /// Tracked independently of [`set_displays`](Self::set_displays) so the
    /// marker + toggle survive snapshot refreshes. The app rebuilds this map from
    /// the resolved continuum config whenever the config or display set changes.
    pub fn set_dimming_info(&mut self, dimming: BTreeMap<StableDisplayId, DimmingInfo>) {
        self.dimming = dimming;
        for row in &mut self.rows {
            let dim = self.dimming.get(&row.id).copied().unwrap_or_default();
            row.dimming_on = dim.dimming_on;
            row.hardware_floor_pct = dim.hardware_floor;
            row.min_perceived_pct = dim.min_perceived_pct;
        }
    }

    /// Toggle software dimming for the row at `row_index`, returning the command
    /// to persist and re-plan.
    ///
    /// A greyed row (or an out-of-range index) changes nothing and returns
    /// `None`. Otherwise the row's toggle state is updated optimistically and a
    /// [`UiCommand::SetDimmingEnabled`] is emitted for the app to apply.
    pub fn toggle_dimming(&mut self, row_index: usize, on: bool) -> Option<UiCommand> {
        let row = self.rows.get_mut(row_index)?;
        if row.greyed {
            return None;
        }
        row.dimming_on = on;
        if let Some(info) = self.dimming.get_mut(&row.id) {
            info.dimming_on = on;
        }
        Some(UiCommand::SetDimmingEnabled {
            id: row.id.clone(),
            on,
        })
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

    /// Update a display's shown level in place from an **external** change
    /// (a reflection poll), without emitting a command.
    ///
    /// Used when the monitor's own buttons (or another app) move the brightness:
    /// the slider follows so it keeps mirroring reality. Drag-safe — the `.slint`
    /// slider ignores model writes to `value` while the user is dragging, so a
    /// reflection that lands mid-drag never fights the user's thumb. A no-op if no
    /// row matches.
    pub fn set_level(&mut self, id: &StableDisplayId, pct: u8) {
        let pct = pct.min(100);
        for row in &mut self.rows {
            if &row.id == id {
                row.level_pct = pct;
            }
        }
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

    // --- dimming toggle + floor marker (feature 1) ---

    fn dim(floor: Option<u8>, min_perceived: u8, on: bool) -> DimmingInfo {
        DimmingInfo {
            hardware_floor: floor,
            min_perceived_pct: min_perceived,
            dimming_on: on,
        }
    }

    #[test]
    fn transition_fraction_is_the_pos_of_the_floor() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Ext", 60, DisplayKind::ExternalDdc)]);
        let mut info = BTreeMap::new();
        // floor 20, anchor 25 ⇒ line B = pos(20) = 25 + 75·0.2 = 40 ⇒ 0.40.
        info.insert(id_of("A"), dim(Some(20), 25, true));
        vm.set_dimming_info(info);
        let row = vm.rows().first().unwrap();
        assert!((row.transition_fraction() - 0.40).abs() < 1e-6);
        // Line A (hardware zero) sits at the anchor, 0.25.
        assert!((row.slider_geometry().hw_zero.unwrap() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn markers_coincide_at_floor_zero() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Ext", 60, DisplayKind::ExternalDdc)]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(0), 25, true));
        vm.set_dimming_info(info);
        let g = vm.rows().first().unwrap().slider_geometry();
        // floor 0 ⇒ lines A and B coincide at the anchor (0.25).
        assert_eq!(g.hw_zero, g.transition);
        assert!((g.transition.unwrap() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn transition_moves_with_floor_but_line_a_does_not() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Low", 60, DisplayKind::ExternalDdc),
            snap("B", "High", 60, DisplayKind::ExternalDdc),
        ]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(0), 25, true)); // B = 0.25
        info.insert(id_of("B"), dim(Some(40), 25, true)); // B = pos(40) = 0.55
        vm.set_dimming_info(info);
        let low = vm.rows().iter().find(|r| r.id == id_of("A")).unwrap();
        let high = vm.rows().iter().find(|r| r.id == id_of("B")).unwrap();
        assert!((low.transition_fraction() - 0.25).abs() < 1e-6);
        assert!((high.transition_fraction() - 0.55).abs() < 1e-6);
        // Line A (hardware zero) is floor-independent.
        assert_eq!(
            low.slider_geometry().hw_zero,
            high.slider_geometry().hw_zero
        );
    }

    #[test]
    fn min_usable_is_the_transition_only_when_dimming_off() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Ext", 60, DisplayKind::ExternalDdc)]);
        // Dimming OFF ⇒ the slider bottoms out at the transition B = 0.40.
        let mut off = BTreeMap::new();
        off.insert(id_of("A"), dim(Some(20), 25, false));
        vm.set_dimming_info(off);
        assert!((vm.rows().first().unwrap().slider_geometry().min_usable - 0.40).abs() < 1e-6);
        // Dimming ON ⇒ it can reach full dark (0).
        let mut on = BTreeMap::new();
        on.insert(id_of("A"), dim(Some(20), 25, true));
        vm.set_dimming_info(on);
        assert!((vm.rows().first().unwrap().slider_geometry().min_usable - 0.0).abs() < 1e-6);
    }

    #[test]
    fn row_marker_methods_expose_both_lines_when_distinct() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Ext", 60, DisplayKind::ExternalDdc)]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(20), 25, true)); // floor 20, anchor 25
        vm.set_dimming_info(info);
        let row = vm.rows().first().unwrap();
        // Line A = anchor 0.25; line B = pos(20) = 0.40; distinct ⇒ draw both.
        assert!((row.hw_zero_fraction() - 0.25).abs() < 1e-6);
        assert!((row.transition_fraction() - 0.40).abs() < 1e-6);
        assert!(!row.markers_coincide());
    }

    #[test]
    fn row_markers_coincide_at_floor_zero_and_software_only() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Ext", 60, DisplayKind::ExternalDdc),
            snap("B", "Sw", 60, DisplayKind::SoftwareOnly),
        ]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(0), 25, true)); // floor 0 ⇒ A == B
        info.insert(id_of("B"), dim(None, 25, true)); // software-only ⇒ no markers
        vm.set_dimming_info(info);
        assert!(vm.rows().iter().all(FlyoutRow::markers_coincide));
    }

    #[test]
    fn rows_expose_marker_and_toggle_from_dimming_info() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Ext", 60, DisplayKind::ExternalDdc),
            snap("B", "Sw", 60, DisplayKind::SoftwareOnly),
        ]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(20), 25, true));
        info.insert(id_of("B"), dim(None, 25, false));
        vm.set_dimming_info(info);

        let ext = vm.rows().first().expect("row A");
        assert!(ext.has_hardware_floor());
        // B = pos(20) with anchor 25 = 0.40.
        assert!((ext.transition_fraction() - 0.40).abs() < 1e-6);
        assert!(ext.dimming_on);

        let sw = vm.rows().get(1).expect("row B");
        assert!(!sw.has_hardware_floor());
        assert!(!sw.dimming_on);
        // Software-only: no handoff marker.
        assert_eq!(sw.slider_geometry().transition, None);
    }

    #[test]
    fn dimming_info_survives_a_snapshot_refresh() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Ext", 60, DisplayKind::ExternalDdc)]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(0), 25, true));
        vm.set_dimming_info(info);
        // A fresh snapshot batch must not clear the marker/toggle state.
        vm.set_displays(vec![snap("A", "Ext", 55, DisplayKind::ExternalDdc)]);
        let row = vm.rows().first().unwrap();
        // floor 0, anchor 25 ⇒ transition at 0.25.
        assert!((row.transition_fraction() - 0.25).abs() < 1e-6);
        assert!(row.dimming_on);
        assert_eq!(row.level_pct, 55);
    }

    #[test]
    fn toggle_dimming_emits_command_and_updates_row() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Ext", 60, DisplayKind::ExternalDdc)]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(20), 25, true));
        vm.set_dimming_info(info);

        assert_eq!(
            vm.toggle_dimming(0, false),
            Some(UiCommand::SetDimmingEnabled {
                id: id_of("A"),
                on: false,
            })
        );
        assert!(!vm.rows().first().unwrap().dimming_on);
        // Toggling back on round-trips.
        assert_eq!(
            vm.toggle_dimming(0, true),
            Some(UiCommand::SetDimmingEnabled {
                id: id_of("A"),
                on: true,
            })
        );
        assert!(vm.rows().first().unwrap().dimming_on);
    }

    #[test]
    fn toggle_dimming_ignores_greyed_and_out_of_range() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap("A", "Ext", 60, DisplayKind::ExternalDdc)]);
        vm.set_unresponsive(&id_of("A"), true);
        assert_eq!(vm.toggle_dimming(0, false), None);
        assert_eq!(vm.toggle_dimming(9, true), None);
    }
}

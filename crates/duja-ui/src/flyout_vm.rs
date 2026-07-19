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
//! Rows are **order-stable by identity**: a display keeps the row position it was
//! first seen at for as long as it stays connected.
//! [`set_displays`](FlyoutVm::set_displays) matches each incoming snapshot to an
//! existing row by [`StableDisplayId`] and never reindexes a survivor;
//! genuinely-new displays are **appended after** the survivors (in id order, so a
//! *fresh* population is deterministic) and gone displays are dropped.
//!
//! Precisely: an **addition** never moves a survivor (a new display goes to the
//! end, never above an existing row); a **removal** compacts the rows below the
//! gone display, preserving their relative order. The flyout addresses each
//! slider's `changed` by its *positional* row index, so this ordering keeps a row
//! a user is **mid-drag** on bound to the same display when a hot-plug arrives —
//! a re-sort under a held thumb would retarget the drag to the wrong display. The
//! one residual case is a concurrent **unplug of a *different* display** during a
//! drag, which compacts and can shift the dragged row; it is rare (two hands, two
//! monitors) and the external-change reflection path re-settles the level, so it
//! is left as a known edge rather than moving slider addressing to id-keyed.
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
use duja_core::sync::SyncGroups;

use crate::accent::AccentChoice;
use crate::command::UiCommand;

/// The single group name the flyout's "Link all" toggle uses in its
/// [`SyncGroups`]. Every linked (non-greyed) row is a member of this one group.
const LINK_GROUP: &str = "link";

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
    /// Whether the display has **no working hardware brightness** (software-only).
    /// When set, the flyout forces its "Software dimming" toggle on and
    /// non-interactive (the overlay is the display's only dimming channel).
    /// Independent of the physical kind (#67).
    pub software_only: bool,
}

/// One monitor's row, as the `.slint` list renders it.
///
/// Every field is presentation-ready: the shell copies them straight into the
/// Slint model without further logic.
// RATIONALE: the four bools are independent, orthogonal presentation flags
// (greyed, slider-enabled, dimming-on, software-only) the `.slint` layer reads
// verbatim into `FlyoutRowData`; folding them into a state enum would obscure that
// 1:1 mapping and buy nothing.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlyoutRow {
    /// Durable identity, used to address [`UiCommand::SetLevel`] and to sort.
    pub id: StableDisplayId,
    /// Human-readable monitor name (from EDID; not translatable).
    pub display_name: String,
    /// Current unified user level, `0..=100`.
    pub level_pct: u8,
    /// Short label for the physical provenance (e.g. `External`, `Internal`).
    pub kind_label: String,
    /// Whether the row is dimmed because the display is unresponsive.
    pub greyed: bool,
    /// Whether the slider accepts input (the inverse of [`greyed`](Self::greyed)
    /// for P4; kept as its own field so future capability gating can diverge).
    pub slider_enabled: bool,
    /// Whether software dimming is engaged for this display (drives the toggle).
    pub dimming_on: bool,
    /// Whether the display is software-only (no working hardware brightness). Drives
    /// the flyout's forced-on, non-interactive "Software dimming" toggle and makes
    /// [`toggle_dimming`](FlyoutVm::toggle_dimming) a no-op for the row. It never
    /// changes [`kind_label`](Self::kind_label) — provenance stays Internal/External.
    pub software_only: bool,
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
    /// Session-only offset engine backing "Link all". While linked, every
    /// non-greyed row is a member of the one [`LINK_GROUP`] with its offset from
    /// the anchor; a drag fans a master value out to the members, preserving the
    /// per-monitor gaps. Rebuilt on [`link_toggled`](Self::link_toggled) and never
    /// persisted — the config `sync_group`/`sync_offset` schema is untouched.
    link_group: SyncGroups,
    theme: Theme,
    /// The accent the palette is painted in. The shell resolves it against
    /// [`theme`](Self::theme) on every render, so a *theme* change automatically
    /// re-pushes the right accent variants with no extra wiring.
    accent: AccentChoice,
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
            link_group: SyncGroups::new(),
            theme: Theme::default(),
            accent: AccentChoice::default(),
        }
    }

    /// Merge a fresh batch of engine snapshots into the rows, **order-stably**.
    ///
    /// Each surviving display keeps its current row position (its data refreshed
    /// in place); genuinely-new displays are appended after the survivors in id
    /// order, and displays no longer present are dropped. Never reindexing a
    /// survivor is what keeps a row a user is mid-drag on bound to the same
    /// display across a hot-plug (see the module `# Ordering` note) — the flyout
    /// binds each slider's `changed` to its positional row index.
    ///
    /// The greyed/unresponsive and dimming state is preserved: a display still in
    /// the unresponsive set stays greyed and slider-disabled at its snapshot
    /// level.
    pub fn set_displays(&mut self, snapshots: Vec<DisplaySnapshot>) {
        // Index the incoming batch by id: a `BTreeMap` both de-duplicates and
        // yields a deterministic (id-sorted) order for the genuinely-new displays
        // appended below.
        let mut incoming: BTreeMap<StableDisplayId, DisplaySnapshot> = snapshots
            .into_iter()
            .map(|snap| (snap.id.clone(), snap))
            .collect();
        let mut rows = Vec::with_capacity(incoming.len());
        // 1. Keep every surviving row AT ITS CURRENT POSITION, refreshed in place.
        for existing in &self.rows {
            if let Some(snap) = incoming.remove(&existing.id) {
                rows.push(self.build_row(snap));
            }
        }
        // 2. Append the genuinely-new displays after the survivors, in id order.
        for (_, snap) in incoming {
            rows.push(self.build_row(snap));
        }
        self.rows = rows;
        // While linked, enrol any genuinely-new (hot-plugged) non-greyed row into
        // the link group at offset 0 so it moves with the group instead of sitting
        // frozen. Displays present at link time are already members and keep their
        // recorded offset; offset 0 for a newcomer is a deliberate minimal choice
        // (it tracks the anchor rather than trying to reconstruct a gap).
        if self.link_all {
            for row in &self.rows {
                if !row.greyed && self.link_group.offset_of(&row.id).is_none() {
                    self.link_group.add(LINK_GROUP, row.id.clone(), 0);
                }
            }
        }
    }

    /// Build one presentation row from a snapshot, applying the independently
    /// tracked greyed + dimming state for that display.
    fn build_row(&self, snap: DisplaySnapshot) -> FlyoutRow {
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
            // Authoritative from the snapshot (a `set_dimming_info` refresh keeps it
            // in sync from the DimmingInfo channel).
            software_only: snap.software_only,
            hardware_floor_pct: dim.hardware_floor,
            min_perceived_pct: dim.min_perceived_pct,
        }
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
            row.software_only = dim.software_only;
            row.hardware_floor_pct = dim.hardware_floor;
            row.min_perceived_pct = dim.min_perceived_pct;
        }
    }

    /// Toggle software dimming for the row at `row_index`, returning the command
    /// to persist and re-plan.
    ///
    /// A greyed row, a **software-only** row, or an out-of-range index changes
    /// nothing and returns `None`. A software-only display has no hardware channel,
    /// so software dimming is the *only* dimming it has: its toggle is forced on and
    /// non-interactive in the flyout, and this guard makes a stray toggle event a
    /// no-op so the slider is never stranded. Otherwise the row's toggle state is
    /// updated optimistically and a [`UiCommand::SetDimmingEnabled`] is emitted for
    /// the app to apply.
    pub fn toggle_dimming(&mut self, row_index: usize, on: bool) -> Option<UiCommand> {
        let row = self.rows.get_mut(row_index)?;
        if row.greyed || row.software_only {
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
    ///
    /// Membership note: reviving a row (`false`) does not re-enrol it into an
    /// active "Link all" group until it is next dragged (re-enrolling at offset 0)
    /// or the next `set_displays` runs, so a just-revived row can briefly stay put
    /// while a *different* linked row is dragged — acceptable, documented behavior.
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
    /// empty vector. When link-all is on, the dragged slider sets the group's
    /// *master* and every non-greyed member is re-derived as
    /// `clamp(master + offset, 0, 100)`, preserving the per-monitor gaps captured
    /// when the link engaged (see [`link_toggled`](Self::link_toggled)); one
    /// [`UiCommand::SetLevel`] is emitted per moved member. Because each member is
    /// derived from the master and its own offset — never its previous value —
    /// pushing the group to a bound and back is drift-free: gaps saturate at 0/100
    /// and reopen exactly. A member pinned at a bound can make the dragged slider
    /// *saturate* before it reaches the pointer; that is margin preservation, not a
    /// bug. Greyed members emit nothing and keep their value. Otherwise only the
    /// touched row is updated and emitted.
    pub fn slider_changed(&mut self, row_index: usize, pct: u8) -> Vec<UiCommand> {
        let pct = pct.min(100);
        // A greyed row (or an out-of-range index) changes nothing and emits an
        // empty vector. Capturing the id in the same guard gives the link branch an
        // owned id (so no borrow of `rows` is held across the mutations below) at
        // the same one-clone cost the unlinked branch already paid.
        let Some(dragged_id) = self
            .rows
            .get(row_index)
            .filter(|row| !row.greyed)
            .map(|row| row.id.clone())
        else {
            return Vec::new();
        };

        let mut commands = Vec::new();
        if self.link_all {
            // Membership drift: a dragged row absent from the group (greyed when
            // link engaged and later revived, or an unenrolled hot-plug) joins at
            // offset 0 so its own slider is never frozen.
            let offset = if let Some(off) = self.link_group.offset_of(&dragged_id) {
                off
            } else {
                self.link_group.add(LINK_GROUP, dragged_id, 0);
                0
            };
            // Recover the master the dragged slider implies, then fan it out. The
            // saturating clamp is what yields margin preservation at the bounds.
            let master = i16::from(pct)
                .checked_sub(i16::from(offset))
                .unwrap_or(0)
                .clamp(0, 100);
            let master = u8::try_from(master).unwrap_or(0);
            for (id, target) in self.link_group.fan_out(LINK_GROUP, master) {
                // A member greyed after link engaged emits nothing / keeps its value.
                if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
                    if row.greyed {
                        continue;
                    }
                    row.level_pct = target;
                    commands.push(UiCommand::SetLevel { id, pct: target });
                }
            }
        } else if let Some(row) = self.rows.get_mut(row_index) {
            row.level_pct = pct;
            commands.push(UiCommand::SetLevel {
                id: dragged_id,
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

    /// Set the link-all toggle, (re)building the offset group. Pure presentation
    /// state; emits no command.
    ///
    /// Turning link **on** snapshots the current gaps: the **first non-greyed row**
    /// is the anchor (offset 0) and every other non-greyed row joins the group at
    /// `its level − anchor level`. From then on a drag fans a
    /// master out to those offsets (see [`slider_changed`](Self::slider_changed)),
    /// so the gaps present at this instant are what is preserved. Turning link
    /// **off** clears the group. Greyed rows are excluded; if one is later revived
    /// and dragged it re-enrols at offset 0.
    pub fn link_toggled(&mut self, on: bool) {
        self.link_all = on;
        self.link_group = SyncGroups::new();
        if on {
            let anchor_level = self.rows.iter().find(|r| !r.greyed).map(|r| r.level_pct);
            if let Some(anchor_level) = anchor_level {
                for row in &self.rows {
                    if row.greyed {
                        continue;
                    }
                    // level_pct and anchor_level are 0..=100, so the difference is
                    // -100..=100 and fits an i8; the total-arithmetic forms keep the
                    // lint wall happy and the fallbacks are unreachable.
                    let offset = i16::from(row.level_pct)
                        .checked_sub(i16::from(anchor_level))
                        .unwrap_or(0)
                        .clamp(-100, 100);
                    let offset = i8::try_from(offset).unwrap_or(0);
                    self.link_group.add(LINK_GROUP, row.id.clone(), offset);
                }
            }
        }
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

    /// Switch the accent colour. Pure presentation state; emits no command.
    pub fn set_accent(&mut self, accent: AccentChoice) {
        self.accent = accent;
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

    /// The current accent colour.
    #[must_use]
    pub fn accent(&self) -> AccentChoice {
        self.accent
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
        DisplayKind::InternalPanel => "Internal",
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
            software_only: false,
            user_level_pct: level,
            capabilities: Capabilities::default(),
        }
    }

    /// A software-only display: a real physical `kind` plus the runtime flag set.
    fn snap_software_only(
        serial: &str,
        name: &str,
        level: u8,
        kind: DisplayKind,
    ) -> DisplaySnapshot {
        DisplaySnapshot {
            software_only: true,
            ..snap(serial, name, level, kind)
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
            snap("B", "Middle", 50, DisplayKind::InternalPanel),
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
    fn set_displays_keeps_existing_row_positions_when_a_lower_id_hot_plugs_in() {
        let mut vm = FlyoutVm::new();
        // Two displays whose ids sort B < C.
        vm.set_displays(vec![
            snap("B", "Bee", 50, DisplayKind::ExternalDdc),
            snap("C", "Cee", 60, DisplayKind::ExternalDdc),
        ]);
        // A lower-sorting display "A" hot-plugs in mid-session. Order-stable: the
        // existing rows keep their positions and the newcomer is appended — a
        // re-sort would slide B/C down one and (because the flyout addresses a
        // slider drag by its positional row index) retarget a held thumb to the
        // wrong display.
        vm.set_displays(vec![
            snap("A", "Ay", 40, DisplayKind::ExternalDdc),
            snap("B", "Bee", 50, DisplayKind::ExternalDdc),
            snap("C", "Cee", 60, DisplayKind::ExternalDdc),
        ]);
        let ids: Vec<&str> = vm.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                id_of("B").as_str(),
                id_of("C").as_str(),
                id_of("A").as_str()
            ],
            "existing rows stay put; the newcomer is appended (never a re-sort)"
        );
        // The row at index 0 is still B — never reassigned to the lower-sorting A.
        assert_eq!(
            vm.rows().first().map(|r| r.id.as_str()),
            Some(id_of("B").as_str())
        );
    }

    #[test]
    fn set_displays_drop_keeps_surviving_row_positions_stable() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("B", "Bee", 50, DisplayKind::ExternalDdc),
            snap("C", "Cee", 60, DisplayKind::ExternalDdc),
        ]);
        // Hot-plug A in (appended): rows are now [B, C, A].
        vm.set_displays(vec![
            snap("A", "Ay", 40, DisplayKind::ExternalDdc),
            snap("B", "Bee", 50, DisplayKind::ExternalDdc),
            snap("C", "Cee", 60, DisplayKind::ExternalDdc),
        ]);
        // Now the middle display C disconnects. The survivors keep their
        // established positions: B stays at index 0 and A stays where it was
        // appended — no re-sort back to [A, B].
        vm.set_displays(vec![
            snap("A", "Ay", 40, DisplayKind::ExternalDdc),
            snap("B", "Bee", 50, DisplayKind::ExternalDdc),
        ]);
        let ids: Vec<&str> = vm.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![id_of("B").as_str(), id_of("A").as_str()],
            "a removal shifts nothing else: survivors keep their order"
        );
    }

    #[test]
    fn kind_labels_map_each_variant() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Ext", 10, DisplayKind::ExternalDdc),
            snap("B", "Int", 10, DisplayKind::InternalPanel),
        ]);
        let labels: Vec<&str> = vm.rows().iter().map(|r| r.kind_label.as_str()).collect();
        // Physical/provenance labels only — never "Software" (that is a runtime
        // control-mode, carried by `software_only`, not a display kind).
        assert_eq!(labels, vec!["External", "Internal"]);
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
    fn link_all_preserves_offsets() {
        let mut vm = FlyoutVm::new();
        // A=80 (anchor), B=50, C=60: B trails the anchor by 30, C by 20.
        vm.set_displays(vec![
            snap("A", "Left", 80, DisplayKind::ExternalDdc),
            snap("B", "Right", 50, DisplayKind::ExternalDdc),
            snap("C", "Third", 60, DisplayKind::ExternalDdc),
        ]);
        vm.link_toggled(true);
        // Drag the *non-anchor* B down 10 (50 → 40). The whole group slides down
        // 10 in lock-step, keeping every gap — it does NOT snap all rows to 40.
        let cmds = vm.slider_changed(1, 40);
        assert_eq!(
            cmds,
            vec![
                UiCommand::SetLevel {
                    id: id_of("A"),
                    pct: 70,
                },
                UiCommand::SetLevel {
                    id: id_of("B"),
                    pct: 40,
                },
                UiCommand::SetLevel {
                    id: id_of("C"),
                    pct: 50,
                },
            ]
        );
        // 80/50/60 all shifted down by 10 → 70/40/50; the 30- and 20-point gaps
        // that existed before the drag are intact.
        let levels: Vec<u8> = vm.rows().iter().map(|r| r.level_pct).collect();
        assert_eq!(levels, vec![70, 40, 50]);
    }

    #[test]
    fn link_all_offsets_saturate_and_reopen_without_drift() {
        let mut vm = FlyoutVm::new();
        // A=50 (anchor), B=80 (+30), C=60 (+10) — two members ABOVE the anchor.
        vm.set_displays(vec![
            snap("A", "Left", 50, DisplayKind::ExternalDdc),
            snap("B", "Right", 80, DisplayKind::ExternalDdc),
            snap("C", "Third", 60, DisplayKind::ExternalDdc),
        ]);
        vm.link_toggled(true);
        // Push the anchor to the top: B (+30) and C (+10) both saturate at 100.
        vm.slider_changed(0, 100);
        let saturated: Vec<u8> = vm.rows().iter().map(|r| r.level_pct).collect();
        assert_eq!(
            saturated,
            vec![100, 100, 100],
            "members clamp at the 100 bound"
        );
        // Bring the anchor back down: every original gap reopens *exactly* — the
        // saturated members carry no residue (fan_out re-derives from master +
        // offset, never the clamped previous value). Mirrors sync.rs's
        // `offset_clamps_at_bounds_without_drift`.
        vm.slider_changed(0, 50);
        let restored: Vec<u8> = vm.rows().iter().map(|r| r.level_pct).collect();
        assert_eq!(restored, vec![50, 80, 60], "gaps restored with zero drift");
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
    fn link_all_suppresses_setlevel_for_row_greyed_after_link() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Left", 40, DisplayKind::ExternalDdc),
            snap("B", "Right", 50, DisplayKind::ExternalDdc),
        ]);
        // Both non-greyed at link time, so B *joins* the group (A anchor offset 0,
        // B offset +10) — unlike a row greyed before link, which never joins.
        vm.link_toggled(true);
        // B goes unresponsive AFTER the link engaged: it is still a group member,
        // so fan_out yields a target for it, but the fan-out loop must skip it.
        vm.set_unresponsive(&id_of("B"), true);
        // Drag a *different* (non-greyed) row, A, to 30.
        let cmds = vm.slider_changed(0, 30);
        // Only A is commanded; the now-greyed member B emits no SetLevel...
        assert_eq!(
            cmds,
            vec![UiCommand::SetLevel {
                id: id_of("A"),
                pct: 30,
            }]
        );
        // ...and keeps its pre-grey level (fan_out would have put it at 40).
        assert_eq!(vm.rows().get(1).map(|r| r.level_pct), Some(50));
    }

    #[test]
    fn link_all_dragged_row_absent_from_group_joins_at_offset_zero() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Left", 80, DisplayKind::ExternalDdc),
            snap("B", "Right", 50, DisplayKind::ExternalDdc),
            snap("C", "Third", 60, DisplayKind::ExternalDdc),
        ]);
        // B is greyed when link engages, so only the anchor A and C join the
        // group (A offset 0, C offset -20). B is left out entirely.
        vm.set_unresponsive(&id_of("B"), true);
        vm.link_toggled(true);
        // B returns responsive but is still absent from the group —
        // `set_unresponsive` never touches membership (drift). Dragging B must not
        // freeze it: it joins at offset 0 (tracking the anchor) while the
        // established member C keeps its -20 offset. Absolute linking would drag C
        // to 30 too; offset linking keeps it 20 below → 10.
        vm.set_unresponsive(&id_of("B"), false);
        let cmds = vm.slider_changed(1, 30);
        assert_eq!(
            cmds,
            vec![
                UiCommand::SetLevel {
                    id: id_of("A"),
                    pct: 30,
                },
                UiCommand::SetLevel {
                    id: id_of("B"),
                    pct: 30,
                },
                UiCommand::SetLevel {
                    id: id_of("C"),
                    pct: 10,
                },
            ]
        );
        let levels: Vec<u8> = vm.rows().iter().map(|r| r.level_pct).collect();
        assert_eq!(
            levels,
            vec![30, 30, 10],
            "B never freezes; C keeps its offset"
        );
    }

    #[test]
    fn link_all_hot_plugged_row_joins_the_group() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Left", 80, DisplayKind::ExternalDdc),
            snap("B", "Right", 60, DisplayKind::ExternalDdc),
        ]);
        vm.link_toggled(true); // group: A offset 0, B offset -20
        // C hot-plugs in while linked; a new non-greyed row enrols at offset 0 so
        // it moves with the group instead of sitting frozen.
        vm.set_displays(vec![
            snap("A", "Left", 80, DisplayKind::ExternalDdc),
            snap("B", "Right", 60, DisplayKind::ExternalDdc),
            snap("C", "Third", 45, DisplayKind::ExternalDdc),
        ]);
        // Drag the anchor A down to 70: A and B keep their 20-point gap and C
        // (offset 0) tracks the anchor.
        let cmds = vm.slider_changed(0, 70);
        assert_eq!(
            cmds,
            vec![
                UiCommand::SetLevel {
                    id: id_of("A"),
                    pct: 70,
                },
                UiCommand::SetLevel {
                    id: id_of("B"),
                    pct: 50,
                },
                UiCommand::SetLevel {
                    id: id_of("C"),
                    pct: 70,
                },
            ]
        );
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
            software_only: false,
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
            snap_software_only("B", "Sw", 60, DisplayKind::InternalPanel),
        ]);
        let mut info = BTreeMap::new();
        info.insert(id_of("A"), dim(Some(0), 25, true)); // floor 0 ⇒ A == B
        info.insert(id_of("B"), dim(None, 25, true)); // floorless (software-only) ⇒ no markers
        vm.set_dimming_info(info);
        assert!(vm.rows().iter().all(FlyoutRow::markers_coincide));
    }

    #[test]
    fn rows_expose_marker_and_toggle_from_dimming_info() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Ext", 60, DisplayKind::ExternalDdc),
            snap_software_only("B", "Sw", 60, DisplayKind::InternalPanel),
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

    #[test]
    fn software_only_row_carries_the_flag_and_toggle_dimming_is_a_noop() {
        // #67: a software-only display carries the flag straight off the snapshot,
        // its "Software dimming" toggle can't be changed (the overlay is its only
        // channel), and its kind label stays physical provenance — never "Software".
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snap_software_only(
            "A",
            "Panel",
            60,
            DisplayKind::InternalPanel,
        )]);
        let row = vm.rows().first().unwrap();
        assert!(
            row.software_only,
            "the row must carry the software-only flag"
        );
        assert_eq!(
            row.kind_label, "Internal",
            "label stays physical provenance"
        );
        assert!(!row.greyed, "software-only is not the same as unresponsive");

        // The toggle is a no-op regardless of the requested state (forced on).
        assert_eq!(vm.toggle_dimming(0, false), None);
        assert_eq!(vm.toggle_dimming(0, true), None);
    }

    #[test]
    fn no_row_ever_labels_a_kind_software() {
        // Belt-and-braces: neither physical kind, nor a software-only display, ever
        // yields the string "Software" as a kind label.
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![
            snap("A", "Ext", 50, DisplayKind::ExternalDdc),
            snap("B", "Int", 50, DisplayKind::InternalPanel),
            snap_software_only("C", "SwPanel", 50, DisplayKind::InternalPanel),
        ]);
        assert!(vm.rows().iter().all(|r| r.kind_label != "Software"));
    }
}

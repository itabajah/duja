//! The [`DisplayManager`]: the pure heart of hot-plug handling.
//!
//! The manager does **not** talk to hardware or the OS. It consumes
//! enumeration results as plain data ([`DiscoveredDisplay`]) and emits
//! decisions as plain data ([`ManagerEvent`]); the platform layer feeds it and
//! executes what comes back. Enumerations arrive **post-debounce** (the
//! debouncer lives in [`crate::debounce`]) — the manager never re-debounces.
//!
//! # Diffing rules
//!
//! On every [`apply_enumeration`](DisplayManager::apply_enumeration):
//! - a never-seen id ⇒ [`ManagerEvent::Added`];
//! - a known id that is missing ⇒ [`ManagerEvent::Removed`] — its record is
//!   **kept**, not dropped, so a replug can restore it;
//! - a disconnected id that reappears ⇒ [`ManagerEvent::Reattached`], carrying
//!   the last user level recorded via
//!   [`record_user_level`](DisplayManager::record_user_level);
//! - a display previously marked unresponsive that is sighted again ⇒
//!   [`ManagerEvent::Responsive`];
//! - a connected id seen again ⇒ **no event** (an OS handle change is not a
//!   new display); its metadata and `last_seen` are refreshed silently.
//!
//! Events are ordered deterministically: `Added`/`Reattached`/`Responsive`
//! follow the enumeration's input order, then `Removed` events follow sorted
//! by id.
//!
//! # Identical twin monitors
//!
//! Some monitors ship without a serial, so two identical units share one
//! EDID-derived id. When a single enumeration contains N > 1 displays with the
//! same [`StableDisplayId`], the manager assigns deterministic slot suffixes
//! ([`StableDisplayId::with_slot`], slots `0..N-1`) **in the order given** —
//! the platform layer supplies connector order. Per-display settings therefore
//! **follow the port** for serial-less twins: swap the cables and the twins
//! swap settings. If the twin count for an EDID changes (e.g. one of a pair is
//! unplugged), the survivor enumerates alone and takes the bare, un-slotted
//! id — a distinct identity from either slot.

// RATIONALE: the domain vocabulary namespaces its types (DisplayManager,
// ManagerEvent) within the `manager` module; the names are fixed by the plan
// and read correctly at call sites.
#![allow(clippy::module_name_repetitions)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::id::StableDisplayId;
use crate::model::{Capabilities, DisplayKind, DisplaySnapshot};

/// The user level assumed for a display before any level is recorded, in
/// percent. Visible only until the first probe reflects the real value.
pub const DEFAULT_USER_LEVEL_PCT: u8 = 50;

/// One display as reported by a platform enumeration pass, as plain data.
///
/// The platform layer builds these from OS handles + EDID after each
/// (debounced) display-change event; the manager never sees the handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDisplay {
    /// Durable EDID-derived identity (pre-slotting; see the module docs on
    /// twins).
    pub id: StableDisplayId,
    /// Which backend class controls this display.
    pub kind: DisplayKind,
    /// Human-readable name, if the backend resolved one.
    pub name: Option<String>,
    /// Capabilities as probed (or as cached by the platform layer).
    pub capabilities: Capabilities,
}

/// A decision emitted by the manager for the platform layer / UI to execute.
///
/// Ids in events are always the **resolved** ids (slot-suffixed for twins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerEvent {
    /// A never-before-seen display appeared: spawn a worker, probe it.
    Added {
        /// The new display's resolved id.
        id: StableDisplayId,
    },
    /// A connected display disappeared: park its worker. State is kept for a
    /// future replug.
    Removed {
        /// The vanished display's resolved id.
        id: StableDisplayId,
    },
    /// A previously-removed display is back: respawn its worker and, if a
    /// level was recorded, restore it.
    Reattached {
        /// The returning display's resolved id.
        id: StableDisplayId,
        /// The last user level recorded for it, if any (percent).
        restore_level: Option<u8>,
    },
    /// The display stopped acknowledging control operations (watchdog): grey
    /// it in the UI, stop sending writes.
    Unresponsive {
        /// The stuck display's resolved id.
        id: StableDisplayId,
    },
    /// A display previously marked unresponsive was sighted by a successful
    /// enumeration: un-grey it, resume writes.
    Responsive {
        /// The recovered display's resolved id.
        id: StableDisplayId,
    },
}

/// The connection state of a display the manager knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayState {
    /// Present in the most recent enumeration.
    Connected {
        /// When the display was last seen by an enumeration.
        last_seen: Instant,
    },
    /// Missing from the most recent enumeration; record retained for replug.
    Disconnected {
        /// When the display went missing.
        since: Instant,
    },
}

/// Everything the manager retains for one resolved display id.
#[derive(Debug, Clone)]
struct DisplayRecord {
    /// Connected / disconnected, with the relevant instant.
    state: DisplayState,
    /// Backend class, refreshed from the latest sighting.
    kind: DisplayKind,
    /// Resolved name from the latest sighting, if any.
    name: Option<String>,
    /// Capabilities from the latest sighting.
    capabilities: Capabilities,
    /// The last level recorded via [`DisplayManager::record_user_level`].
    last_user_level: Option<u8>,
    /// `false` after the watchdog marks the display stuck, until re-sighted.
    responsive: bool,
}

/// Pure hot-plug state: diffing, per-display state, and level restore.
///
/// Keyed by resolved [`StableDisplayId`] in a `BTreeMap`, so every listing
/// comes out sorted by id and the diff path allocates only a few small id
/// sets per (rare) hot-plug pass.
///
/// # Examples
/// ```
/// # fn edid_id(serial: u32) -> duja_core::id::StableDisplayId {
/// #     let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x04, 0x21, 0, 0];
/// #     e.extend_from_slice(&serial.to_le_bytes());
/// #     e.resize(127, 0);
/// #     let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
/// #     e.push(sum.wrapping_neg());
/// #     duja_core::id::StableDisplayId::from_edid(&e).unwrap()
/// # }
/// use std::time::Instant;
/// use duja_core::manager::{DiscoveredDisplay, DisplayManager, ManagerEvent};
/// use duja_core::model::{Capabilities, DisplayKind};
///
/// let monitor = DiscoveredDisplay {
///     id: edid_id(7),
///     kind: DisplayKind::ExternalDdc,
///     name: Some("Office".to_owned()),
///     capabilities: Capabilities::default(),
/// };
///
/// let mut manager = DisplayManager::new();
/// let t0 = Instant::now();
///
/// // First sighting: a decision to add.
/// let events = manager.apply_enumeration(vec![monitor.clone()], t0);
/// assert!(matches!(events.as_slice(), [ManagerEvent::Added { .. }]));
///
/// // The user dims it; the monitor is unplugged, then replugged:
/// // the same EDID brings the level back.
/// manager.record_user_level(&monitor.id, 30);
/// manager.apply_enumeration(vec![], t0);
/// let events = manager.apply_enumeration(vec![monitor], t0);
/// assert!(matches!(
///     events.as_slice(),
///     [ManagerEvent::Reattached { restore_level: Some(30), .. }]
/// ));
/// ```
#[derive(Debug, Clone, Default)]
pub struct DisplayManager {
    records: BTreeMap<StableDisplayId, DisplayRecord>,
}

impl DisplayManager {
    /// Create a manager with no known displays.
    #[must_use]
    pub fn new() -> Self {
        DisplayManager {
            records: BTreeMap::new(),
        }
    }

    /// Apply one (post-debounce) enumeration pass observed at `now`, returning
    /// the resulting decisions.
    ///
    /// See the module docs for the diffing rules, event ordering, and twin
    /// slotting. This method is total: any input, including pathological
    /// duplicate ids, yields events without panicking.
    pub fn apply_enumeration(
        &mut self,
        seen: Vec<DiscoveredDisplay>,
        now: Instant,
    ) -> Vec<ManagerEvent> {
        let resolved = resolve_slots(seen);
        let mut events = Vec::new();

        // The membership set the removal sweep diffs against.
        let seen_ids: BTreeSet<StableDisplayId> =
            resolved.iter().map(|(id, _)| id.clone()).collect();

        // Sightings first, in the enumeration's input order.
        for (id, display) in resolved {
            match self.records.get_mut(&id) {
                None => {
                    self.records.insert(
                        id.clone(),
                        DisplayRecord {
                            state: DisplayState::Connected { last_seen: now },
                            kind: display.kind,
                            name: display.name,
                            capabilities: display.capabilities,
                            last_user_level: None,
                            responsive: true,
                        },
                    );
                    events.push(ManagerEvent::Added { id });
                }
                Some(record) => {
                    let was_disconnected =
                        matches!(record.state, DisplayState::Disconnected { .. });
                    let was_unresponsive = !record.responsive;
                    record.state = DisplayState::Connected { last_seen: now };
                    record.kind = display.kind;
                    record.name = display.name;
                    record.capabilities = display.capabilities;
                    record.responsive = true;
                    if was_disconnected {
                        events.push(ManagerEvent::Reattached {
                            id: id.clone(),
                            restore_level: record.last_user_level,
                        });
                    }
                    if was_unresponsive {
                        events.push(ManagerEvent::Responsive { id });
                    }
                }
            }
        }

        // Then the removal sweep, in id order (`BTreeMap` iteration). Records
        // are retained — a replug must be able to restore them.
        for (id, record) in &mut self.records {
            if matches!(record.state, DisplayState::Connected { .. }) && !seen_ids.contains(id) {
                record.state = DisplayState::Disconnected { since: now };
                events.push(ManagerEvent::Removed { id: id.clone() });
            }
        }

        events
    }

    /// Record the user's chosen level (percent, clamped to 100) for a display,
    /// so a later [`ManagerEvent::Reattached`] can restore it.
    ///
    /// Returns `true` if the display is known (connected **or** disconnected —
    /// levels survive unplugs), `false` for an unknown id.
    pub fn record_user_level(&mut self, id: &StableDisplayId, pct: u8) -> bool {
        match self.records.get_mut(id) {
            Some(record) => {
                record.last_user_level = Some(pct.min(100));
                true
            }
            None => false,
        }
    }

    /// Mark a connected display as unresponsive (fed by the P3 watchdog when
    /// a control operation gets stuck).
    ///
    /// Returns the [`ManagerEvent::Unresponsive`] decision the first time, or
    /// `None` if the display is unknown, disconnected, or already marked
    /// (idempotent). Responsiveness returns via the next successful
    /// enumeration that sights the display, which emits
    /// [`ManagerEvent::Responsive`].
    pub fn mark_unresponsive(&mut self, id: &StableDisplayId) -> Option<ManagerEvent> {
        let record = self.records.get_mut(id)?;
        if record.responsive && matches!(record.state, DisplayState::Connected { .. }) {
            record.responsive = false;
            Some(ManagerEvent::Unresponsive { id: id.clone() })
        } else {
            None
        }
    }

    /// UI-facing snapshots of every **connected** display, sorted by id.
    ///
    /// A display with no resolved name falls back to its id string; a display
    /// with no recorded level reports [`DEFAULT_USER_LEVEL_PCT`]. Unresponsive
    /// displays are included (the UI greys them; it does not hide them).
    #[must_use]
    pub fn snapshots(&self) -> Vec<DisplaySnapshot> {
        self.records
            .iter()
            .filter(|(_, record)| matches!(record.state, DisplayState::Connected { .. }))
            .map(|(id, record)| DisplaySnapshot {
                id: id.clone(),
                name: record.name.clone().unwrap_or_else(|| id.to_string()),
                kind: record.kind,
                user_level_pct: record.last_user_level.unwrap_or(DEFAULT_USER_LEVEL_PCT),
                capabilities: record.capabilities.clone(),
            })
            .collect()
    }

    /// The connection state of `id`, if the manager knows it.
    #[must_use]
    pub fn state_of(&self, id: &StableDisplayId) -> Option<DisplayState> {
        self.records.get(id).map(|record| record.state)
    }

    /// Whether `id` is currently believed responsive; `None` for unknown ids.
    ///
    /// The flag is sticky across a disconnect: a display that vanished while
    /// unresponsive stays flagged until an enumeration sights it again.
    #[must_use]
    pub fn is_responsive(&self, id: &StableDisplayId) -> Option<bool> {
        self.records.get(id).map(|record| record.responsive)
    }
}

/// Assign `-slot<n>` twin suffixes to a list of ids, in the order given.
///
/// Ids that occur more than once get `-slot<n>` suffixes ([`StableDisplayId::with_slot`],
/// slots `0..N-1`) assigned in input (connector) order; ids that occur exactly
/// once pass through untouched. Pure and total for any input.
///
/// This is the exact twin-slotting rule
/// [`DisplayManager::apply_enumeration`] applies, exposed so backends that
/// address displays **without** the manager (e.g. `dujactl`) present and route
/// twins the same way. Because the assignment depends only on input order, a
/// backend that re-enumerates in the same deterministic order can pair a
/// `-slot<n>` id back to the Nth bare-id match (see
/// [`crate::id::select_slot_match`]).
#[must_use]
pub fn assign_twin_slots(ids: &[StableDisplayId]) -> Vec<StableDisplayId> {
    // First pass: which ids collide?
    let mut once: BTreeSet<&StableDisplayId> = BTreeSet::new();
    let mut duplicated: BTreeSet<&StableDisplayId> = BTreeSet::new();
    for id in ids {
        if !once.insert(id) {
            duplicated.insert(id);
        }
    }

    // Second pass: suffix colliding ids in the order given.
    let mut next_slot: BTreeMap<&StableDisplayId, u32> = BTreeMap::new();
    ids.iter()
        .map(|id| {
            if duplicated.contains(id) {
                let slot = next_slot.entry(id).or_insert(0);
                let resolved = id.with_slot(*slot);
                *slot = slot.wrapping_add(1);
                resolved
            } else {
                id.clone()
            }
        })
        .collect()
}

/// Resolve serial-less-twin collisions in one enumeration pass, pairing each
/// resolved id back to its [`DiscoveredDisplay`]. See [`assign_twin_slots`].
fn resolve_slots(seen: Vec<DiscoveredDisplay>) -> Vec<(StableDisplayId, DiscoveredDisplay)> {
    let ids: Vec<StableDisplayId> = seen.iter().map(|d| d.id.clone()).collect();
    assign_twin_slots(&ids).into_iter().zip(seen).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Capabilities, DisplayKind, Feature};
    use crate::testing::FakeClock;
    use proptest::prelude::*;
    use std::time::Duration;

    /// Build a valid EDID (manufacturer `AAA`, product 0) with the given
    /// numeric serial. Non-zero serials yield `AAA-0000-s<n>`; serial 0 yields
    /// the hash-fallback id — exactly the serial-less-twin situation.
    fn id_with_serial(serial: u32) -> StableDisplayId {
        let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        e.push(0x04); // bytes 8..=9 encode "AAA"
        e.push(0x21);
        e.push(0x00); // product code = 0
        e.push(0x00);
        e.extend_from_slice(&serial.to_le_bytes());
        e.resize(127, 0x00);
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg());
        StableDisplayId::from_edid(&e).unwrap()
    }

    /// Distinct single-serial ids sorting as `a < b < c`.
    fn a() -> StableDisplayId {
        id_with_serial(1)
    }
    fn b() -> StableDisplayId {
        id_with_serial(2)
    }
    fn c() -> StableDisplayId {
        id_with_serial(3)
    }
    /// A serial-less (hash-identity) id — twins of this model collide.
    fn twin() -> StableDisplayId {
        id_with_serial(0)
    }

    fn disc(id: &StableDisplayId) -> DiscoveredDisplay {
        DiscoveredDisplay {
            id: id.clone(),
            kind: DisplayKind::ExternalDdc,
            name: Some("Monitor".to_owned()),
            capabilities: Capabilities::default(),
        }
    }

    fn disc_named(id: &StableDisplayId, name: &str) -> DiscoveredDisplay {
        DiscoveredDisplay {
            name: Some(name.to_owned()),
            ..disc(id)
        }
    }

    fn disc_unnamed(id: &StableDisplayId) -> DiscoveredDisplay {
        DiscoveredDisplay {
            name: None,
            ..disc(id)
        }
    }

    fn now() -> Instant {
        FakeClock::new().now()
    }

    fn added(id: StableDisplayId) -> ManagerEvent {
        ManagerEvent::Added { id }
    }
    fn removed(id: StableDisplayId) -> ManagerEvent {
        ManagerEvent::Removed { id }
    }
    fn reattached(id: StableDisplayId, restore_level: Option<u8>) -> ManagerEvent {
        ManagerEvent::Reattached { id, restore_level }
    }

    #[test]
    fn empty_manager_knows_nothing() {
        let m = DisplayManager::new();
        assert!(m.snapshots().is_empty());
        assert_eq!(m.state_of(&a()), None);
        assert_eq!(m.is_responsive(&a()), None);
    }

    #[test]
    fn first_enumeration_adds_displays_in_input_order() {
        let mut m = DisplayManager::new();
        // Deliberately out of id order: events follow INPUT order.
        let events = m.apply_enumeration(vec![disc(&b()), disc(&a())], now());
        assert_eq!(events, vec![added(b()), added(a())]);
        // Snapshots are nevertheless sorted by id.
        let ids: Vec<StableDisplayId> = m.snapshots().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![a(), b()]);
    }

    #[test]
    fn os_handle_change_does_not_create_new_display() {
        let mut m = DisplayManager::new();
        let t0 = now();
        assert_eq!(m.apply_enumeration(vec![disc(&a())], t0), vec![added(a())]);
        // The OS re-enumerates the same panel (new HMONITOR, same EDID):
        // same stable id -> NO Added, no events at all.
        assert_eq!(m.apply_enumeration(vec![disc(&a())], t0), vec![]);
        assert_eq!(m.snapshots().len(), 1);
    }

    #[test]
    fn re_enumeration_refreshes_metadata_silently() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc_named(&a(), "Old name")], now());

        let mut caps = Capabilities::default();
        caps.features.insert(Feature::Brightness);
        caps.hardware_range = true;
        let refreshed = DiscoveredDisplay {
            capabilities: caps.clone(),
            ..disc_named(&a(), "New name")
        };
        assert_eq!(m.apply_enumeration(vec![refreshed], now()), vec![]);

        let snaps = m.snapshots();
        let snap = snaps.first().unwrap();
        assert_eq!(snap.name, "New name");
        assert_eq!(snap.capabilities, caps);
    }

    #[test]
    fn missing_display_is_removed_but_state_kept() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&a()), disc(&b())], now());
        let t1 = now();
        assert_eq!(
            m.apply_enumeration(vec![disc(&a())], t1),
            vec![removed(b())]
        );
        // Gone from snapshots, but NOT forgotten.
        let ids: Vec<StableDisplayId> = m.snapshots().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![a()]);
        assert_eq!(
            m.state_of(&b()),
            Some(DisplayState::Disconnected { since: t1 })
        );
    }

    #[test]
    fn replug_same_edid_restores_last_level() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&a())], now());
        assert!(m.record_user_level(&a(), 40));

        assert_eq!(m.apply_enumeration(vec![], now()), vec![removed(a())]);
        // The user unplugs and replugs the same monitor: identity survives via
        // EDID, and the recorded level comes back with it.
        assert_eq!(
            m.apply_enumeration(vec![disc(&a())], now()),
            vec![reattached(a(), Some(40))]
        );
        assert_eq!(m.snapshots().first().unwrap().user_level_pct, 40);
    }

    #[test]
    fn a_seen_display_never_re_emits_added_while_retained() {
        // The app pushes a saved level to the engine only on the FIRST sight of a
        // display (its one-shot `applied` guard). That guard is sufficient
        // precisely because the manager never emits `Added` twice for a retained
        // record: a mere re-enumeration is silent, and a reconnect is a
        // `Reattached` (which the engine restores). This pins that invariant down.
        let mut m = DisplayManager::new();

        // First sight: exactly one Added.
        assert_eq!(
            m.apply_enumeration(vec![disc(&a())], now()),
            vec![added(a())]
        );
        // Re-enumeration while connected: silent (no second Added).
        assert_eq!(m.apply_enumeration(vec![disc(&a())], now()), vec![]);

        // Unplug then replug: a Reattached, NEVER a second Added.
        assert_eq!(m.apply_enumeration(vec![], now()), vec![removed(a())]);
        let replug = m.apply_enumeration(vec![disc(&a())], now());
        assert_eq!(replug, vec![reattached(a(), None)]);

        // Repeated unplug/replug cycles keep yielding Reattached, never Added.
        for _ in 0..3 {
            let _ = m.apply_enumeration(vec![], now());
            let evs = m.apply_enumeration(vec![disc(&a())], now());
            assert!(
                !evs.iter().any(|e| matches!(e, ManagerEvent::Added { .. })),
                "a retained display must never re-emit Added, got {evs:?}"
            );
            assert!(
                evs.iter()
                    .any(|e| matches!(e, ManagerEvent::Reattached { .. })),
                "a reconnect must reattach, got {evs:?}"
            );
        }
    }

    #[test]
    fn reattach_without_recorded_level_carries_none() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&a())], now());
        let _ = m.apply_enumeration(vec![], now());
        assert_eq!(
            m.apply_enumeration(vec![disc(&a())], now()),
            vec![reattached(a(), None)]
        );
    }

    #[test]
    fn record_user_level_reports_known_and_clamps() {
        let mut m = DisplayManager::new();
        assert!(
            !m.record_user_level(&a(), 30),
            "unknown id must report false"
        );

        let _ = m.apply_enumeration(vec![disc(&a())], now());
        assert!(m.record_user_level(&a(), 255));
        assert_eq!(m.snapshots().first().unwrap().user_level_pct, 100);

        // Recording still works while disconnected (state is kept).
        let _ = m.apply_enumeration(vec![], now());
        assert!(m.record_user_level(&a(), 25));
        assert_eq!(
            m.apply_enumeration(vec![disc(&a())], now()),
            vec![reattached(a(), Some(25))]
        );
    }

    #[test]
    fn assign_twin_slots_slots_only_collisions_in_order() {
        let t = twin();
        // Uniquely-identified ids pass through untouched.
        assert_eq!(assign_twin_slots(&[a(), b()]), vec![a(), b()]);
        // Collisions get slot suffixes in input order; a non-colliding id
        // interleaved keeps its bare identity.
        assert_eq!(
            assign_twin_slots(&[t.clone(), a(), t.clone()]),
            vec![t.with_slot(0), a(), t.with_slot(1)]
        );
    }

    #[test]
    fn identical_twin_monitors_without_serial_get_distinct_slots() {
        let mut m = DisplayManager::new();
        let t = twin();
        let events = m.apply_enumeration(vec![disc(&t), disc(&t)], now());
        // Slot suffixes assigned in the order given (connector order).
        assert_eq!(events, vec![added(t.with_slot(0)), added(t.with_slot(1))]);
        let ids: Vec<StableDisplayId> = m.snapshots().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![t.with_slot(0), t.with_slot(1)]);
    }

    #[test]
    fn twin_slots_stable_across_repeated_enumerations() {
        let mut m = DisplayManager::new();
        let t = twin();
        let _ = m.apply_enumeration(vec![disc(&t), disc(&t)], now());
        // Same twins, same order: nothing changed, nothing fires.
        assert_eq!(m.apply_enumeration(vec![disc(&t), disc(&t)], now()), vec![]);
        assert_eq!(m.snapshots().len(), 2);
    }

    #[test]
    fn twin_levels_follow_their_slot_across_replug() {
        let mut m = DisplayManager::new();
        let t = twin();
        let _ = m.apply_enumeration(vec![disc(&t), disc(&t)], now());
        assert!(m.record_user_level(&t.with_slot(0), 10));
        assert!(m.record_user_level(&t.with_slot(1), 90));

        let _ = m.apply_enumeration(vec![], now());
        // Settings follow the port: replug in the same connector order and
        // each twin gets its own level back.
        assert_eq!(
            m.apply_enumeration(vec![disc(&t), disc(&t)], now()),
            vec![
                reattached(t.with_slot(0), Some(10)),
                reattached(t.with_slot(1), Some(90)),
            ]
        );
    }

    #[test]
    fn three_twins_get_three_slots() {
        let mut m = DisplayManager::new();
        let t = twin();
        let events = m.apply_enumeration(vec![disc(&t), disc(&t), disc(&t)], now());
        assert_eq!(
            events,
            vec![
                added(t.with_slot(0)),
                added(t.with_slot(1)),
                added(t.with_slot(2)),
            ]
        );
    }

    #[test]
    fn single_twin_survivor_takes_the_bare_identity() {
        let mut m = DisplayManager::new();
        let t = twin();
        let _ = m.apply_enumeration(vec![disc(&t), disc(&t)], now());
        // One twin unplugged: the survivor enumerates alone, so no slotting
        // applies and it takes the bare id — a new identity by design
        // (the manager cannot know WHICH port survived; documented).
        let events = m.apply_enumeration(vec![disc(&t)], now());
        assert_eq!(
            events,
            vec![
                added(t.clone()),
                removed(t.with_slot(0)),
                removed(t.with_slot(1)),
            ]
        );
    }

    #[test]
    fn mark_unresponsive_emits_once_and_keeps_display_visible() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&a())], now());
        assert_eq!(
            m.mark_unresponsive(&a()),
            Some(ManagerEvent::Unresponsive { id: a() })
        );
        assert_eq!(m.is_responsive(&a()), Some(false));
        // Greyed, not hidden: the UI decides presentation.
        assert_eq!(m.snapshots().len(), 1);
        // Idempotent: no duplicate decisions for the watchdog to re-execute.
        assert_eq!(m.mark_unresponsive(&a()), None);
    }

    #[test]
    fn mark_unresponsive_on_unknown_or_disconnected_is_a_noop() {
        let mut m = DisplayManager::new();
        assert_eq!(m.mark_unresponsive(&a()), None);

        let _ = m.apply_enumeration(vec![disc(&a())], now());
        let _ = m.apply_enumeration(vec![], now());
        assert_eq!(m.mark_unresponsive(&a()), None);
        assert_eq!(m.is_responsive(&a()), Some(true));
    }

    #[test]
    fn sighting_restores_responsiveness() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&a())], now());
        let _ = m.mark_unresponsive(&a());
        // Still connected; the next successful enumeration proves it's alive.
        assert_eq!(
            m.apply_enumeration(vec![disc(&a())], now()),
            vec![ManagerEvent::Responsive { id: a() }]
        );
        assert_eq!(m.is_responsive(&a()), Some(true));
    }

    #[test]
    fn removal_mid_write_marks_disconnected_without_panic() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&a())], now());
        // A write gets stuck in the driver; the watchdog fires...
        assert_eq!(
            m.mark_unresponsive(&a()),
            Some(ManagerEvent::Unresponsive { id: a() })
        );
        // ...and THEN the monitor is unplugged mid-write. No panic; a clean
        // Removed decision and retained state.
        let t1 = now();
        assert_eq!(m.apply_enumeration(vec![], t1), vec![removed(a())]);
        assert_eq!(
            m.state_of(&a()),
            Some(DisplayState::Disconnected { since: t1 })
        );
        // The unresponsive flag is sticky across the disconnect.
        assert_eq!(m.is_responsive(&a()), Some(false));
        // Replug: existence first, then health — Reattached, then Responsive.
        assert_eq!(
            m.apply_enumeration(vec![disc(&a())], now()),
            vec![reattached(a(), None), ManagerEvent::Responsive { id: a() },]
        );
        assert_eq!(m.is_responsive(&a()), Some(true));
    }

    #[test]
    fn snapshots_are_connected_only_and_sorted() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&c()), disc(&a()), disc(&b())], now());
        let _ = m.apply_enumeration(vec![disc(&c()), disc(&a())], now());
        let ids: Vec<StableDisplayId> = m.snapshots().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![a(), c()]);
    }

    #[test]
    fn snapshot_name_falls_back_to_id_string() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc_unnamed(&a())], now());
        assert_eq!(m.snapshots().first().unwrap().name, a().to_string());
    }

    #[test]
    fn snapshot_defaults_then_tracks_recorded_level() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&a())], now());
        assert_eq!(
            m.snapshots().first().unwrap().user_level_pct,
            DEFAULT_USER_LEVEL_PCT
        );
        assert!(m.record_user_level(&a(), 73));
        assert_eq!(m.snapshots().first().unwrap().user_level_pct, 73);
    }

    #[test]
    fn state_of_tracks_enumeration_instants() {
        let mut clk = FakeClock::new();
        let mut m = DisplayManager::new();

        let t0 = clk.now();
        let _ = m.apply_enumeration(vec![disc(&a())], t0);
        assert_eq!(
            m.state_of(&a()),
            Some(DisplayState::Connected { last_seen: t0 })
        );

        clk.advance(Duration::from_millis(750));
        let t1 = clk.now();
        assert_eq!(m.apply_enumeration(vec![disc(&a())], t1), vec![]);
        assert_eq!(
            m.state_of(&a()),
            Some(DisplayState::Connected { last_seen: t1 })
        );

        clk.advance(Duration::from_millis(750));
        let t2 = clk.now();
        let _ = m.apply_enumeration(vec![], t2);
        assert_eq!(
            m.state_of(&a()),
            Some(DisplayState::Disconnected { since: t2 })
        );
    }

    #[test]
    fn removed_events_are_sorted_by_id() {
        let mut m = DisplayManager::new();
        let _ = m.apply_enumeration(vec![disc(&b()), disc(&a()), disc(&c())], now());
        assert_eq!(
            m.apply_enumeration(vec![], now()),
            vec![removed(a()), removed(b()), removed(c())]
        );
    }

    // --- property tests (plan §4.1: DisplayManager diffing) ---

    /// One scripted operation against the manager.
    #[derive(Debug, Clone)]
    enum Op {
        /// Enumerate the displays with these pool selectors (dups = twins).
        Enumerate(Vec<u32>),
        /// Record a user level for a pool id.
        Record(u32, u8),
        /// Watchdog marks a pool id unresponsive.
        Mark(u32),
    }

    /// Pool of four distinct ids; selector 0 maps to the hash-identity twin.
    fn pool_id(n: u32) -> StableDisplayId {
        id_with_serial(n.wrapping_rem(4))
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            prop::collection::vec(0u32..8, 0..6).prop_map(Op::Enumerate),
            (0u32..8, any::<u8>()).prop_map(|(sel, pct)| Op::Record(sel, pct)),
            (0u32..8).prop_map(Op::Mark),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2_000))]

        /// The manager never panics for arbitrary operation sequences, and its
        /// visible invariants hold after every step: snapshots strictly sorted
        /// by unique id, levels within 0..=100, every snapshot Connected.
        #[test]
        fn apply_enumeration_never_panics(ops in prop::collection::vec(op_strategy(), 0..12)) {
            let mut clk = FakeClock::new();
            let mut m = DisplayManager::new();
            for op in ops {
                clk.advance(Duration::from_millis(50));
                match op {
                    Op::Enumerate(sels) => {
                        let seen: Vec<DiscoveredDisplay> =
                            sels.iter().map(|&s| disc(&pool_id(s))).collect();
                        let _ = m.apply_enumeration(seen, clk.now());
                    }
                    Op::Record(sel, pct) => {
                        let _ = m.record_user_level(&pool_id(sel), pct);
                    }
                    Op::Mark(sel) => {
                        let _ = m.mark_unresponsive(&pool_id(sel));
                    }
                }
                let snaps = m.snapshots();
                for pair in snaps.windows(2) {
                    if let [x, y] = pair {
                        prop_assert!(x.id < y.id, "snapshots unsorted or duplicated");
                    }
                }
                for s in &snaps {
                    prop_assert!(s.user_level_pct <= 100);
                    prop_assert!(
                        matches!(m.state_of(&s.id), Some(DisplayState::Connected { .. })),
                        "snapshot of a non-connected display"
                    );
                    prop_assert!(m.is_responsive(&s.id).is_some());
                }
            }
        }

        /// Applying the same enumeration twice is silent the second time:
        /// slot assignment and diffing are deterministic and idempotent.
        #[test]
        fn second_identical_enumeration_is_silent(sels in prop::collection::vec(0u32..8, 0..6)) {
            let mut clk = FakeClock::new();
            let mut m = DisplayManager::new();
            let seen: Vec<DiscoveredDisplay> = sels.iter().map(|&s| disc(&pool_id(s))).collect();
            let _ = m.apply_enumeration(seen.clone(), clk.now());
            clk.advance(Duration::from_millis(750));
            prop_assert_eq!(m.apply_enumeration(seen, clk.now()), vec![]);
        }
    }
}

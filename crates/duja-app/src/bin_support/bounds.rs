//! An app-side map from a **resolved** display id to its pixel bounds.
//!
//! `duja-core`'s `DiscoveredDisplay` is frozen and carries no bounds, so the app
//! keeps them here, refreshed on every enumeration. Entries are stored in the
//! exact deterministic order the backend reports them (DDC first, then panels),
//! each as `(bare id, Option<bounds>)`. A lookup for a resolved id reuses the
//! same [`select_slot_match`] routing the
//! controller factory uses, so an identical-twin `-slot<n>` id resolves to the
//! Nth bare-id match — the same slot the manager assigned, because both walk the
//! same input order. Panels contribute `None` bounds (no monitor rect is
//! plumbed for them in P4).

use duja_core::dimmer::DisplayBounds;
use duja_core::id::{StableDisplayId, select_slot_match};

/// Resolved-id → bounds, backed by the ordered enumeration.
#[derive(Debug, Clone, Default)]
pub(crate) struct BoundsMap {
    entries: Vec<(String, Option<DisplayBounds>)>,
}

impl BoundsMap {
    /// Build from the ordered `(bare id, bounds)` enumeration.
    pub(crate) fn new(entries: Vec<(String, Option<DisplayBounds>)>) -> Self {
        BoundsMap { entries }
    }

    /// The bounds for a resolved display id, or `None` if unknown / panel.
    pub(crate) fn bounds_for(&self, resolved: &StableDisplayId) -> Option<DisplayBounds> {
        let candidates: Vec<&str> = self.entries.iter().map(|(id, _)| id.as_str()).collect();
        let idx = select_slot_match(resolved.as_str(), &candidates)?;
        self.entries.get(idx).and_then(|(_, bounds)| *bounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x5B09, Some(serial)).unwrap()
    }

    fn bounds(x: i32) -> DisplayBounds {
        DisplayBounds::new(x, 0, 1920, 1080)
    }

    #[test]
    fn resolves_a_plain_id() {
        let map = BoundsMap::new(vec![
            (id("A").as_str().to_owned(), Some(bounds(0))),
            (id("B").as_str().to_owned(), Some(bounds(1920))),
        ]);
        assert_eq!(map.bounds_for(&id("A")), Some(bounds(0)));
        assert_eq!(map.bounds_for(&id("B")), Some(bounds(1920)));
    }

    #[test]
    fn unknown_id_yields_none() {
        let map = BoundsMap::new(vec![(id("A").as_str().to_owned(), Some(bounds(0)))]);
        assert_eq!(map.bounds_for(&id("Z")), None);
    }

    #[test]
    fn panel_entry_reports_no_bounds() {
        let map = BoundsMap::new(vec![(id("A").as_str().to_owned(), None)]);
        assert_eq!(map.bounds_for(&id("A")), None);
    }

    #[test]
    fn twin_slots_map_to_the_nth_bare_match() {
        // Two serial-less twins share a bare id; the manager resolves them to
        // <bare>-slot0 / -slot1 in enumeration order. Each slot must pick the
        // Nth entry's bounds.
        let bare = StableDisplayId::from_parts("GSM", 0x5B09, None).unwrap();
        let map = BoundsMap::new(vec![
            (bare.as_str().to_owned(), Some(bounds(0))),
            (bare.as_str().to_owned(), Some(bounds(1920))),
        ]);
        assert_eq!(map.bounds_for(&bare.with_slot(0)), Some(bounds(0)));
        assert_eq!(map.bounds_for(&bare.with_slot(1)), Some(bounds(1920)));
        // A slot beyond the twins resolves to nothing.
        assert_eq!(map.bounds_for(&bare.with_slot(2)), None);
    }

    #[test]
    fn empty_map_is_all_none() {
        let map = BoundsMap::default();
        assert_eq!(map.bounds_for(&id("A")), None);
    }
}

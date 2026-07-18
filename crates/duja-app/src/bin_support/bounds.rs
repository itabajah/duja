//! An app-side map from a **resolved** display id to its pixel bounds and GDI
//! device name.
//!
//! `duja-core`'s `DiscoveredDisplay` is frozen and carries no bounds, so the app
//! keeps them here, refreshed on every enumeration. Entries are stored in the
//! exact deterministic order the backend reports them (DDC first, then panels),
//! each as `(bare id, Option<bounds>, Option<gdi device>)`. A lookup for a
//! resolved id reuses the same [`select_slot_match`] routing the
//! controller factory uses, so an identical-twin `-slot<n>` id resolves to the
//! Nth bare-id match — the same slot the manager assigned, because both walk the
//! same input order. WMI panels contribute `None` bounds and `None` device,
//! whereas a DDC-fallback internal panel carries DDC geometry like any DDC
//! display; the GDI device name is the gamma channel's ramp target.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use duja_core::dimmer::DisplayBounds;
use duja_core::id::{StableDisplayId, select_slot_match};

/// Resolved-id → bounds + GDI device, backed by the ordered enumeration.
#[derive(Debug, Clone, Default)]
pub(crate) struct BoundsMap {
    entries: Vec<(String, Option<DisplayBounds>, Option<String>)>,
}

impl BoundsMap {
    /// Build from the ordered `(bare id, bounds, gdi device)` enumeration.
    pub(crate) fn new(entries: Vec<(String, Option<DisplayBounds>, Option<String>)>) -> Self {
        BoundsMap { entries }
    }

    /// The index of the entry a resolved id routes to (twin-slot aware).
    fn index_of(&self, resolved: &StableDisplayId) -> Option<usize> {
        let candidates: Vec<&str> = self.entries.iter().map(|(id, _, _)| id.as_str()).collect();
        select_slot_match(resolved.as_str(), &candidates)
    }

    /// The bounds for a resolved display id, or `None` if unknown / panel.
    pub(crate) fn bounds_for(&self, resolved: &StableDisplayId) -> Option<DisplayBounds> {
        let idx = self.index_of(resolved)?;
        self.entries.get(idx).and_then(|(_, bounds, _)| *bounds)
    }

    /// The GDI device name (e.g. `\\.\DISPLAY1`) for a resolved display id — the
    /// gamma channel's ramp target — or `None` if unknown / panel.
    pub(crate) fn device_for(&self, resolved: &StableDisplayId) -> Option<String> {
        let idx = self.index_of(resolved)?;
        self.entries
            .get(idx)
            .and_then(|(_, _, device)| device.clone())
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

    fn dev(n: u32) -> String {
        format!(r"\\.\DISPLAY{n}")
    }

    #[test]
    fn resolves_a_plain_id() {
        let map = BoundsMap::new(vec![
            (id("A").as_str().to_owned(), Some(bounds(0)), Some(dev(1))),
            (
                id("B").as_str().to_owned(),
                Some(bounds(1920)),
                Some(dev(2)),
            ),
        ]);
        assert_eq!(map.bounds_for(&id("A")), Some(bounds(0)));
        assert_eq!(map.bounds_for(&id("B")), Some(bounds(1920)));
        assert_eq!(map.device_for(&id("A")), Some(dev(1)));
        assert_eq!(map.device_for(&id("B")), Some(dev(2)));
    }

    #[test]
    fn unknown_id_yields_none() {
        let map = BoundsMap::new(vec![(
            id("A").as_str().to_owned(),
            Some(bounds(0)),
            Some(dev(1)),
        )]);
        assert_eq!(map.bounds_for(&id("Z")), None);
        assert_eq!(map.device_for(&id("Z")), None);
    }

    #[test]
    fn panel_entry_reports_no_bounds_or_device() {
        let map = BoundsMap::new(vec![(id("A").as_str().to_owned(), None, None)]);
        assert_eq!(map.bounds_for(&id("A")), None);
        assert_eq!(map.device_for(&id("A")), None);
    }

    #[test]
    fn twin_slots_map_to_the_nth_bare_match() {
        // Two serial-less twins share a bare id; the manager resolves them to
        // <bare>-slot0 / -slot1 in enumeration order. Each slot must pick the
        // Nth entry's bounds and device.
        let bare = StableDisplayId::from_parts("GSM", 0x5B09, None).unwrap();
        let map = BoundsMap::new(vec![
            (bare.as_str().to_owned(), Some(bounds(0)), Some(dev(1))),
            (bare.as_str().to_owned(), Some(bounds(1920)), Some(dev(2))),
        ]);
        assert_eq!(map.bounds_for(&bare.with_slot(0)), Some(bounds(0)));
        assert_eq!(map.bounds_for(&bare.with_slot(1)), Some(bounds(1920)));
        assert_eq!(map.device_for(&bare.with_slot(0)), Some(dev(1)));
        assert_eq!(map.device_for(&bare.with_slot(1)), Some(dev(2)));
        // A slot beyond the twins resolves to nothing.
        assert_eq!(map.bounds_for(&bare.with_slot(2)), None);
        assert_eq!(map.device_for(&bare.with_slot(2)), None);
    }

    #[test]
    fn empty_map_is_all_none() {
        let map = BoundsMap::default();
        assert_eq!(map.bounds_for(&id("A")), None);
        assert_eq!(map.device_for(&id("A")), None);
    }
}

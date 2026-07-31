//! An app-side map from a **resolved** display id to its display geometry: its
//! bounds and the two platform tokens the dimming channels address it by.
//!
//! Every value here is platform-specific and this map is deliberately blind to
//! which platform it holds; the contract lives on
//! [`DisplayGeom`] and **must** be read
//! before using any of them on a new platform. In short: bounds are physical
//! pixels on Windows and points on macOS, and there are **two** tokens because
//! they answer two different questions —
//!
//! - [`BoundsMap::gamma_token_for`] **addresses** one display, for the gamma sink;
//! - [`BoundsMap::surface_token_for`] names its **framebuffer**, for
//!   [`clone_group::group_clones`](crate::bin_support::clone_group::group_clones),
//!   which buckets members on the exact string — that equality is how a mirrored
//!   set is detected and collapsed into one control (`#66`).
//!
//! On Windows both are the same GDI device name, because there one device *is* the
//! clone set. On macOS they diverge for a mirror clone, and the surface token can
//! name a display Duja never enumerated, so the two are **not** interchangeable —
//! which is the whole reason they are separate accessors rather than one
//! `device_for`.
//!
//! `duja-core`'s `DiscoveredDisplay` is frozen and carries no bounds, so the app
//! keeps them here, refreshed on every enumeration. Entries are stored in the
//! exact deterministic order the backend reports them (DDC first, then panels). A
//! lookup for a resolved id reuses the same [`select_slot_match`] routing the
//! controller factory uses, so an identical-twin `-slot<n>` id resolves to the
//! Nth bare-id match — the same slot the manager assigned, because both walk the
//! same input order. Every display contributes whatever its backend could report:
//! a DDC display (including a Windows DDC-fallback internal panel) its DDC
//! geometry, a macOS `DisplayServices` panel its own CoreGraphics geometry, and a
//! Windows WMI panel nothing at all — the one shape left with no bounds and no
//! tokens, because WMI reports neither.

// RATIONALE: these pure modules are consumed only by the tray assembly (Windows and macOS),
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use duja_core::dimmer::DisplayBounds;
use duja_core::id::{StableDisplayId, select_slot_match};

use crate::bin_support::backend::DisplayGeom;

/// Resolved-id → display geometry, backed by the ordered enumeration.
#[derive(Debug, Clone, Default)]
pub(crate) struct BoundsMap {
    entries: Vec<DisplayGeom>,
}

impl BoundsMap {
    /// Build from the ordered enumeration (bare ids, in backend order).
    pub(crate) fn new(entries: Vec<DisplayGeom>) -> Self {
        BoundsMap { entries }
    }

    /// The index of the entry a resolved id routes to (twin-slot aware).
    fn index_of(&self, resolved: &StableDisplayId) -> Option<usize> {
        let candidates: Vec<&str> = self.entries.iter().map(|e| e.id.as_str()).collect();
        select_slot_match(resolved.as_str(), &candidates)
    }

    /// The bounds for a resolved display id, or `None` if unknown / panel.
    pub(crate) fn bounds_for(&self, resolved: &StableDisplayId) -> Option<DisplayBounds> {
        let idx = self.index_of(resolved)?;
        self.entries.get(idx).and_then(|e| e.bounds)
    }

    /// The token that **addresses** this display for gamma — the GDI device name
    /// (e.g. `\\.\DISPLAY1`) on Windows, this display's own `CGDirectDisplayID`
    /// in decimal on macOS — or `None` if unknown / panel.
    ///
    /// Not interchangeable with [`Self::surface_token_for`]: on macOS a mirror
    /// clone's surface token is the *master's* id, which may not even be a
    /// display Duja enumerated, so driving gamma through it would dim the wrong
    /// screen.
    pub(crate) fn gamma_token_for(&self, resolved: &StableDisplayId) -> Option<String> {
        let idx = self.index_of(resolved)?;
        self.entries.get(idx).and_then(|e| e.gamma_token.clone())
    }

    /// The token that names this display's **framebuffer** — what mirrored panels
    /// are grouped by — or `None` if unknown / panel (which then stays its own
    /// singleton).
    ///
    /// A key, compared but never dereferenced. See [`Self::gamma_token_for`] for
    /// the half of the old single token that must not be taken from here.
    pub(crate) fn surface_token_for(&self, resolved: &StableDisplayId) -> Option<String> {
        let idx = self.index_of(resolved)?;
        self.entries.get(idx).and_then(|e| e.surface_token.clone())
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

    /// A Windows-shaped entry: one GDI device name in both token slots, which is
    /// what `discover_ddc` stamps there.
    fn win_entry(id: &StableDisplayId, bounds: Option<DisplayBounds>, n: u32) -> DisplayGeom {
        DisplayGeom {
            id: id.as_str().to_owned(),
            bounds,
            gamma_token: Some(dev(n)),
            surface_token: Some(dev(n)),
        }
    }

    /// A Windows/WMI panel entry: no bounds, no tokens, because WMI reports none.
    fn wmi_panel_entry(id: &StableDisplayId) -> DisplayGeom {
        DisplayGeom {
            id: id.as_str().to_owned(),
            bounds: None,
            gamma_token: None,
            surface_token: None,
        }
    }

    #[test]
    fn resolves_a_plain_id() {
        let map = BoundsMap::new(vec![
            win_entry(&id("A"), Some(bounds(0)), 1),
            win_entry(&id("B"), Some(bounds(1920)), 2),
        ]);
        assert_eq!(map.bounds_for(&id("A")), Some(bounds(0)));
        assert_eq!(map.bounds_for(&id("B")), Some(bounds(1920)));
        assert_eq!(map.gamma_token_for(&id("A")), Some(dev(1)));
        assert_eq!(map.gamma_token_for(&id("B")), Some(dev(2)));
        assert_eq!(map.surface_token_for(&id("A")), Some(dev(1)));
        assert_eq!(map.surface_token_for(&id("B")), Some(dev(2)));
    }

    /// The two tokens are read from **separate** fields, so a macOS-shaped entry —
    /// where a mirror clone's surface names the master and its gamma target is
    /// itself — hands each caller its own value.
    ///
    /// This is the one that bites if the accessors are ever collapsed back into a
    /// single `device_for`: on macOS that would drive gamma at display `4` while
    /// the user dragged display `9`'s slider.
    #[test]
    fn a_mirror_clone_addresses_itself_but_groups_by_its_master() {
        let clone = DisplayGeom {
            id: id("A").as_str().to_owned(),
            bounds: Some(bounds(0)),
            // Its own CGDirectDisplayID.
            gamma_token: Some("9".to_owned()),
            // The mirror-set master's.
            surface_token: Some("4".to_owned()),
        };
        let map = BoundsMap::new(vec![clone]);
        assert_eq!(map.gamma_token_for(&id("A")).as_deref(), Some("9"));
        assert_eq!(map.surface_token_for(&id("A")).as_deref(), Some("4"));
    }

    #[test]
    fn unknown_id_yields_none() {
        let map = BoundsMap::new(vec![win_entry(&id("A"), Some(bounds(0)), 1)]);
        assert_eq!(map.bounds_for(&id("Z")), None);
        assert_eq!(map.gamma_token_for(&id("Z")), None);
        assert_eq!(map.surface_token_for(&id("Z")), None);
    }

    #[test]
    fn a_wmi_panel_entry_reports_no_bounds_or_tokens() {
        let map = BoundsMap::new(vec![wmi_panel_entry(&id("A"))]);
        assert_eq!(map.bounds_for(&id("A")), None);
        assert_eq!(map.gamma_token_for(&id("A")), None);
        assert_eq!(map.surface_token_for(&id("A")), None);
    }

    /// A macOS `DisplayServices` panel is *not* the geometry-less shape above: it
    /// reports its rect and both tokens like any CoreGraphics display, which is
    /// what makes it software-dimmable. Routed through the same accessors as a
    /// monitor — there is no panel-specific path, and there must not be one.
    #[test]
    fn a_macos_panel_entry_reports_its_bounds_and_both_tokens() {
        let panel = DisplayGeom {
            id: id("A").as_str().to_owned(),
            bounds: Some(DisplayBounds::new(0, 0, 1512, 982)),
            gamma_token: Some("1".to_owned()),
            surface_token: Some("1".to_owned()),
        };
        let map = BoundsMap::new(vec![panel]);
        assert_eq!(
            map.bounds_for(&id("A")),
            Some(DisplayBounds::new(0, 0, 1512, 982))
        );
        assert_eq!(map.gamma_token_for(&id("A")).as_deref(), Some("1"));
        assert_eq!(map.surface_token_for(&id("A")).as_deref(), Some("1"));
    }

    #[test]
    fn twin_slots_map_to_the_nth_bare_match() {
        // Two serial-less twins share a bare id; the manager resolves them to
        // <bare>-slot0 / -slot1 in enumeration order. Each slot must pick the
        // Nth entry's bounds and tokens.
        let bare = StableDisplayId::from_parts("GSM", 0x5B09, None).unwrap();
        let map = BoundsMap::new(vec![
            win_entry(&bare, Some(bounds(0)), 1),
            win_entry(&bare, Some(bounds(1920)), 2),
        ]);
        assert_eq!(map.bounds_for(&bare.with_slot(0)), Some(bounds(0)));
        assert_eq!(map.bounds_for(&bare.with_slot(1)), Some(bounds(1920)));
        assert_eq!(map.gamma_token_for(&bare.with_slot(0)), Some(dev(1)));
        assert_eq!(map.gamma_token_for(&bare.with_slot(1)), Some(dev(2)));
        assert_eq!(map.surface_token_for(&bare.with_slot(0)), Some(dev(1)));
        assert_eq!(map.surface_token_for(&bare.with_slot(1)), Some(dev(2)));
        // A slot beyond the twins resolves to nothing.
        assert_eq!(map.bounds_for(&bare.with_slot(2)), None);
        assert_eq!(map.gamma_token_for(&bare.with_slot(2)), None);
        assert_eq!(map.surface_token_for(&bare.with_slot(2)), None);
    }

    #[test]
    fn empty_map_is_all_none() {
        let map = BoundsMap::default();
        assert_eq!(map.bounds_for(&id("A")), None);
        assert_eq!(map.gamma_token_for(&id("A")), None);
        assert_eq!(map.surface_token_for(&id("A")), None);
    }
}

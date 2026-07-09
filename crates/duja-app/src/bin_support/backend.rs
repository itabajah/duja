//! Real hardware enumeration and controller opening for the `duja` binary.
//!
//! # Backend → [`DiscoveredDisplay`] mapping
//!
//! `duja-ddc` external monitors map to [`DisplayKind::ExternalDdc`] and
//! `duja-panel` internal panels to [`DisplayKind::InternalPanel`]. Names come
//! straight from each backend.
//!
//! **Capabilities are set statically at enumeration** — brightness-only, with
//! `hardware_range: true` — rather than probed here. This is the minimal correct
//! thing for P3: brightness is the one feature Duja controls uniformly, and it
//! matches exactly what [`duja_panel::PanelController::probe`] reports. The
//! engine's initial `Get` then calibrates the true brightness *maximum* per
//! display. A full DDC/CI capability probe (contrast / input-source discovery)
//! is deliberately deferred so enumeration stays a cheap metadata pass and the
//! stress harness's write/read accounting is not polluted by probe traffic.
//!
//! # Handle hygiene
//!
//! `duja_ddc::enumerate()` hands back live physical-monitor handles inside each
//! `DdcDisplay`. [`discover`] takes only the metadata and drops each display
//! immediately (releasing its handle); [`open_controller`] converts exactly the
//! one matched display into a controller and drops the rest.

use duja_core::controller::BrightnessController;
use duja_core::id::StableDisplayId;
use duja_core::manager::DiscoveredDisplay;
use duja_core::model::{Capabilities, DisplayKind, Feature};

/// Capabilities advertised for a hardware-backed display at enumeration time:
/// brightness only, with a real hardware range. See the module docs.
fn hardware_brightness_caps() -> Capabilities {
    Capabilities {
        features: [Feature::Brightness].into_iter().collect(),
        hardware_range: true,
        raw_capabilities: None,
    }
}

/// Enumerate every controllable display (external DDC first, then panels) as
/// plain [`DiscoveredDisplay`] metadata. Never errors: a failing backend
/// contributes nothing (matching the "graceful absence" contract).
pub(crate) fn discover() -> Vec<DiscoveredDisplay> {
    let mut out = discover_ddc();
    out.extend(discover_panel());
    out
}

#[cfg(windows)]
fn discover_ddc() -> Vec<DiscoveredDisplay> {
    // Each `DdcDisplay` is dropped at the end of the map closure, releasing its
    // physical-monitor handle promptly — we keep only the metadata.
    match duja_ddc::enumerate() {
        Ok(displays) => displays
            .into_iter()
            .map(|d| DiscoveredDisplay {
                id: d.id.clone(),
                kind: DisplayKind::ExternalDdc,
                name: d.name.clone(),
                capabilities: hardware_brightness_caps(),
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(windows))]
fn discover_ddc() -> Vec<DiscoveredDisplay> {
    Vec::new()
}

fn discover_panel() -> Vec<DiscoveredDisplay> {
    match duja_panel::enumerate() {
        Ok(panels) => panels
            .into_iter()
            .map(|p| DiscoveredDisplay {
                id: p.id().clone(),
                kind: DisplayKind::InternalPanel,
                name: Some(p.name().to_owned()),
                capabilities: hardware_brightness_caps(),
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Re-enumerate and open a fresh [`BrightnessController`] for `id`, or `None`
/// if the display is not currently present or cannot be opened.
///
/// This is the shape the engine's `ControllerFactory` needs: it re-enumerates
/// on every call so a hot-plugged display always gets a freshly-opened handle.
pub(crate) fn open_controller(id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    open_ddc(id).or_else(|| open_panel(id))
}

#[cfg(windows)]
fn open_ddc(id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    // `find` consumes non-matching displays (releasing their handles) and stops
    // at the first match; the remaining iterator is dropped, releasing the rest.
    let matched = duja_ddc::enumerate()
        .ok()?
        .into_iter()
        .find(|d| id_matches(id, &d.id))?;
    Some(Box::new(matched.into_controller()))
}

#[cfg(not(windows))]
fn open_ddc(_id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    None
}

fn open_panel(id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    let matched = duja_panel::enumerate()
        .ok()?
        .into_iter()
        .find(|p| id_matches(id, p.id()))?;
    open_panel_controller(&matched)
}

#[cfg(windows)]
fn open_panel_controller(
    panel: &duja_panel::PanelDisplay,
) -> Option<Box<dyn BrightnessController>> {
    panel
        .open()
        .ok()
        .map(|c| Box::new(c) as Box<dyn BrightnessController>)
}

#[cfg(not(windows))]
fn open_panel_controller(
    _panel: &duja_panel::PanelDisplay,
) -> Option<Box<dyn BrightnessController>> {
    None
}

/// Whether `candidate` (a pre-slotting enumerated id) is the display the engine
/// asked for by its resolved `requested` id.
///
/// The engine resolves identical-twin monitors to `-slot<n>`-suffixed ids while
/// enumeration reports the bare EDID id, so an exact match or a `"<id>-slot…"`
/// prefix both count. (For twins this returns the first physical match, which
/// the P3 harness accepts.)
fn id_matches(requested: &StableDisplayId, candidate: &StableDisplayId) -> bool {
    let (r, c) = (requested.as_str(), candidate.as_str());
    r == c
        || r.strip_prefix(c)
            .is_some_and(|rest| rest.starts_with("-slot"))
}

#[cfg(test)]
mod tests {
    use super::{hardware_brightness_caps, id_matches};
    use duja_core::id::StableDisplayId;
    use duja_core::model::Feature;

    fn id(s: &str) -> StableDisplayId {
        // from_parts needs a 3-letter uppercase manufacturer; craft ids by hand
        // through a real constructor so they are well-formed.
        StableDisplayId::from_parts("GSM", 0x5B09, Some(s)).unwrap()
    }

    #[test]
    fn caps_are_brightness_only_hardware_backed() {
        let caps = hardware_brightness_caps();
        assert!(caps.supports(Feature::Brightness));
        assert!(!caps.supports(Feature::Contrast));
        assert!(caps.hardware_range);
        assert_eq!(caps.raw_capabilities, None);
    }

    #[test]
    fn exact_id_matches() {
        let a = id("SERIAL1");
        assert!(id_matches(&a, &a.clone()));
    }

    #[test]
    fn slot_suffixed_request_matches_bare_candidate() {
        let bare = id("TWIN");
        let slotted = bare.with_slot(0);
        assert!(id_matches(&slotted, &bare));
    }

    #[test]
    fn different_ids_do_not_match() {
        assert!(!id_matches(&id("A"), &id("B")));
        // A bare candidate must not match a different id that merely shares a
        // prefix without the `-slot` boundary.
        let short = id("PANEL");
        let longer = id("PANELX");
        assert!(!id_matches(&longer, &short));
    }
}

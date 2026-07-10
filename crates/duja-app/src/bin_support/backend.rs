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
//!
//! # Identical-twin routing
//!
//! [`discover`] reports the **bare** EDID id for each display; the engine's
//! [`DisplayManager`](duja_core::manager::DisplayManager) then resolves
//! serial-less twins to `<bare>-slot<n>` ids, slot = position in enumeration
//! order. When the engine later asks the factory to open one of those ids,
//! [`open_controller`] re-enumerates and, via
//! [`select_slot_match`](duja_core::id::select_slot_match), selects the **Nth**
//! bare-id match for a `-slot<n>` request. This is correct only because both
//! sides walk the *same* deterministic order: `duja_ddc::enumerate()` sorts by
//! device-interface path and `duja_panel::enumerate()` by WMI instance, and
//! [`assign_twin_slots`](duja_core::manager::assign_twin_slots) slots in that
//! same input order — so slot `n` and "the Nth bare match" always coincide.

use duja_core::controller::BrightnessController;
use duja_core::dimmer::DisplayBounds;
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
    discover_all().0
}

/// One display's app-side geometry: its bare id, pixel bounds (external
/// displays only), and GDI device name (the gamma channel's ramp target — also
/// external displays only).
pub(crate) type DisplayGeom = (String, Option<DisplayBounds>, Option<String>);

/// Enumerate displays **and** their pixel bounds + GDI device names in one pass.
///
/// Returns the [`DiscoveredDisplay`] list the engine consumes, plus a parallel
/// [`DisplayGeom`] list in the *same* deterministic order (DDC first, then
/// panels). The geometry list feeds an app-side
/// [`BoundsMap`](crate::bin_support::bounds::BoundsMap); panels contribute
/// `None` bounds and `None` device (no monitor rect or GDI adapter is plumbed
/// for them in P4). Never errors.
pub(crate) fn discover_all() -> (Vec<DiscoveredDisplay>, Vec<DisplayGeom>) {
    let mut displays = Vec::new();
    let mut geom = Vec::new();

    for (display, display_bounds, gdi_device) in discover_ddc() {
        geom.push((
            display.id.as_str().to_owned(),
            Some(display_bounds),
            Some(gdi_device),
        ));
        displays.push(display);
    }
    for display in discover_panel() {
        geom.push((display.id.as_str().to_owned(), None, None));
        displays.push(display);
    }

    (displays, geom)
}

#[cfg(windows)]
fn discover_ddc() -> Vec<(DiscoveredDisplay, DisplayBounds, String)> {
    // Each `DdcDisplay` is dropped at the end of the map closure, releasing its
    // physical-monitor handle promptly — we keep only the metadata, bounds, and
    // GDI device name.
    match duja_ddc::enumerate() {
        Ok(displays) => displays
            .into_iter()
            .map(|d| {
                let display = DiscoveredDisplay {
                    id: d.id.clone(),
                    kind: DisplayKind::ExternalDdc,
                    name: d.name.clone(),
                    capabilities: hardware_brightness_caps(),
                };
                (display, d.bounds, d.gdi_device)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(windows))]
fn discover_ddc() -> Vec<(DiscoveredDisplay, DisplayBounds, String)> {
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
    let displays = duja_ddc::enumerate().ok()?;
    let candidates: Vec<&str> = displays.iter().map(|d| d.id.as_str()).collect();
    let idx = duja_core::id::select_slot_match(id.as_str(), &candidates)?;
    // `nth(idx)` consumes and drops the earlier displays (releasing their
    // handles); the remaining iterator is dropped after, releasing the rest.
    let matched = displays.into_iter().nth(idx)?;
    Some(Box::new(matched.into_controller()))
}

#[cfg(not(windows))]
fn open_ddc(_id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    None
}

fn open_panel(id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    let panels = duja_panel::enumerate().ok()?;
    let candidates: Vec<&str> = panels.iter().map(|p| p.id().as_str()).collect();
    let idx = duja_core::id::select_slot_match(id.as_str(), &candidates)?;
    let matched = panels.into_iter().nth(idx)?;
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

#[cfg(test)]
mod tests {
    use super::hardware_brightness_caps;
    use duja_core::model::Feature;

    #[test]
    fn caps_are_brightness_only_hardware_backed() {
        let caps = hardware_brightness_caps();
        assert!(caps.supports(Feature::Brightness));
        assert!(!caps.supports(Feature::Contrast));
        assert!(caps.hardware_range);
        assert_eq!(caps.raw_capabilities, None);
    }
}

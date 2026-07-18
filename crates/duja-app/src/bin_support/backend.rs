//! Real hardware enumeration and controller opening for the `duja` binary.
//!
//! # Backend → [`DiscoveredDisplay`] mapping
//!
//! `duja-ddc` external monitors map to [`DisplayKind::ExternalDdc`]; internal
//! panels map to [`DisplayKind::InternalPanel`], whether they come from
//! `duja-panel` (WMI) or, as a fallback when WMI cannot see the built-in panel,
//! from `duja-ddc` (a DDC display flagged internal). Names come straight from
//! each backend.
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
        allowed_inputs: Vec::new(),
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
    let ddc: Vec<(DiscoveredDisplay, DisplayGeom)> = discover_ddc()
        .into_iter()
        .map(|(display, display_bounds, gdi_device)| {
            let geom = (
                display.id.as_str().to_owned(),
                Some(display_bounds),
                Some(gdi_device),
            );
            (display, geom)
        })
        .collect();
    let panel: Vec<(DiscoveredDisplay, DisplayGeom)> = discover_panel()
        .into_iter()
        .map(|display| {
            let geom = (display.id.as_str().to_owned(), None, None);
            (display, geom)
        })
        .collect();

    merge_displays(ddc, panel).into_iter().unzip()
}

/// Merge the DDC and panel display lists into the tray's display set, applying
/// the internal-panel fallback policy. Kept DDC entries retain their enumeration
/// order and precede the panels; the WMI panels always follow.
///
/// Truth table, per DDC entry (the WMI panels are always kept):
/// - **External DDC display** — always kept; an external monitor is never in the
///   WMI list, so nothing supersedes it.
/// - **Internal DDC display, WMI returned ≥ 1 panel** — dropped. WMI is
///   authoritative for an internal panel it can control, so its
///   [`DisplayKind::InternalPanel`] entry wins and the DDC duplicate is removed.
///   The signal is "WMI listed *any* panel", not an id match: a serial-less panel
///   derives DIFFERENT ids from the two backends (`from_edid` hashes the whole
///   128-byte EDID; WMI's `from_parts` hashes only `"MFG-PROD"`), so id-matching
///   alone could never dedup it — see
///   `merge_drops_internal_ddc_duplicate_when_wmi_has_the_panel_serial_less`.
/// - **Internal DDC display, WMI returned 0 panels** — KEPT, as the
///   [`DisplayKind::InternalPanel`] fallback. This is the fix: on a laptop whose
///   backlight is GPU/OEM-driven, WMI cannot see the panel and the DDC path is
///   its only carrier, so dropping it here would leave the built-in screen in
///   neither list, vanished (see `internal_panel_survives_when_wmi_is_empty`).
fn merge_displays(
    ddc: Vec<(DiscoveredDisplay, DisplayGeom)>,
    panel: Vec<(DiscoveredDisplay, DisplayGeom)>,
) -> Vec<(DiscoveredDisplay, DisplayGeom)> {
    // WMI is authoritative for any internal panel it can see, so an internal DDC
    // fallback survives only when WMI listed no panel at all. External DDC entries
    // are always kept (an external is never in the WMI list). The dedup signal is
    // "WMI listed any panel", NOT an id match, because a serial-less panel derives
    // divergent ids across the two backends — see the truth table above.
    let wmi_has_panel = !panel.is_empty();
    let mut out: Vec<(DiscoveredDisplay, DisplayGeom)> = ddc
        .into_iter()
        .filter(|(display, _)| display.kind != DisplayKind::InternalPanel || !wmi_has_panel)
        .collect();
    out.extend(panel);
    out
}

#[cfg(windows)]
fn discover_ddc() -> Vec<(DiscoveredDisplay, DisplayBounds, String)> {
    // Each `DdcDisplay` is dropped at the end of the map closure, releasing its
    // physical-monitor handle promptly — we keep only the metadata, bounds, and
    // GDI device name. A display the DDC backend flags `is_internal` is a laptop
    // panel surfaced as the fallback carrier, so it is classified InternalPanel
    // (not ExternalDdc); the merge then keeps it only when WMI lists no panel.
    match duja_ddc::enumerate() {
        Ok(displays) => displays
            .into_iter()
            .map(|d| {
                let kind = if d.is_internal {
                    DisplayKind::InternalPanel
                } else {
                    DisplayKind::ExternalDdc
                };
                let display = DiscoveredDisplay {
                    id: d.id.clone(),
                    kind,
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
///
/// **WMI is tried before DDC.** A WMI-controllable internal panel must be driven
/// by native WMI backlight, not DDC-over-eDP; and now that `duja_ddc::enumerate`
/// also surfaces internal panels, a DDC-first order could wrongly open a DDC
/// handle for a WMI-owned panel. An external monitor is never in the WMI list
/// (WMI lists only `WmiMonitorBrightness` internal panels), so `open_panel`
/// returns `None` for it and it falls through to `open_ddc`. A fallback internal
/// panel that WMI cannot see likewise falls through to `open_ddc`, which
/// re-matches it by id; the engine's verify-first write then routes it to real
/// hardware (if DDC-over-eDP answers) or a `SoftwareOnly` overlay (if not).
pub(crate) fn open_controller(id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    open_panel(id).or_else(|| open_ddc(id))
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
    use super::{DisplayGeom, hardware_brightness_caps, merge_displays};
    use duja_core::dimmer::DisplayBounds;
    use duja_core::id::StableDisplayId;
    use duja_core::manager::DiscoveredDisplay;
    use duja_core::model::{DisplayKind, Feature};

    #[test]
    fn caps_are_brightness_only_hardware_backed() {
        let caps = hardware_brightness_caps();
        assert!(caps.supports(Feature::Brightness));
        assert!(!caps.supports(Feature::Contrast));
        assert!(caps.hardware_range);
        assert_eq!(caps.raw_capabilities, None);
    }

    /// A DDC-backed entry (external) for `id`, with dummy geometry.
    fn ddc_entry(id: &StableDisplayId, name: &str) -> (DiscoveredDisplay, DisplayGeom) {
        let display = DiscoveredDisplay {
            id: id.clone(),
            kind: DisplayKind::ExternalDdc,
            name: Some(name.to_owned()),
            capabilities: hardware_brightness_caps(),
        };
        let geom = (
            id.as_str().to_owned(),
            Some(DisplayBounds::new(0, 0, 100, 100)),
            Some(r"\\.\display1".to_owned()),
        );
        (display, geom)
    }

    /// A DDC-backed entry for an INTERNAL panel surfaced by the fallback — kind
    /// `InternalPanel`, exactly as `discover_ddc` now labels a `DdcDisplay` whose
    /// `is_internal` flag is set — carrying the external-style geometry the DDC
    /// backend still provides for it.
    fn ddc_internal_entry(id: &StableDisplayId, name: &str) -> (DiscoveredDisplay, DisplayGeom) {
        let display = DiscoveredDisplay {
            id: id.clone(),
            kind: DisplayKind::InternalPanel,
            name: Some(name.to_owned()),
            capabilities: hardware_brightness_caps(),
        };
        let geom = (
            id.as_str().to_owned(),
            Some(DisplayBounds::new(0, 0, 100, 100)),
            Some(r"\\.\display1".to_owned()),
        );
        (display, geom)
    }

    /// A WMI panel entry (internal) for `id`, with no geometry (matches how the
    /// panel backend contributes `None` bounds/device).
    fn panel_entry(id: &StableDisplayId, name: &str) -> (DiscoveredDisplay, DisplayGeom) {
        let display = DiscoveredDisplay {
            id: id.clone(),
            kind: DisplayKind::InternalPanel,
            name: Some(name.to_owned()),
            capabilities: hardware_brightness_caps(),
        };
        (display, (id.as_str().to_owned(), None, None))
    }

    #[test]
    fn merge_drops_internal_ddc_duplicate_when_wmi_has_the_panel_serial_bearing() {
        // A serial-BEARING built-in panel: both backends derive the SAME id
        // (from_edid's serial-string path and WMI's from_parts agree). The DDC
        // backend now surfaces it as the internal fallback (kind InternalPanel),
        // and WMI also lists it. Policy: an internal DDC entry is dropped whenever
        // WMI returned any panel — WMI is authoritative for a panel it can control
        // — so the id survives exactly once, as the WMI InternalPanel. Plus one
        // genuine external monitor, present only in the DDC list, which always
        // survives.
        let shared = StableDisplayId::from_parts("GSM", 0x5B09, Some("PANEL1")).unwrap();
        let external = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();

        let ddc = vec![
            ddc_internal_entry(&shared, "internal-as-ddc"),
            ddc_entry(&external, "real external"),
        ];
        let panel = vec![panel_entry(&shared, "Built-in")];

        let out = merge_displays(ddc, panel);

        // The shared id survives exactly once, as the InternalPanel (WMI) entry.
        let shared_hits: Vec<&DiscoveredDisplay> = out
            .iter()
            .map(|(display, _)| display)
            .filter(|display| display.id == shared)
            .collect();
        assert_eq!(
            shared_hits.len(),
            1,
            "internal panel must not be duplicated"
        );
        assert_eq!(
            shared_hits.first().map(|display| display.kind),
            Some(DisplayKind::InternalPanel),
            "the surviving entry must be the WMI InternalPanel, not the DDC one"
        );
        // The genuine external monitor is untouched.
        assert!(
            out.iter()
                .any(|(display, _)| display.id == external
                    && display.kind == DisplayKind::ExternalDdc)
        );
        assert_eq!(out.len(), 2);
    }

    /// A checksum-valid 128-byte EDID for `mfg`/`product` with NO serial (zero
    /// numeric serial, no serial-string descriptor), so `from_edid` takes the
    /// content-hash fallback. Built without indexing / raw arithmetic to stay
    /// inside the lint wall.
    fn serial_less_edid(mfg: &str, product: u16) -> Vec<u8> {
        let mut e: Vec<u8> = vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        let mut letters = mfg.bytes();
        let val = |c: u8| u16::from(c).wrapping_sub(64) & 0x1F;
        let v0 = val(letters.next().unwrap_or(b'A'));
        let v1 = val(letters.next().unwrap_or(b'A'));
        let v2 = val(letters.next().unwrap_or(b'A'));
        e.extend_from_slice(&((v0 << 10) | (v1 << 5) | v2).to_be_bytes());
        e.extend_from_slice(&product.to_le_bytes());
        e.extend_from_slice(&0u32.to_le_bytes());
        e.resize(127, 0x00);
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg());
        e
    }

    #[test]
    fn merge_drops_internal_ddc_duplicate_when_wmi_has_the_panel_serial_less() {
        // A serial-LESS built-in panel: the two backends derive DIFFERENT ids —
        // `from_edid` hashes the full 128-byte EDID, WMI's `from_parts` hashes
        // only "MFG-PROD" — so id-matching alone could never dedup them. The new
        // policy does not rely on id-matching: because WMI returned a panel, the
        // internal DDC entry is dropped regardless of the id divergence, leaving
        // exactly the WMI InternalPanel. This is the very duplicate the OLD
        // id-match dedup let through as a second, mislabeled row.
        let edid = serial_less_edid("AUO", 0x1234);
        let ddc_id = StableDisplayId::from_edid(&edid).unwrap();
        let wmi_id = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();
        assert_ne!(
            ddc_id, wmi_id,
            "serial-less DDC and WMI ids must diverge (from_edid vs from_parts hash inputs)"
        );

        let ddc = vec![ddc_internal_entry(&ddc_id, "internal-as-ddc")];
        let panel = vec![panel_entry(&wmi_id, "Built-in")];
        let out = merge_displays(ddc, panel);
        assert_eq!(
            out.len(),
            1,
            "WMI presence dedups the divergent-id internal DDC entry"
        );
        let survivor = out.first().map(|(display, _)| display);
        assert_eq!(survivor.map(|d| d.id.clone()), Some(wmi_id));
        assert_eq!(survivor.map(|d| d.kind), Some(DisplayKind::InternalPanel));
    }

    #[test]
    fn internal_panel_survives_when_wmi_is_empty() {
        // THE bug fix. On the user's laptop the built-in panel's backlight is not
        // ACPI/WMI-driven, so `discover_panel` (WMI) returns nothing. The DDC
        // fallback surfaces the panel (kind InternalPanel); with no WMI panel to
        // supersede it, the merge MUST keep it — otherwise the internal panel
        // appears in neither list and vanishes from the tray (the exact v0.1.2
        // regression this guards). An external monitor from the same DDC pass is
        // kept alongside it.
        let internal = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();
        let external = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();

        let ddc = vec![
            ddc_internal_entry(&internal, "Built-in (DDC fallback)"),
            ddc_entry(&external, "real external"),
        ];
        let panel: Vec<(DiscoveredDisplay, DisplayGeom)> = Vec::new();

        let out = merge_displays(ddc, panel);

        let internal_hit = out
            .iter()
            .find(|(display, _)| display.id == internal)
            .map(|(display, _)| display);
        assert_eq!(
            internal_hit.map(|d| d.kind),
            Some(DisplayKind::InternalPanel),
            "the internal panel must survive as InternalPanel when WMI is empty"
        );
        assert!(
            out.iter()
                .any(|(display, _)| display.id == external
                    && display.kind == DisplayKind::ExternalDdc),
            "the external monitor survives alongside it"
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn external_ddc_display_always_survives_regardless_of_wmi() {
        // An external monitor is never in the WMI panel list, and the merge must
        // never drop it — present it beside a WMI panel (with a different id) and
        // confirm it is kept as ExternalDdc.
        let external = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();
        let panel_id = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();

        let ddc = vec![ddc_entry(&external, "real external")];
        let panel = vec![panel_entry(&panel_id, "Built-in")];

        let out = merge_displays(ddc, panel);
        assert!(
            out.iter()
                .any(|(display, _)| display.id == external
                    && display.kind == DisplayKind::ExternalDdc)
        );
        assert_eq!(out.len(), 2);
    }
}

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

    dedup_displays(ddc, panel).into_iter().unzip()
}

/// Concatenate the DDC and panel display lists, dropping any DDC entry whose
/// stable id also appears in the panel list. Surviving DDC entries keep their
/// order and precede the panels.
///
/// This is **defense-in-depth** behind the primary fix, and its reach is
/// deliberately limited by identity derivation. The DDC backend already omits
/// internal/embedded panels during correlation (via `outputTechnology`), so in
/// practice no built-in panel reaches this step; dedup only mops up a residual
/// cross-backend duplicate.
///
/// It catches **serial-bearing** duplicates: a panel that exposes a serial
/// derives the *same* [`StableDisplayId`] from either backend (`from_edid`'s
/// serial-string path and WMI's `from_parts` agree), so the duplicate is
/// matched and the authoritative WMI [`DisplayKind::InternalPanel`] entry wins.
/// A **serial-less** panel does *not* converge: `from_edid` hashes the full
/// 128-byte EDID while `from_parts` hashes only `"MFG-PROD"`, so the two ids
/// differ and dedup cannot match them (see
/// `serial_less_internal_panel_ids_diverge_across_backends`). Serial-less
/// internal panels are therefore excluded by the `outputTechnology`
/// classification skip, never by this dedup.
fn dedup_displays(
    ddc: Vec<(DiscoveredDisplay, DisplayGeom)>,
    panel: Vec<(DiscoveredDisplay, DisplayGeom)>,
) -> Vec<(DiscoveredDisplay, DisplayGeom)> {
    let mut out: Vec<(DiscoveredDisplay, DisplayGeom)> = ddc
        .into_iter()
        .filter(|(display, _)| !panel.iter().any(|(p, _)| p.id == display.id))
        .collect();
    out.extend(panel);
    out
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
    use super::{DisplayGeom, dedup_displays, hardware_brightness_caps};
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
    fn dedup_drops_ddc_duplicate_of_internal_panel() {
        // Exercises the dedup LOGIC for the case it actually covers: a
        // serial-BEARING panel, whose id both backends derive identically. We
        // inject one shared id into both lists directly (rather than
        // round-tripping two backends); a serial-bearing panel genuinely
        // converges this way, whereas a serial-less one would NOT — see
        // `serial_less_internal_panel_ids_diverge_across_backends`. Plus one
        // genuine external monitor present only in the DDC list.
        let shared = StableDisplayId::from_parts("GSM", 0x5B09, Some("PANEL1")).unwrap();
        let external = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();

        let ddc = vec![
            ddc_entry(&shared, "internal-as-ddc"),
            ddc_entry(&external, "real external"),
        ];
        let panel = vec![panel_entry(&shared, "Built-in")];

        let out = dedup_displays(ddc, panel);

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
    fn serial_less_internal_panel_ids_diverge_across_backends() {
        // Documents why the `outputTechnology` classification skip — not this
        // dedup — is what removes a serial-less built-in panel. The DDC backend
        // derives identity with `from_edid` (hashing the full 128-byte EDID);
        // the WMI backend uses `from_parts` (hashing only "MFG-PROD"). With no
        // serial to anchor them the two ids differ, so `dedup_displays` cannot
        // match a serial-less panel that ever slipped through as external.
        let edid = serial_less_edid("AUO", 0x1234);
        let ddc_id = StableDisplayId::from_edid(&edid).unwrap();
        let wmi_id = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();
        assert_ne!(
            ddc_id, wmi_id,
            "serial-less DDC and WMI ids must diverge (from_edid vs from_parts hash inputs)"
        );

        // Both entries therefore survive dedup — nothing to match on. This is
        // the residual case the outputTechnology skip is responsible for, not
        // dedup.
        let ddc = vec![ddc_entry(&ddc_id, "internal-as-ddc")];
        let panel = vec![panel_entry(&wmi_id, "Built-in")];
        assert_eq!(dedup_displays(ddc, panel).len(), 2);
    }
}

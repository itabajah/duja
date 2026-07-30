//! Direct in-process backend access for `dujactl` (no engine, no IPC).
//!
//! `dujactl` this phase talks straight to `duja-ddc` and `duja-panel`: it
//! enumerates, opens a controller for a display on demand, and does one paced
//! read/write. Handle hygiene mirrors the app's: [`discover`] keeps only
//! metadata (dropping each backend display immediately — releasing its
//! physical-monitor handle on Windows, its I2C service handle on macOS), and
//! [`open`] converts exactly the matched display.
//!
//! # Backend → [`CtlDisplay`] mapping
//!
//! `duja-panel`'s panels — the OS's native backlight backend, WMI on Windows and
//! the private `DisplayServices` framework on macOS — are always
//! [`DisplayKind::InternalPanel`]. A `duja-ddc` display is
//! [`DisplayKind::ExternalDdc`] unless that backend flags it internal, i.e. a
//! laptop's embedded panel surfaced as the DDC *fallback* carrier, which is an
//! internal panel too. [`map_ddc_display`] is the single place that decides.
//!
//! The two lists are then reconciled by [`merge_displays`] — the panel backend
//! owns any panel it can see — and identical-twin ids are resolved on the
//! **merged** list by [`merge_and_resolve_slots`]. [`open`] tries the panel
//! backend **before** DDC.
//!
//! # Deliberate duplication
//!
//! This mapping is a knowing copy of `duja-app`'s `bin_support::backend`, not a
//! shared crate. `dujactl` is a ~0.8 MB companion binary that deliberately does
//! **not** depend on `duja-app`, and the two mappings are not the same function:
//! the app produces `DiscoveredDisplay` + geometry (bounds and a gamma-target
//! token) for the engine and the dimmer, while this one produces the three fields
//! [`CtlDisplay`] prints. Hoisting them would couple the CLI to the tray app's
//! surface to save a dozen lines. Keep them in step by hand.
//!
//! Since then the copied surface is not only the *shape* but the **policy**: the
//! merge truth table on [`merge_displays`] and [`open`]'s panel-first order are
//! mirrored, rule for rule, from that module — because a CLI that disagrees with
//! the tray about which row is the real built-in panel, or about which transport
//! drives it, is a bug in its own right. Nothing enforces the mirror: there is no
//! shared crate and no test comparing the two. **Change one, change the other.**
//!
//! Both DDC platforms — Windows and macOS — are wired: `duja-ddc` and
//! `duja-panel` expose the same `enumerate`/open surface on each, so one
//! definition serves both and only a target with no backend at all gets a stub.

use duja_core::controller::BrightnessController;
use duja_core::id::StableDisplayId;
use duja_core::model::DisplayKind;

/// One enumerated display, as `dujactl` needs it: identity, kind and name.
#[derive(Debug, Clone)]
pub struct CtlDisplay {
    /// Durable EDID-derived identity.
    pub id: StableDisplayId,
    /// Which backend class controls it.
    pub kind: DisplayKind,
    /// Human-readable name.
    pub name: String,
}

/// Enumerate every controllable display (surviving DDC entries first, then
/// panels), deduplicated and identity-resolved.
///
/// Never errors: a failing backend simply contributes nothing.
///
/// This is only the two hardware enumerations plus
/// [`merge_and_resolve_slots`] — every decision lives in that pure pipeline, so
/// the policy is unit-tested rather than reachable only through hardware.
///
/// Identical-twin monitors that share one EDID id are disambiguated with
/// `-slot<n>` suffixes — the same convention the daemon's
/// [`DisplayManager`](duja_core::manager::DisplayManager) applies — so every
/// row is individually addressable. [`open`] routes those slot ids back to the
/// Nth physical unit (see [`duja_core::id::select_slot_match`]).
pub fn discover() -> Vec<CtlDisplay> {
    merge_and_resolve_slots(discover_ddc(), discover_panel())
}

/// Merge the two backend lists, then resolve identical-twin `-slot<n>` ids — the
/// whole of [`discover`]'s policy, with the hardware calls factored out.
///
/// The order matters, and is the fix for the CLI's worst symptom. Slotting runs on
/// the **merged** list: before the merge existed, a built-in panel that both
/// backends reported with the *same* (serial-bearing) id looked to
/// [`assign_twin_slots`](duja_core::manager::assign_twin_slots) like a pair of
/// identical twins, so both rows were stamped `-slot0` / `-slot1`. `-slot1` then
/// matched **nothing** — [`open`] re-enumerates one backend at a time and each
/// list holds only a single bare match — so `dujactl set <id>-slot1` failed
/// outright, while `-slot0` opened the DDC entry and wrote VCP `0x10` over eDP, a
/// silent no-op on most laptops. Deduplicating first leaves the panel with its
/// bare id, which is exactly the id [`open`] can resolve.
fn merge_and_resolve_slots(ddc: Vec<CtlDisplay>, panel: Vec<CtlDisplay>) -> Vec<CtlDisplay> {
    let mut out = merge_displays(ddc, panel);
    let ids: Vec<StableDisplayId> = out.iter().map(|d| d.id.clone()).collect();
    for (display, resolved) in out
        .iter_mut()
        .zip(duja_core::manager::assign_twin_slots(&ids))
    {
        display.id = resolved;
    }
    out
}

/// Merge the DDC and panel display lists, applying the internal-panel fallback
/// policy. Kept DDC entries retain their enumeration order and precede the
/// panels; the panel-backend entries always follow.
///
/// Mirrored from `duja-app`'s `merge_displays`; see this module's "Deliberate
/// duplication" note. "Panel backend" is `duja-panel`: WMI on Windows,
/// `DisplayServices` on macOS.
///
/// Truth table, per DDC entry (the panel-backend panels are always kept):
/// - **External DDC display** — always kept; an external monitor is never in the
///   panel-backend list, so nothing supersedes it.
/// - **Internal DDC display, panel backend returned ≥ 1 panel** — dropped. The
///   panel backend is authoritative for an internal panel it can control, so its
///   [`DisplayKind::InternalPanel`] entry wins and the DDC duplicate is removed.
///   The signal is "the panel backend listed *any* panel", **not** an id match: on
///   Windows a serial-less panel derives DIFFERENT ids from the two backends
///   (`from_edid` hashes the whole 128-byte EDID, WMI's `from_parts` hashes only
///   `"MFG-PROD"`), so id-matching alone could never dedup it.
/// - **Internal DDC display, panel backend returned 0 panels** — KEPT, as the
///   [`DisplayKind::InternalPanel`] fallback. On a laptop whose backlight is
///   GPU/OEM-driven the native backend cannot see the panel and the DDC path is
///   its only carrier, so dropping it here would make the built-in screen vanish
///   from `list` entirely — the `v0.1.3` regression this rule exists to prevent.
///
/// # On macOS the dedup is inert
///
/// The two backends cannot overlap there: the macOS DDC backend drops built-in
/// panels at enumeration (`CGDisplayIsBuiltin`), so [`map_ddc_display`] is passed
/// `is_internal: false` for every macOS entry and no internal DDC entry ever
/// exists to drop — nor is there any DDC fallback carrier, so a macOS internal
/// panel comes from `DisplayServices` or not at all. The policy below is
/// byte-for-byte the same on both platforms; on macOS it simply never fires, and
/// the truth table's second and third rows describe Windows alone.
fn merge_displays(ddc: Vec<CtlDisplay>, panel: Vec<CtlDisplay>) -> Vec<CtlDisplay> {
    let panel_backend_has_panel = !panel.is_empty();
    let mut out: Vec<CtlDisplay> = ddc
        .into_iter()
        .filter(|d| d.kind != DisplayKind::InternalPanel || !panel_backend_has_panel)
        .collect();
    out.extend(panel);
    out
}

/// How many of `displays` are external DDC monitors — `doctor`'s first count.
///
/// Counted on the **merged** set [`discover`] returns, never on a raw `duja-ddc`
/// enumeration. A Windows built-in panel that the DDC backend also surfaces is
/// classified [`DisplayKind::InternalPanel`], so it is not reported here as an
/// external monitor; it used to be, in the very diagnostic users are asked to
/// attach to monitor-quirk issues.
pub fn external_count(displays: &[CtlDisplay]) -> usize {
    displays
        .iter()
        .filter(|d| d.kind == DisplayKind::ExternalDdc)
        .count()
}

/// How many of `displays` are internal panels — `doctor`'s second count.
///
/// Includes a surviving DDC-fallback internal panel (Windows only): it is reached
/// over DDC, but it *is* the built-in screen, so it belongs to this count rather
/// than the external-monitor one.
pub fn internal_count(displays: &[CtlDisplay]) -> usize {
    displays
        .iter()
        .filter(|d| d.kind == DisplayKind::InternalPanel)
        .count()
}

/// Map one DDC-enumerated display onto a [`CtlDisplay`].
///
/// The single home of the DDC classification rule, shared by both DDC platforms
/// because it is the same rule on both — only the *input* differs, and only as far
/// as the backends' fields force:
///
/// - **Windows** passes `d.is_internal`. `duja_ddc::enumerate` has flagged a
///   laptop's embedded panel since `v0.1.3`, and that flag is the whole
///   difference between "external monitor" and "the built-in screen surfaced as a
///   fallback carrier".
/// - **macOS** passes `false`, because there is nothing else it could pass: that
///   backend filters built-ins out at enumeration with `CGDisplayIsBuiltin` and
///   its `DdcDisplay` carries no such field, so every entry it yields *is* an
///   external monitor.
///
/// `name` is the backend's optional monitor name; `"-"` stands in when it has
/// none, since the `list` table always prints a cell.
// RATIONALE: only the Windows and macOS `discover_ddc` arms call this. On a target
// with no DDC backend at all the stub returns an empty list, so the mapping is
// genuinely unreachable there — but it stays un-`cfg`'d because it is pure policy
// that must exist in exactly one place, and its tests run on every CI lane.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn map_ddc_display(id: StableDisplayId, is_internal: bool, name: Option<String>) -> CtlDisplay {
    CtlDisplay {
        id,
        kind: if is_internal {
            DisplayKind::InternalPanel
        } else {
            DisplayKind::ExternalDdc
        },
        name: name.unwrap_or_else(|| "-".to_owned()),
    }
}

/// Enumerate the DDC backend's displays. Windows arm: the `is_internal` flag
/// decides the kind (see [`map_ddc_display`]). Each `DdcDisplay` is dropped at the
/// end of the closure, releasing its physical-monitor handle promptly.
#[cfg(windows)]
fn discover_ddc() -> Vec<CtlDisplay> {
    match duja_ddc::enumerate() {
        Ok(displays) => displays
            .into_iter()
            .map(|d| map_ddc_display(d.id.clone(), d.is_internal, d.name.clone()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Enumerate the DDC backend's displays. macOS arm: identical but for the
/// hard-coded `is_internal: false`, which is the only divergence the backends'
/// field difference forces (see [`map_ddc_display`]). Each `DdcDisplay` is dropped
/// at the end of the closure, releasing its I2C service handle promptly.
#[cfg(target_os = "macos")]
fn discover_ddc() -> Vec<CtlDisplay> {
    match duja_ddc::enumerate() {
        Ok(displays) => displays
            .into_iter()
            .map(|d| map_ddc_display(d.id.clone(), false, d.name.clone()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// No DDC backend on this target: `duja-ddc` exposes `enumerate` only on Windows
/// and macOS (Linux lands in P7).
#[cfg(not(any(windows, target_os = "macos")))]
fn discover_ddc() -> Vec<CtlDisplay> {
    Vec::new()
}

/// Enumerate the native panel backend's internal panels. Not `cfg`-gated:
/// `duja_panel::enumerate` exists on every target and returns an empty list where
/// there is no backend.
fn discover_panel() -> Vec<CtlDisplay> {
    match duja_panel::enumerate() {
        Ok(panels) => panels
            .into_iter()
            .map(|p| CtlDisplay {
                id: p.id().clone(),
                kind: DisplayKind::InternalPanel,
                name: p.name().to_owned(),
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Open a fresh [`BrightnessController`] for the display whose id string is
/// `id`, or `None` if no present display matches.
///
/// **The panel backend is tried before DDC** — WMI on Windows, `DisplayServices`
/// on macOS — mirroring the app's `open_controller`. A panel the native backlight
/// API can drive must be driven through it, not over DDC-on-eDP: on Windows
/// `duja_ddc::enumerate` also surfaces the built-in panel, so a DDC-first order
/// would open a DDC handle for a panel WMI owns and write VCP `0x10` at an
/// embedded display that mostly ignores it. An external monitor is never in the
/// panel list (WMI lists only `WmiMonitorBrightness` internal panels;
/// `DisplayServices` only built-in ones), so [`open_panel`] returns `None` for it
/// and it falls through to [`open_ddc`]. A fallback internal panel the native
/// backend cannot see likewise falls through and is re-matched by id there. On
/// macOS the two lists cannot overlap at all (see [`merge_displays`]), so the
/// order there just means "built-in panel first, external monitors second".
pub fn open(id: &str) -> Option<Box<dyn BrightnessController>> {
    open_panel(id).or_else(|| open_ddc(id))
}

/// Open the DDC display matching `id`. Shared by both DDC platforms: the
/// `enumerate` → `into_controller` surface and the drop-releases-the-handle
/// discipline are identical on Windows and macOS.
#[cfg(any(windows, target_os = "macos"))]
fn open_ddc(id: &str) -> Option<Box<dyn BrightnessController>> {
    let displays = duja_ddc::enumerate().ok()?;
    let candidates: Vec<&str> = displays.iter().map(|d| d.id.as_str()).collect();
    let idx = duja_core::id::select_slot_match(id, &candidates)?;
    let matched = displays.into_iter().nth(idx)?;
    Some(Box::new(matched.into_controller()))
}

/// No DDC backend on this target, so nothing can be opened (Linux lands in P7).
#[cfg(not(any(windows, target_os = "macos")))]
fn open_ddc(_id: &str) -> Option<Box<dyn BrightnessController>> {
    None
}

/// Open the native panel backend's panel matching `id`, or `None` when it lists
/// no such panel — which is the normal answer for an external monitor.
fn open_panel(id: &str) -> Option<Box<dyn BrightnessController>> {
    let panels = duja_panel::enumerate().ok()?;
    let candidates: Vec<&str> = panels.iter().map(|p| p.id().as_str()).collect();
    let idx = duja_core::id::select_slot_match(id, &candidates)?;
    let matched = panels.into_iter().nth(idx)?;
    open_panel_controller(&matched)
}

/// Open a controller for one enumerated panel. Shared by both panel platforms:
/// `PanelDisplay::open` exists on Windows (WMI) and macOS (`DisplayServices`) with
/// the same signature shape, and `PanelController` is generic over its transport,
/// so both box identically.
#[cfg(any(windows, target_os = "macos"))]
fn open_panel_controller(
    panel: &duja_panel::PanelDisplay,
) -> Option<Box<dyn BrightnessController>> {
    panel
        .open()
        .ok()
        .map(|c| Box::new(c) as Box<dyn BrightnessController>)
}

/// No panel backend on this target: `duja-panel` enumerates nothing there, so
/// this is unreachable in practice (Linux lands in P7).
#[cfg(not(any(windows, target_os = "macos")))]
fn open_panel_controller(
    _panel: &duja_panel::PanelDisplay,
) -> Option<Box<dyn BrightnessController>> {
    None
}

#[cfg(test)]
mod tests {
    use super::{
        CtlDisplay, external_count, internal_count, map_ddc_display, merge_and_resolve_slots,
        merge_displays,
    };
    use crate::fmt::kind_label;
    use duja_core::id::StableDisplayId;
    use duja_core::model::DisplayKind;

    /// A DDC row as the **Windows** backend would hand it over: the `is_internal`
    /// flag goes through the real classification helper, so these tests exercise
    /// the shipping mapping rather than a hand-stamped kind.
    fn ddc(id: &StableDisplayId, is_internal: bool, name: &str) -> CtlDisplay {
        map_ddc_display(id.clone(), is_internal, Some(name.to_owned()))
    }

    /// A native-panel-backend row (WMI / `DisplayServices`), always internal.
    fn panel(id: &StableDisplayId, name: &str) -> CtlDisplay {
        CtlDisplay {
            id: id.clone(),
            kind: DisplayKind::InternalPanel,
            name: name.to_owned(),
        }
    }

    /// A checksum-valid 128-byte EDID for `mfg`/`product` with NO serial (zero
    /// numeric serial, no serial-string descriptor), so `from_edid` takes the
    /// content-hash fallback and diverges from WMI's `from_parts`.
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

    /// Consequence 2 — the one that made `dujactl set` fail outright.
    ///
    /// A serial-bearing built-in panel is reported by BOTH backends with the same
    /// derived id. Un-merged, `assign_twin_slots` saw identical twins and stamped
    /// `-slot0` / `-slot1`; `-slot1` then resolved to nothing in either backend's
    /// list, so the id `list` printed could not be set or read. Merged first, the
    /// panel keeps its bare, resolvable id.
    #[test]
    fn serial_bearing_panel_from_both_backends_keeps_one_bare_addressable_id() {
        let shared = StableDisplayId::from_parts("GSM", 0x5B09, Some("PANEL1")).unwrap();
        let bare = shared.as_str().to_owned();

        let out = merge_and_resolve_slots(
            vec![ddc(&shared, true, "internal-as-ddc")],
            vec![panel(&shared, "Built-in")],
        );

        let ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec![bare.as_str()], "one row, keeping its bare id");
        assert!(
            out.iter().all(|d| !d.id.as_str().contains("-slot")),
            "a deduplicated panel is not an identical twin, so it must not be slotted"
        );
        assert_eq!(
            out.first().map(|d| d.kind),
            Some(DisplayKind::InternalPanel),
            "the survivor is the native panel entry, not the DDC one"
        );
    }

    /// Genuine identical twins must still be slotted — the dedup must not have
    /// swallowed the twin-routing it now runs after.
    #[test]
    fn genuine_identical_external_twins_are_still_slotted() {
        let twin = StableDisplayId::from_parts("DEL", 0xA131, None).unwrap();
        let out = merge_and_resolve_slots(
            vec![ddc(&twin, false, "twin a"), ddc(&twin, false, "twin b")],
            Vec::new(),
        );
        let ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                format!("{}-slot0", twin.as_str()).as_str(),
                format!("{}-slot1", twin.as_str()).as_str(),
            ]
        );
    }

    /// Consequence 1 — the built-in panel was labelled an external monitor.
    /// Asserted through the user-visible `list` / `doctor` label, not just the
    /// enum, since that string is the whole symptom.
    #[test]
    fn ddc_internal_flag_labels_the_built_in_panel_internal() {
        let id = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();

        let internal = ddc(&id, true, "Built-in (DDC fallback)");
        assert_eq!(internal.kind, DisplayKind::InternalPanel);
        assert_eq!(kind_label(internal.kind), "internal");

        let external = ddc(&id, false, "real external");
        assert_eq!(external.kind, DisplayKind::ExternalDdc);
        assert_eq!(kind_label(external.kind), "external");
    }

    /// A DDC display with no recovered monitor name still prints a cell.
    #[test]
    fn ddc_display_without_a_name_falls_back_to_a_dash() {
        let id = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();
        assert_eq!(map_ddc_display(id, false, None).name, "-");
    }

    /// Consequence 3 — a serial-LESS panel derives DIFFERENT ids from the two
    /// backends (`from_edid` hashes the full EDID, `from_parts` hashes only
    /// `"MFG-PROD"`), so id-matching could never dedup it and the CLI listed two
    /// distinct rows, one of them driving DDC-over-eDP. The presence signal does
    /// dedup it.
    #[test]
    fn serial_less_panel_with_divergent_ids_is_still_deduplicated() {
        let edid = serial_less_edid("AUO", 0x1234);
        let ddc_id = StableDisplayId::from_edid(&edid).unwrap();
        let wmi_id = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();
        assert_ne!(
            ddc_id, wmi_id,
            "serial-less DDC and panel-backend ids must diverge for this test to mean anything"
        );

        let out = merge_and_resolve_slots(
            vec![ddc(&ddc_id, true, "internal-as-ddc")],
            vec![panel(&wmi_id, "Built-in")],
        );

        assert_eq!(out.len(), 1, "the divergent-id duplicate is dropped");
        assert_eq!(out.first().map(|d| d.id.clone()), Some(wmi_id));
        assert_eq!(
            out.first().map(|d| d.kind),
            Some(DisplayKind::InternalPanel)
        );
    }

    /// The `v0.1.3` guarantee that must NOT regress: when the native backend lists
    /// no panel at all, the DDC fallback IS the built-in screen, so it stays — and
    /// stays classified internal. Dropping it here would make the laptop's own
    /// display vanish from `dujactl list`.
    #[test]
    fn internal_ddc_panel_survives_when_the_panel_backend_is_empty() {
        let internal = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();
        let external = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();

        let out = merge_displays(
            vec![
                ddc(&internal, true, "Built-in (DDC fallback)"),
                ddc(&external, false, "real external"),
            ],
            Vec::new(),
        );

        assert_eq!(out.len(), 2);
        assert_eq!(
            out.iter().find(|d| d.id == internal).map(|d| d.kind),
            Some(DisplayKind::InternalPanel),
            "the built-in panel must survive, as an internal panel"
        );
        assert!(
            out.iter()
                .any(|d| d.id == external && d.kind == DisplayKind::ExternalDdc),
            "the external monitor survives alongside it"
        );
    }

    /// An external monitor is never in the panel-backend list and must never be
    /// dropped by the dedup.
    #[test]
    fn external_ddc_display_always_survives_regardless_of_the_panel_backend() {
        let external = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();
        let panel_id = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();

        let out = merge_displays(
            vec![ddc(&external, false, "real external")],
            vec![panel(&panel_id, "Built-in")],
        );

        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .any(|d| d.id == external && d.kind == DisplayKind::ExternalDdc)
        );
    }

    /// Consequence 4 — `doctor` counted the built-in panel as an external DDC
    /// monitor, in the report users attach to monitor-quirk issues. The counts are
    /// derived from the merged set, so one laptop panel + one monitor reads as
    /// exactly that.
    #[test]
    fn doctor_counts_come_from_the_merged_set_not_raw_backend_lengths() {
        let shared = StableDisplayId::from_parts("GSM", 0x5B09, Some("PANEL1")).unwrap();
        let external = StableDisplayId::from_parts("DEL", 0xA131, Some("EXT1")).unwrap();

        let out = merge_and_resolve_slots(
            vec![
                ddc(&shared, true, "internal-as-ddc"),
                ddc(&external, false, "real external"),
            ],
            vec![panel(&shared, "Built-in")],
        );

        assert_eq!(external_count(&out), 1, "one external monitor, not two");
        assert_eq!(internal_count(&out), 1, "one built-in panel, listed once");
        assert_eq!(out.len(), 2);
    }

    /// With no native panel, the surviving DDC-fallback panel counts as an
    /// internal panel — it is reached over DDC but it is the built-in screen.
    #[test]
    fn a_ddc_fallback_panel_counts_as_internal_not_external() {
        let internal = StableDisplayId::from_parts("AUO", 0x1234, None).unwrap();
        let out = merge_and_resolve_slots(vec![ddc(&internal, true, "Built-in")], Vec::new());
        assert_eq!(external_count(&out), 0);
        assert_eq!(internal_count(&out), 1);
    }
}

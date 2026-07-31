//! Real hardware enumeration and controller opening for the `duja` binary.
//!
//! # Backend → [`DiscoveredDisplay`] mapping
//!
//! `duja-ddc` external monitors map to [`DisplayKind::ExternalDdc`]; internal
//! panels map to [`DisplayKind::InternalPanel`], whether they come from
//! `duja-panel` — the OS's native panel backend, WMI on Windows and the private
//! `DisplayServices` framework on macOS — or, as a fallback when that backend
//! cannot see the built-in panel, from `duja-ddc` (a DDC display flagged
//! internal). Names come straight from each backend.
//!
//! The DDC internal-panel **fallback is Windows-only**. The macOS DDC backend
//! filters built-in panels out at enumeration (`CGDisplayIsBuiltin`) and its
//! `DdcDisplay` carries no `is_internal` flag at all, so every macOS DDC entry is
//! an external monitor and a macOS internal panel can reach the tray only through
//! `duja-panel`/`DisplayServices` (see `discover_ddc` and `merge_displays`).
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
//! `duja_ddc::enumerate()` hands back a live, owned OS handle inside each
//! `DdcDisplay`: a physical-monitor `HANDLE` on Windows, an I2C service handle
//! (`IOAVService` on Apple Silicon, `IOI2CInterface` on Intel) on macOS. Both
//! release it in `Drop`, so the discipline is identical on either platform:
//! [`discover`] takes only the metadata and drops each display immediately
//! (releasing its handle); [`open_controller`] converts exactly the one matched
//! display into a controller — moving that handle into it — and drops the rest.
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
//! sides walk the *same* deterministic order: each backend's `enumerate()` fixes
//! one — `duja_ddc` sorts by device-interface path on Windows and by
//! `CGDirectDisplayID` on macOS, while `duja_panel` follows WMI instance order on
//! Windows and `CGGetOnlineDisplayList` order on macOS — and
//! [`assign_twin_slots`](duja_core::manager::assign_twin_slots) slots in that
//! same input order, so slot `n` and "the Nth bare match" always coincide. (In
//! practice twin slotting only bites for DDC monitors: a machine has one internal
//! panel outside exotic dual-panel laptops — see `docs/debt.md` — and macOS
//! surfaces at most one built-in display.)

use duja_core::controller::BrightnessController;
use duja_core::dimmer::DisplayBounds;
use duja_core::id::StableDisplayId;
use duja_core::manager::DiscoveredDisplay;
use duja_core::model::{Capabilities, DisplayKind, Feature};
use duja_panel::PanelGeometry;

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

/// One display's app-side geometry: its bare id, its display bounds, and the two
/// platform tokens the dimming channels need.
///
/// Carried by every DDC display — external monitors and a (Windows-only)
/// DDC-fallback internal panel alike — and by a panel from the OS panel backend
/// **when that backend can report it**, which today means macOS: a
/// `DisplayServices` panel is an ordinary CoreGraphics display and answers
/// `CGDisplayBounds`/`CGDisplayMirrorsDisplay` like any other. A Windows WMI panel
/// still contributes all `None`, because WMI exposes neither a monitor rect nor a
/// GDI device for the panel it drives (`docs/debt.md`). The three fields move
/// together — see [`duja_panel::PanelGeometry`], whose absence means "this backend
/// cannot say", never "this display has no position".
///
/// # Bounds, whose **unit differs by platform**
///
/// On **Windows** these are virtual-desktop **physical pixels**
/// (`MONITORINFO::rcMonitor`). On **macOS** they are **points**, not pixels:
/// `CGDisplayBounds` reports a Retina display's logical point size, and the macOS
/// overlay/window layer speaks the same points (see `duja_dimmer`'s `mac_geom`
/// "Units" section). Both are top-left origin, y-down. These bounds feed
/// `bin_support::dimming`'s planner and the overlay `DimCommand`s, which pass them
/// through to the platform dimmer verbatim — correct on both platforms precisely
/// because each side stays in its own unit, so nothing here may assume pixels or
/// mix a bound with a physical-pixel quantity without scaling first.
///
/// # Two tokens, because they answer two different questions
///
/// This used to be one opaque `String` with **two** invariants, because it had two
/// consumers:
///
/// - **(a) Addressable by the platform gamma channel.** The gamma sink resolves a
///   display id to a token and hands it to `duja_dimmer`'s `GammaDisplay`.
/// - **(b) Identical for every panel sharing one framebuffer.** `clone_group`'s
///   `group_clones` buckets members on the exact string (case-folded), so token
///   equality *is* the mirror-detection mechanism: it is what collapses a
///   Duplicate-mode set into one control with one overlay, and what routes the
///   "any member software-only ⇒ pin the hardware members to MAX" rule. See
///   `clone_group`, `#66` and ADR-0018.
///
/// **Windows can hold both in one value; macOS cannot.** A GDI device name *is*
/// the clone set — `MONITORINFOEX::szDevice` is shared by every mirrored panel and
/// one ramp on it covers them all — so both fields carry that same string there.
/// CoreGraphics gives every display its own id and its own transfer table, and the
/// two questions come apart: the surface token of a clone is the *master's* id,
/// and that master need not even be a display Duja enumerated (the built-in panel
/// is filtered out by `CGDisplayIsBuiltin`, and is the master whenever a `MacBook`
/// mirrors its screen to a projector). Addressing gamma through it would dim the
/// laptop screen instead of the monitor whose slider moved.
///
/// So the two are separate fields with separate names, and `BoundsMap` exposes
/// them through separately named accessors. Neither may be shown to a user or
/// parsed as a device path: both are tokens, not names.
///
/// # What is still hardware-blind
///
/// Duja has no Mac, and the CI runners are virtualized with no external display,
/// so no test or run has ever observed a real macOS mirror set. What is *proven*
/// is the surface rule ([`duja_core::macos`]'s tests, which run on every CI OS);
/// what is **assumed** is that the enumeration each backend uses reports every
/// member of a mirror set rather than only the master. That assumption is deliberately
/// not load-bearing: if only the master is enumerated, each surface token equals
/// its own display id, `group_clones` builds the same singletons it builds today,
/// and behaviour is unchanged. It can only *add* a merge, never remove one — and
/// it cannot mis-address anything, because addressing no longer goes through it.
/// See `docs/debt.md`.
#[derive(Debug, Clone)]
pub(crate) struct DisplayGeom {
    /// The display's **bare** EDID id (pre twin-slot resolution), which is what
    /// `BoundsMap` routes a resolved id back to.
    pub(crate) id: String,
    /// Display bounds in this platform's unit (see above), or `None` for a panel
    /// the OS panel backend cannot place (a Windows WMI panel).
    pub(crate) bounds: Option<DisplayBounds>,
    /// The token that **addresses** this display for gamma: the GDI device name on
    /// Windows, this display's own `CGDirectDisplayID` in decimal on macOS —
    /// including for a macOS built-in panel, which is addressed as itself whether
    /// or not it is mirroring. `None` for a Windows WMI panel, which has no gamma
    /// device.
    pub(crate) gamma_token: Option<String>,
    /// The token that names this display's **framebuffer**, which mirrored panels
    /// are grouped by: the GDI device name on Windows (identical to
    /// [`Self::gamma_token`] there), the mirror-set master's `CGDirectDisplayID` in
    /// decimal on macOS. A macOS built-in panel carries it too, which is what lets
    /// a mirror set spanning both backends — the `MacBook`-to-projector layout —
    /// collapse into one control instead of stacking two overlays on one surface.
    /// `None` for a Windows WMI panel, which cannot be correlated to a surface and
    /// so stays its own singleton.
    pub(crate) surface_token: Option<String>,
}

/// Enumerate displays **and** their geometry in one pass.
///
/// Returns the [`DiscoveredDisplay`] list the engine consumes, plus a parallel
/// [`DisplayGeom`] list in the *same* deterministic order (DDC first, then
/// panels). The geometry list feeds an app-side
/// [`BoundsMap`](crate::bin_support::bounds::BoundsMap): a DDC display — including
/// a Windows DDC-fallback internal panel — keeps its DDC geometry, and a panel
/// keeps whatever the OS panel backend reported for it (everything on macOS,
/// nothing on Windows). See [`DisplayGeom`] for the per-platform units and the two
/// tokens. Never errors.
pub(crate) fn discover_all() -> (Vec<DiscoveredDisplay>, Vec<DisplayGeom>) {
    let ddc: Vec<(DiscoveredDisplay, DisplayGeom)> = discover_ddc()
        .into_iter()
        .map(|found| {
            let geom = DisplayGeom {
                id: found.display.id.as_str().to_owned(),
                bounds: Some(found.bounds),
                gamma_token: Some(found.gamma_token),
                surface_token: Some(found.surface_token),
            };
            (found.display, geom)
        })
        .collect();
    let panel: Vec<(DiscoveredDisplay, DisplayGeom)> = discover_panel()
        .into_iter()
        .map(|found| {
            let geom = panel_geom(&found.display, found.geometry.as_ref());
            (found.display, geom)
        })
        .collect();

    merge_displays(ddc, panel).into_iter().unzip()
}

/// Merge the DDC and panel display lists into the tray's display set, applying
/// the internal-panel fallback policy. Kept DDC entries retain their enumeration
/// order and precede the panels; the panel-backend entries always follow.
///
/// "Panel backend" throughout is `duja-panel`: WMI on Windows, `DisplayServices`
/// on macOS.
///
/// Truth table, per DDC entry (the panel-backend panels are always kept):
/// - **External DDC display** — always kept; an external monitor is never in the
///   panel-backend list, so nothing supersedes it.
/// - **Internal DDC display, panel backend returned ≥ 1 panel** — dropped. The
///   panel backend is authoritative for an internal panel it can control, so its
///   [`DisplayKind::InternalPanel`] entry wins and the DDC duplicate is removed.
///   The signal is "the panel backend listed *any* panel", not an id match: on
///   Windows a serial-less panel derives DIFFERENT ids from the two backends
///   (`from_edid` hashes the whole 128-byte EDID; WMI's `from_parts` hashes only
///   `"MFG-PROD"`), so id-matching alone could never dedup it — see
///   `merge_drops_internal_ddc_duplicate_when_wmi_has_the_panel_serial_less`.
/// - **Internal DDC display, panel backend returned 0 panels** — KEPT, as the
///   [`DisplayKind::InternalPanel`] fallback. This is the fix: on a laptop whose
///   backlight is GPU/OEM-driven, WMI cannot see the panel and the DDC path is
///   its only carrier, so dropping it here would leave the built-in screen in
///   neither list, vanished (see `internal_panel_survives_when_wmi_is_empty`).
///
/// # On macOS the dedup is inert
///
/// The two backends cannot overlap there: the macOS DDC backend drops built-in
/// panels at enumeration (`CGDisplayIsBuiltin`), so `discover_ddc` labels every
/// macOS DDC entry [`DisplayKind::ExternalDdc`] and no internal DDC entry ever
/// exists to drop — nor is there any DDC fallback carrier, so a macOS internal
/// panel comes from `DisplayServices` or not at all. The policy below is
/// byte-for-byte the same on both platforms; on macOS it simply never fires, and
/// the truth table's second and third rows describe Windows alone.
fn merge_displays(
    ddc: Vec<(DiscoveredDisplay, DisplayGeom)>,
    panel: Vec<(DiscoveredDisplay, DisplayGeom)>,
) -> Vec<(DiscoveredDisplay, DisplayGeom)> {
    // The panel backend is authoritative for any internal panel it can see, so an
    // internal DDC fallback survives only when that backend listed no panel at all.
    // External DDC entries are always kept (an external is never in the panel list).
    // The dedup signal is "the panel backend listed any panel", NOT an id match,
    // because on Windows a serial-less panel derives divergent ids across the two
    // backends — see the truth table above. On macOS nothing here fires: no DDC
    // entry is ever `InternalPanel`.
    let panel_backend_has_panel = !panel.is_empty();
    let mut out: Vec<(DiscoveredDisplay, DisplayGeom)> = ddc
        .into_iter()
        .filter(|(display, _)| {
            display.kind != DisplayKind::InternalPanel || !panel_backend_has_panel
        })
        .collect();
    out.extend(panel);
    out
}

/// One display found by the DDC backend: its metadata plus the geometry
/// [`discover_all`] folds into a [`DisplayGeom`].
///
/// Named fields rather than a tuple because two of them are same-typed platform
/// tokens whose meanings are not interchangeable — see [`DisplayGeom`]. On Windows
/// they are deliberately the same string.
struct FoundDdc {
    display: DiscoveredDisplay,
    bounds: DisplayBounds,
    gamma_token: String,
    surface_token: String,
}

#[cfg(windows)]
fn discover_ddc() -> Vec<FoundDdc> {
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
                FoundDdc {
                    display,
                    bounds: d.bounds,
                    // One GDI device name does both jobs on Windows: it addresses
                    // the display AND names the framebuffer every mirrored panel
                    // shares. See `DisplayGeom` for why macOS cannot do the same.
                    gamma_token: d.gdi_device.clone(),
                    surface_token: d.gdi_device,
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(target_os = "macos")]
fn discover_ddc() -> Vec<FoundDdc> {
    // Same shape as the Windows arm — metadata, bounds and the display-surface
    // token, with each `DdcDisplay` dropped at the end of the closure so its I2C
    // service handle is released promptly — with two platform differences:
    //
    // 1. The kind is ALWAYS `ExternalDdc`. The macOS DDC backend filters built-in
    //    panels out at enumeration (`CGDisplayIsBuiltin`) and its `DdcDisplay` has
    //    no `is_internal` flag, so unlike Windows there is no internal-DDC fallback
    //    carrier to classify. The consequence is deliberate and worth stating: a
    //    macOS internal panel can only come from `duja-panel`/`DisplayServices`; if
    //    that framework cannot control it, Duja cannot either, and no DDC entry
    //    will stand in for it.
    // 2. The two tokens are DIFFERENT values here, where Windows repeats one
    //    string. `cg_display_id` addresses this display; `surface_id` names its
    //    framebuffer (the mirror-set master when it is a clone). Grouping on the
    //    former would re-introduce #66 (two overlays on one mirrored surface);
    //    addressing gamma through the latter would dim a different display, and
    //    possibly one Duja does not even list. `DisplayGeom` has the full contract.
    //    `bounds` are points here, not physical pixels — also on `DisplayGeom`.

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
                FoundDdc {
                    display,
                    bounds: d.bounds,
                    gamma_token: d.cg_display_id.to_string(),
                    surface_token: d.surface_id.to_string(),
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// No DDC backend on this target: `duja-ddc` exposes `enumerate` only on Windows
/// and macOS, so there is nothing to enumerate (Linux lands in P7).
#[cfg(not(any(windows, target_os = "macos")))]
fn discover_ddc() -> Vec<FoundDdc> {
    Vec::new()
}

/// Enumerate the OS panel backend's internal panels as [`DiscoveredDisplay`]
/// metadata plus whatever geometry the backend reported. Not cfg-gated:
/// `duja_panel::enumerate` exists on every target (it returns an empty list where
/// there is no backend), so this reports real panels on Windows *and* macOS.
/// `open_panel_controller` must therefore be able to open them on both — a table
/// row stamped `hardware_range: true` that no opener can serve would claim control
/// Duja does not have.
fn discover_panel() -> Vec<FoundPanel> {
    match duja_panel::enumerate() {
        Ok(panels) => panels
            .into_iter()
            .map(|p| FoundPanel {
                display: DiscoveredDisplay {
                    id: p.id().clone(),
                    kind: DisplayKind::InternalPanel,
                    name: Some(p.name().to_owned()),
                    capabilities: hardware_brightness_caps(),
                },
                geometry: p.geometry().cloned(),
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// One panel found by the OS panel backend: its metadata plus whatever geometry
/// that backend could report — the panel twin of [`FoundDdc`], so both arms of
/// [`discover_all`] carry their geometry the same way.
///
/// (Unlike [`FoundDdc`] the two fields are differently typed, so the struct is
/// here for symmetry and naming rather than to prevent a transposed pair.)
struct FoundPanel {
    display: DiscoveredDisplay,
    geometry: Option<duja_panel::PanelGeometry>,
}

/// Fold a panel's backend-reported geometry into a [`DisplayGeom`].
///
/// All three fields move together, because [`duja_panel::PanelGeometry`] is
/// all-or-nothing: a backend either knows where its panel is or does not. Today
/// macOS knows and Windows does not (see [`duja_panel::PanelGeometry`] for why),
/// so this one body produces the panel's full geometry on a Mac and the historic
/// all-`None` row on a Windows laptop — no `cfg` needed, and no platform assumed.
///
/// `None` is what keeps `dimming::plan` from planning an overlay at a rectangle
/// nobody knows; `Some` is what finally lets it plan one for a macOS built-in
/// panel, which before this could not be software-dimmed at all.
fn panel_geom(display: &DiscoveredDisplay, geometry: Option<&PanelGeometry>) -> DisplayGeom {
    DisplayGeom {
        id: display.id.as_str().to_owned(),
        bounds: geometry.map(PanelGeometry::bounds),
        gamma_token: geometry.map(|g| g.gamma_token().to_owned()),
        surface_token: geometry.map(|g| g.surface_token().to_owned()),
    }
}

/// Re-enumerate and open a fresh [`BrightnessController`] for `id`, or `None`
/// if the display is not currently present or cannot be opened.
///
/// This is the shape the engine's `ControllerFactory` needs: it re-enumerates
/// on every call so a hot-plugged display always gets a freshly-opened handle.
///
/// **The panel backend is tried before DDC** — WMI on Windows, `DisplayServices`
/// on macOS. A panel the native backlight API can control must be driven through
/// it, not over DDC-on-eDP; and because `duja_ddc::enumerate` also surfaces
/// internal panels on Windows, a DDC-first order could wrongly open a DDC handle
/// for a WMI-owned panel. An external monitor is never in the panel list (WMI
/// lists only `WmiMonitorBrightness` internal panels; `DisplayServices` only
/// built-in ones), so `open_panel` returns `None` for it and it falls through to
/// `open_ddc`. A fallback internal panel that WMI cannot see likewise falls
/// through to `open_ddc`, which re-matches it by id; the engine's verify-first
/// write then routes it to real hardware (if DDC-over-eDP answers) or a
/// software-only overlay (if not). That fallback case is Windows-only — on macOS
/// the lists cannot overlap (see [`merge_displays`]), so the order there just
/// means "built-in panel first, external monitors second".
pub(crate) fn open_controller(id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    open_panel(id).or_else(|| open_ddc(id))
}

/// Open the DDC display matching `id`. One definition for both DDC platforms: the
/// `duja-ddc` surface (`enumerate` → `DdcDisplay { id, .. }` → `into_controller`)
/// and its handle-release-on-drop discipline are identical on Windows and macOS,
/// and both controllers implement `BrightnessController`, so this body is shared
/// rather than copied per platform.
#[cfg(any(windows, target_os = "macos"))]
fn open_ddc(id: &StableDisplayId) -> Option<Box<dyn BrightnessController>> {
    let displays = duja_ddc::enumerate().ok()?;
    let candidates: Vec<&str> = displays.iter().map(|d| d.id.as_str()).collect();
    let idx = duja_core::id::select_slot_match(id.as_str(), &candidates)?;
    // `nth(idx)` consumes and drops the earlier displays (releasing their
    // handles); the remaining iterator is dropped after, releasing the rest.
    let matched = displays.into_iter().nth(idx)?;
    Some(Box::new(matched.into_controller()))
}

/// No DDC backend on this target, so nothing can be opened (Linux lands in P7).
#[cfg(not(any(windows, target_os = "macos")))]
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

/// Open a controller for one enumerated panel. One definition for both panel
/// platforms: `PanelDisplay::open` exists on Windows and macOS with the same
/// signature shape (`Result<PanelController<_>, PanelError>`), and
/// `PanelController` is generic over its transport, so the WMI and
/// `DisplayServices` controllers box identically.
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
    use super::{DisplayGeom, hardware_brightness_caps, merge_displays, panel_geom};
    use duja_core::dimmer::DisplayBounds;
    use duja_core::id::StableDisplayId;
    use duja_core::manager::DiscoveredDisplay;
    use duja_core::model::{DisplayKind, Feature};
    use duja_panel::PanelGeometry;

    // --- the panel backend's geometry, folded into a `DisplayGeom` ------------
    //
    // This is the seam the macOS built-in panel reaches the dimming planner
    // through, and the only one a test on any lane can observe: `discover_panel`
    // itself calls `duja_panel::enumerate`, which needs a machine with a panel.
    // What these pin is the fold — that a reported geometry arrives intact and in
    // the right slots, and that an unreported one stays absent. What they cannot
    // pin is that macOS *reports* one; that is `duja-panel`'s
    // `panel_geometry` tests plus a live Mac (`docs/debt.md`).

    fn panel_display(id: &StableDisplayId) -> DiscoveredDisplay {
        DiscoveredDisplay {
            id: id.clone(),
            kind: DisplayKind::InternalPanel,
            name: Some("Internal Display".to_owned()),
            capabilities: hardware_brightness_caps(),
        }
    }

    /// A Windows/WMI panel reports no geometry, and must keep contributing none:
    /// stamping a placeholder would plan an overlay at a rectangle nobody knows.
    #[test]
    fn a_panel_without_reported_geometry_contributes_none() {
        let id = StableDisplayId::from_parts("GSM", 0x5B09, Some("PANEL")).unwrap();
        let geom = panel_geom(&panel_display(&id), None);

        assert_eq!(geom.id, id.as_str());
        assert_eq!(geom.bounds, None);
        assert_eq!(geom.gamma_token, None);
        assert_eq!(geom.surface_token, None);
    }

    /// The fix. A macOS built-in panel reports bounds and both tokens, and all
    /// three must reach the map — without them `dimming::plan` emits no
    /// `DimCommand` and the panel cannot be software-dimmed at all.
    ///
    /// The three values are deliberately all different, and the two tokens are
    /// the mirror-clone shape (own id `9`, master `4`): a fold that read one
    /// token twice, or crossed the two, would pass a fixture that reused one
    /// string and fails here.
    #[test]
    fn a_panel_with_reported_geometry_carries_bounds_and_both_tokens() {
        let id = StableDisplayId::from_parts("APP", 0xA2E5, Some("1")).unwrap();
        let bounds = DisplayBounds::new(-1512, 12, 1512, 982);
        let reported = PanelGeometry::new(bounds, "9".to_owned(), "4".to_owned());

        let geom = panel_geom(&panel_display(&id), Some(&reported));

        assert_eq!(geom.id, id.as_str());
        assert_eq!(geom.bounds, Some(bounds));
        assert_eq!(geom.gamma_token.as_deref(), Some("9"));
        assert_eq!(geom.surface_token.as_deref(), Some("4"));
    }

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
        (display, ddc_geom(id))
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
        (display, ddc_geom(id))
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
        (
            display,
            DisplayGeom {
                id: id.as_str().to_owned(),
                bounds: None,
                gamma_token: None,
                surface_token: None,
            },
        )
    }

    /// DDC-style geometry: bounds plus both tokens. Windows-shaped, i.e. one
    /// device name in both slots, which is what `discover_ddc` stamps there.
    fn ddc_geom(id: &StableDisplayId) -> DisplayGeom {
        DisplayGeom {
            id: id.as_str().to_owned(),
            bounds: Some(DisplayBounds::new(0, 0, 100, 100)),
            gamma_token: Some(r"\\.\display1".to_owned()),
            surface_token: Some(r"\\.\display1".to_owned()),
        }
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

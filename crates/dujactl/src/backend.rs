//! Direct in-process backend access for `dujactl` (no engine, no IPC).
//!
//! `dujactl` this phase talks straight to `duja-ddc` and `duja-panel`: it
//! enumerates, opens a controller for a display on demand, and does one paced
//! read/write. Handle hygiene mirrors the app's: [`discover`] keeps only
//! metadata (dropping each backend display immediately — releasing its
//! physical-monitor handle on Windows, its I2C service handle on macOS), and
//! [`open`] converts exactly the matched display.
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

/// Enumerate every controllable display (external DDC first, then panels).
///
/// Never errors: a failing backend simply contributes nothing.
///
/// Identical-twin monitors that share one EDID id are disambiguated with
/// `-slot<n>` suffixes — the same convention the daemon's
/// [`DisplayManager`](duja_core::manager::DisplayManager) applies — so every
/// row is individually addressable. [`open`] routes those slot ids back to the
/// Nth physical unit (see [`duja_core::id::select_slot_match`]).
pub fn discover() -> Vec<CtlDisplay> {
    let mut out = discover_ddc();
    out.extend(discover_panel());
    let ids: Vec<StableDisplayId> = out.iter().map(|d| d.id.clone()).collect();
    for (display, resolved) in out
        .iter_mut()
        .zip(duja_core::manager::assign_twin_slots(&ids))
    {
        display.id = resolved;
    }
    out
}

/// Count of external DDC displays seen (for `doctor`).
pub fn ddc_count() -> usize {
    discover_ddc().len()
}

/// Count of internal panels seen (for `doctor`).
pub fn panel_count() -> usize {
    discover_panel().len()
}

/// Map the DDC backend's displays onto [`CtlDisplay`]. One definition for both
/// DDC platforms: this reads only `id` and `name`, which the Windows and macOS
/// `DdcDisplay` share, so nothing here needs adapting per platform (the app's
/// mapping does, because it also reads bounds and the platform gamma token).
///
/// On macOS [`DisplayKind::ExternalDdc`] is exactly right: that backend filters
/// built-in panels out at enumeration (`CGDisplayIsBuiltin`), so every entry it
/// yields *is* an external monitor. On **Windows** the same label is also applied
/// to a DDC-fallback *internal* panel, which the app's mapping classifies
/// `InternalPanel` instead — a pre-existing divergence deliberately left alone by
/// the macOS wiring (as is [`open`]'s DDC-first order); both get their own fix.
#[cfg(any(windows, target_os = "macos"))]
fn discover_ddc() -> Vec<CtlDisplay> {
    match duja_ddc::enumerate() {
        Ok(displays) => displays
            .into_iter()
            .map(|d| CtlDisplay {
                id: d.id.clone(),
                kind: DisplayKind::ExternalDdc,
                name: d.name.clone().unwrap_or_else(|| "-".to_owned()),
            })
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
pub fn open(id: &str) -> Option<Box<dyn BrightnessController>> {
    open_ddc(id).or_else(|| open_panel(id))
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

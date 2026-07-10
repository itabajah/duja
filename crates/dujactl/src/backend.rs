//! Direct in-process backend access for `dujactl` (no engine, no IPC).
//!
//! `dujactl` this phase talks straight to `duja-ddc` and `duja-panel`: it
//! enumerates, opens a controller for a display on demand, and does one paced
//! read/write. Handle hygiene mirrors the app's: [`discover`] keeps only
//! metadata (dropping each backend display immediately), and [`open`] converts
//! exactly the matched display.

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

#[cfg(windows)]
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

#[cfg(not(windows))]
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

#[cfg(windows)]
fn open_ddc(id: &str) -> Option<Box<dyn BrightnessController>> {
    let displays = duja_ddc::enumerate().ok()?;
    let candidates: Vec<&str> = displays.iter().map(|d| d.id.as_str()).collect();
    let idx = duja_core::id::select_slot_match(id, &candidates)?;
    let matched = displays.into_iter().nth(idx)?;
    Some(Box::new(matched.into_controller()))
}

#[cfg(not(windows))]
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

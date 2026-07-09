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
pub fn discover() -> Vec<CtlDisplay> {
    let mut out = discover_ddc();
    out.extend(discover_panel());
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
    let matched = duja_ddc::enumerate()
        .ok()?
        .into_iter()
        .find(|d| d.id.as_str() == id)?;
    Some(Box::new(matched.into_controller()))
}

#[cfg(not(windows))]
fn open_ddc(_id: &str) -> Option<Box<dyn BrightnessController>> {
    None
}

fn open_panel(id: &str) -> Option<Box<dyn BrightnessController>> {
    let matched = duja_panel::enumerate()
        .ok()?
        .into_iter()
        .find(|p| p.id().as_str() == id)?;
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

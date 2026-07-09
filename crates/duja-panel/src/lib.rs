//! Internal laptop-panel brightness control.
//!
//! DDC/CI cannot reach internal panels; each OS has a distinct native API:
//! Windows `WmiMonitorBrightnessMethods` (`root\wmi`), macOS private
//! `DisplayServicesSetBrightness` (dlopen'd, graceful fallback), Linux logind
//! D-Bus `SetBrightness` with a `/sys/class/backlight` write fallback.
//!
//! # Architecture
//!
//! A [`PanelTransport`] is the minimal, OS-specific brightness primitive (query
//! current + levels, set brightness). [`PanelController`] adapts any transport
//! to `duja_core`'s [`BrightnessController`](duja_core::controller::BrightnessController)
//! trait, applying the panel semantics (brightness-only, percent-domain,
//! clamp-on-overrange). This split keeps the `unsafe` COM code confined to the
//! Windows `wmi` module while the whole adapter is exercised cross-platform by
//! `duja_core`'s controller contract suite against a fake transport.
//!
//! # Enumeration and graceful absence
//!
//! [`enumerate`] lists the internal panels that expose brightness control. On a
//! machine with **no** internal panel — every desktop — it returns
//! `Ok(vec![])`, never an error: the absence of the WMI class or of any panel
//! instance is the expected state, not a failure. Only a genuine backend fault
//! on a machine that *does* have a panel surfaces as [`PanelError`].
//!
//! This crate is currently a Windows-only backend; on other targets [`enumerate`]
//! is a no-op returning an empty list, so the workspace still builds and tests
//! everywhere (the pure adapter logic is platform-independent).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod controller;
mod error;
mod transport;

#[cfg(windows)]
pub mod wmi;

pub use controller::PanelController;
pub use error::PanelError;
pub use transport::{PanelBrightness, PanelTransport};

use duja_core::id::StableDisplayId;

/// An internal panel discovered by [`enumerate`], carrying its durable identity,
/// a human-readable name, and enough OS handle to open a controller for it.
///
/// The `instance_name` is the WMI `InstanceName` that keys every `WmiMonitor*`
/// class for this panel; the Windows-only `open` method uses it to bind a
/// transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelDisplay {
    id: StableDisplayId,
    name: String,
    #[cfg_attr(not(windows), allow(dead_code))]
    // RATIONALE: `instance_name` keys the WMI transport in `open()`, which only
    // exists on Windows; on other targets the field is retained for a uniform
    // public type but is unused.
    instance_name: String,
}

impl PanelDisplay {
    /// The panel's durable, EDID-derived identity.
    #[must_use]
    pub fn id(&self) -> &StableDisplayId {
        &self.id
    }

    /// A human-readable name for the panel (falls back to a generic label when
    /// the panel exposes no friendly name).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The WMI `InstanceName` that identifies this panel across the
    /// `WmiMonitor*` classes.
    #[must_use]
    pub fn instance_name(&self) -> &str {
        &self.instance_name
    }

    /// Open a brightness controller bound to this panel.
    ///
    /// Constructs a fresh WMI transport (and COM apartment) on the calling
    /// thread; see [`wmi::WmiTransport`] for the threading contract.
    ///
    /// # Errors
    /// [`PanelError`] if the COM apartment or WMI connection cannot be
    /// established.
    #[cfg(windows)]
    pub fn open(&self) -> Result<PanelController<wmi::WmiTransport>, PanelError> {
        let transport = wmi::WmiTransport::open(self.instance_name.clone())?;
        Ok(PanelController::new(transport))
    }
}

/// Enumerate the internal panels that expose brightness control.
///
/// Returns `Ok(vec![])` when there is no internal panel (the desktop case); see
/// the [crate docs](crate) on graceful absence.
///
/// # Errors
/// [`PanelError`] only on a genuine backend fault (a COM/WMI failure on a
/// machine that has the WMI infrastructure). A missing class or an empty
/// instance set is **not** an error.
#[cfg(windows)]
pub fn enumerate() -> Result<Vec<PanelDisplay>, PanelError> {
    wmi::enumerate()
}

/// Enumerate the internal panels that expose brightness control.
///
/// On non-Windows targets this backend is not implemented, so the list is
/// always empty. See the Windows variant for the real behaviour.
///
/// # Errors
/// Never errors on non-Windows targets.
#[cfg(not(windows))]
pub fn enumerate() -> Result<Vec<PanelDisplay>, PanelError> {
    Ok(Vec::new())
}

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_against_core() {
        assert_eq!(version(), duja_core::version());
    }

    #[test]
    fn enumerate_on_this_machine_does_not_error() {
        // On the CI/dev desktop there is no internal panel, so this must return
        // Ok — an empty list on Windows, and unconditionally empty elsewhere —
        // and must never panic.
        let panels = enumerate().expect("enumerate must not error on this machine");
        #[cfg(not(windows))]
        assert!(panels.is_empty());
        // On Windows we assert only that it succeeded; a machine *with* a panel
        // would legitimately return a non-empty list.
        let _ = panels;
    }

    #[test]
    fn panel_display_accessors() {
        let display = PanelDisplay {
            id: StableDisplayId::from_parts("GSM", 0x5B09, Some("PANEL1")).unwrap(),
            name: "Internal Display".to_owned(),
            instance_name: r"DISPLAY\GSM5B09\4&abcd&0&UID0".to_owned(),
        };
        assert_eq!(display.id().as_str(), "GSM-5B09-PANEL1");
        assert_eq!(display.name(), "Internal Display");
        assert!(display.instance_name().contains("GSM5B09"));
    }
}

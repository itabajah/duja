//! The Windows DDC/CI backend: monitor enumeration, EDID identity, and the real
//! dxva2 [`VcpTransport`].
//!
//! [`enumerate`] discovers the attached external monitors and returns a
//! [`DdcDisplay`] per DDC-capable monitor, each carrying a stable EDID-derived
//! identity and an owned physical-monitor handle. [`DdcDisplay::into_controller`]
//! turns one into a thread-owned [`DdcController`] over the [`Dxva2Transport`].
//!
//! All `unsafe` FFI is confined to the [`sys`] submodule.

// RATIONALE: the backend's public vocabulary (`DdcDisplay`, `DdcError`,
// `Dxva2Transport`) namespaces the crate concept; the qualified names read best
// at call sites and the surface is small.
#![allow(clippy::module_name_repetitions)]

mod sys;

use std::fmt;

use duja_core::dimmer::DisplayBounds;
use duja_core::id::StableDisplayId;
use duja_core::quirks::QuirkDb;
use windows::Win32::Foundation::HANDLE;

use crate::clock::SystemClock;
use crate::controller::DdcController;
use crate::correlate::correlate;
use crate::transport::{TransportError, VcpReading, VcpTransport};

/// A failure enumerating the attached displays.
#[derive(Debug, thiserror::Error)]
pub enum DdcError {
    /// A Windows display or `SetupAPI` call failed.
    #[error("a Windows display enumeration call failed: {0}")]
    Os(windows::core::Error),
}

impl From<windows::core::Error> for DdcError {
    fn from(err: windows::core::Error) -> Self {
        DdcError::Os(err)
    }
}

/// An owned physical-monitor `HANDLE`, safe to move onto a worker thread.
///
/// dxva2 permits single-threaded use of a physical-monitor handle from any
/// thread; the danger is *sharing*. This wrapper owns the handle exclusively —
/// it is never copied or aliased — and destroys it on drop, so exactly one
/// thread ever touches it. Per ADR-0002 each DDC worker owns its handle alone.
struct PhysicalMonitorHandle {
    raw: HANDLE,
}

// SAFETY: the wrapped HANDLE is owned exclusively by whichever thread holds this
// value (it is moved, never cloned or shared) and dxva2 tolerates use of a
// physical-monitor handle from any single thread. No aliasing is possible.
unsafe impl Send for PhysicalMonitorHandle {}

impl fmt::Debug for PhysicalMonitorHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PhysicalMonitorHandle")
            .finish_non_exhaustive()
    }
}

impl Drop for PhysicalMonitorHandle {
    fn drop(&mut self) {
        sys::destroy(self.raw);
    }
}

/// The production [`VcpTransport`]: dxva2 VCP calls against one physical monitor.
#[derive(Debug)]
pub struct Dxva2Transport {
    handle: PhysicalMonitorHandle,
}

impl VcpTransport for Dxva2Transport {
    fn read_vcp(&mut self, code: u8) -> Result<VcpReading, TransportError> {
        sys::get_vcp(self.handle.raw, code)
    }

    fn write_vcp(&mut self, code: u8, value: u16) -> Result<(), TransportError> {
        sys::set_vcp(self.handle.raw, code, value)
    }

    fn read_capabilities(&mut self) -> Result<String, TransportError> {
        sys::read_caps(self.handle.raw)
    }
}

/// One enumerated external monitor: its stable identity, friendly name, raw
/// EDID, and the owned handle needed to control it.
///
/// Turn it into a controller with [`into_controller`](Self::into_controller);
/// dropping it without doing so releases the underlying monitor handle.
#[derive(Debug)]
pub struct DdcDisplay {
    /// Durable EDID-derived identity.
    pub id: StableDisplayId,
    /// Human-readable name (EDID monitor-name descriptor, else the device
    /// string), if one was recovered.
    pub name: Option<String>,
    /// The raw EDID bytes read from the registry.
    pub edid: Vec<u8>,
    /// Physical pixel bounds of this monitor in the virtual desktop, from
    /// `MONITORINFO::rcMonitor`. Feeds the overlay dimmer's per-display window
    /// geometry (see `duja-app`'s bounds map).
    pub bounds: DisplayBounds,
    /// GDI adapter/source device name (e.g. `\\.\DISPLAY1`), from
    /// `MONITORINFOEX::szDevice`. This is the handle `CreateDCW` needs to drive
    /// the display's gamma ramp, so `duja-app` correlates a resolved id to its
    /// gamma target through it (see the app's bounds map and gamma channel).
    pub gdi_device: String,
    handle: PhysicalMonitorHandle,
    sort_key: String,
}

impl DdcDisplay {
    /// Consume this display and build a thread-owned [`DdcController`] over the
    /// dxva2 transport, with quirks resolved from the embedded database.
    #[must_use]
    pub fn into_controller(self) -> DdcController<Dxva2Transport, SystemClock> {
        let quirks = QuirkDb::embedded().resolve(&self.id);
        let transport = Dxva2Transport {
            handle: self.handle,
        };
        DdcController::with_parts(transport, quirks, SystemClock)
    }
}

/// Enumerate the attached DDC-capable **external** monitors, in a deterministic
/// order (sorted by device interface path).
///
/// Identity is recovered from each monitor's registry EDID and correlated to
/// its physical handle via the device interface path. Two classes of target are
/// deliberately **excluded** from the returned list:
///
/// - **Internal / embedded panels** — any target whose CCD `outputTechnology`
///   marks it as internal, embedded `DisplayPort`, or embedded UDI (a laptop's
///   built-in eDP). These belong to `duja-panel`, which enumerates them as
///   internal panels; surfacing them here too would double-count and mislabel
///   the built-in screen as external.
/// - A monitor whose EDID cannot be correlated or parsed — skipped rather than
///   given a fabricated identity.
///
/// Every returned [`DdcDisplay`] is therefore a real external monitor with a
/// genuine EDID-derived [`StableDisplayId`]; each excluded target's
/// physical-monitor handle is released.
///
/// # Errors
/// [`DdcError::Os`] if the `SetupAPI` device-information set cannot be opened.
pub fn enumerate() -> Result<Vec<DdcDisplay>, DdcError> {
    let edids = sys::collect_monitor_edids()?;
    let targets = sys::monitor_paths();
    // Pure correlation of path -> EDID -> identity. Internal panels are omitted
    // (they belong to `duja-panel`), so their `HMONITOR`s below match nothing
    // and are dropped rather than mislabelled as external DDC monitors.
    let resolved = correlate(&targets, &edids);

    let mut displays = Vec::new();
    for hmon in sys::enum_hmonitors() {
        let Some((gdi, bounds)) = sys::gdi_device_and_bounds(hmon) else {
            continue;
        };
        let Ok(handles) = sys::physical_monitors(hmon) else {
            continue;
        };
        let mut handles = handles.into_iter();
        let Some(first) = handles.next() else {
            continue;
        };
        // Wrap the primary handle immediately so every early-exit path below
        // releases it via Drop. One HMONITOR yields one monitor identity, so any
        // extra physical monitors (mirrored sets) are released explicitly.
        let handle = PhysicalMonitorHandle { raw: first };
        for extra in handles {
            sys::destroy(extra);
        }

        // Attach this HMONITOR's handle + bounds to its correlated external
        // identity, matched by GDI adapter name. An HMONITOR with no external
        // identity (the internal panel, or a monitor whose EDID failed to
        // correlate) drops its handle here.
        let gdi_key = gdi.to_ascii_lowercase();
        let Some(display) = resolved.iter().find(|c| c.gdi_device == gdi_key) else {
            continue; // `handle` drops here, releasing the monitor.
        };

        displays.push(DdcDisplay {
            id: display.id.clone(),
            name: display.name.clone(),
            edid: display.edid.clone(),
            bounds,
            gdi_device: gdi,
            handle,
            sort_key: display.sort_key.clone(),
        });
    }

    displays.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    Ok(displays)
}

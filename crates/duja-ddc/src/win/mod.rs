//! The Windows DDC/CI backend: monitor enumeration, EDID identity, and the real
//! dxva2 [`VcpTransport`].
//!
//! [`enumerate`] discovers the attached monitors and returns a [`DdcDisplay`] per
//! DDC-capable monitor, each carrying a stable EDID-derived identity and an owned
//! physical-monitor handle — normally an external monitor, but also a laptop's
//! internal panel when it is surfaced as a fallback (see [`enumerate`]).
//! [`DdcDisplay::into_controller`] turns one into a thread-owned
//! [`DdcController`] over the [`Dxva2Transport`].
//!
//! All `unsafe` FFI is confined to the [`sys`] submodule.

// RATIONALE: the backend's public vocabulary (`DdcDisplay`, `DdcError`,
// `Dxva2Transport`) namespaces the crate concept; the qualified names read best
// at call sites and the surface is small.
#![allow(clippy::module_name_repetitions)]

mod sys;

use std::collections::BTreeMap;
use std::fmt;

use duja_core::dimmer::DisplayBounds;
use duja_core::id::StableDisplayId;
use duja_core::quirks::QuirkDb;
use windows::Win32::Foundation::HANDLE;

use crate::clock::SystemClock;
use crate::controller::DdcController;
use crate::correlate::{
    CorrelatedDisplay, correlate, leftover_handles, pair_handles_to_displays, should_probe,
};
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

/// One enumerated monitor: its stable identity, friendly name, raw EDID, and the
/// owned handle needed to control it.
///
/// Usually an external DDC/CI monitor, but it may be a laptop's internal panel
/// surfaced as a fallback — see [`is_internal`](Self::is_internal) and
/// [`enumerate`].
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
    /// Whether this is a laptop's internal, embedded panel (eDP) surfaced as a
    /// **fallback**, rather than an external monitor. `duja-app` maps it to
    /// `DisplayKind::InternalPanel` and prefers WMI to drive it; the DDC handle
    /// carried here is used only when WMI cannot see the panel at all. See
    /// [`enumerate`].
    pub is_internal: bool,
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

/// Enumerate the attached DDC-capable monitors, in a deterministic order (sorted
/// by device interface path).
///
/// Identity is recovered from each monitor's registry EDID and correlated to its
/// physical handle via the device interface path. Every returned [`DdcDisplay`]
/// has a genuine EDID-derived [`StableDisplayId`]; a target whose EDID cannot be
/// correlated or parsed is skipped rather than given a fabricated identity, and
/// its physical-monitor handle is released.
///
/// # Internal panels (fallback)
/// A laptop's built-in panel — any target whose CCD `outputTechnology` marks it
/// internal, embedded `DisplayPort`, or embedded UDI — is normally owned by
/// `duja-panel`. But on many laptops the panel's backlight is driven by the
/// GPU/OEM stack, not ACPI/WMI, so that backend cannot see it and the panel would
/// otherwise appear in neither list and vanish. So it is surfaced here too,
/// flagged [`is_internal`](DdcDisplay::is_internal), as a **fallback carrier** —
/// never at an external monitor's expense. An internal identity binds only to a
/// physical-monitor handle left over after external pairing (via the correlation
/// seam's `leftover_handles`); `duja-app` prefers WMI to drive it and only falls
/// back to this DDC handle when WMI has no panel at all. An internal target with
/// no parseable EDID is skipped, exactly like an external one.
///
/// # Duplicate (mirror) mode
/// One GDI source — hence one `HMONITOR` — can front several physical panels,
/// each with its own physical-monitor handle. Enumeration emits **one
/// [`DdcDisplay`] per external panel**, so a mirrored pair becomes two
/// independently controllable rows (identical panels collide on their bare id
/// and are later slotted `-slot0`/`-slot1`). When such an `HMONITOR` carries
/// more handles than external identities (a laptop's built-in eDP mirrored with
/// an external monitor), each handle is DDC-probed so the external identity binds
/// to the handle that answers; the silent eDP handle it yields is then the
/// leftover that the internal identity binds, so the built-in panel is surfaced
/// rather than released.
///
/// # Errors
/// [`DdcError::Os`] if the `SetupAPI` device-information set cannot be opened.
pub fn enumerate() -> Result<Vec<DdcDisplay>, DdcError> {
    let edids = sys::collect_monitor_edids()?;
    let targets = sys::monitor_paths();
    // Pure correlation of path -> EDID -> identity. Internal panels are now
    // resolved too (flagged `is_internal`) so a built-in panel WMI cannot see is
    // not lost; they are bound below only to handles external pairing leaves over.
    // In Duplicate (mirror) mode one GDI source fronts several targets, so
    // `correlate` yields one display PER mirrored panel, all carrying that
    // source's GDI name.
    let resolved = correlate(&targets, &edids);

    let mut displays = Vec::new();
    for hmon in sys::enum_hmonitors() {
        let Some((gdi, bounds)) = sys::gdi_device_and_bounds(hmon) else {
            continue;
        };
        let Ok(raw_handles) = sys::physical_monitors(hmon) else {
            continue;
        };
        // Own every physical handle immediately (RAII): from here on, however
        // this iteration exits, each handle is released exactly once — moved into
        // an emitted `DdcDisplay` or dropped. Nothing can leak or double-free, and
        // no early-exit added below can regress that.
        let handles: Vec<PhysicalMonitorHandle> = raw_handles
            .into_iter()
            .map(|raw| PhysicalMonitorHandle { raw })
            .collect();

        // Every identity correlated to THIS HMONITOR's GDI source, split by kind.
        // A mirrored HMONITOR matches more than one external — each becomes its
        // own controllable row (the v0.1.2 mirror fix, where the old code kept
        // only the first) — and may additionally match an internal panel mirrored
        // onto the same source.
        let gdi_key = gdi.to_ascii_lowercase();
        let matched: Vec<&CorrelatedDisplay> = resolved
            .iter()
            .filter(|c| c.gdi_device == gdi_key)
            .collect();
        let externals: Vec<&CorrelatedDisplay> =
            matched.iter().copied().filter(|c| !c.is_internal).collect();
        let internals: Vec<&CorrelatedDisplay> =
            matched.iter().copied().filter(|c| c.is_internal).collect();

        // --- external pairing: bit-for-bit the pre-fix (v0.1.2) behaviour ---
        // Bind each external display to one physical handle. Probe only when
        // `should_probe` sees a real ambiguity — MORE handles than externals AND
        // at least one external to bind: a laptop's eDP mirrored beside an
        // external monitor (two handles, one external identity), so the external
        // attaches to the handle that answers DDC, not the silent eDP.
        // `sys::handle_answers_ddc` is paced + retried (the P1 read model), so a
        // genuine external is not mis-read as silent — which would otherwise bind
        // the display to, and keep, the wrong handle. When every handle already
        // has an external (a lone monitor, or identical external twins whose
        // handles drive interchangeable panels), OR there is no external at all (an
        // internal-only HMONITOR), probing is skipped: those paced reads (~300 ms
        // per silent handle, on the engine thread) would be pure waste. This reads
        // `externals` where the old code read `matched` — identical, since
        // `matched` then held only externals (internals having been dropped in
        // correlation) — so external probing and `-slot<n>` ordering are unchanged.
        let answers: Vec<bool> = if should_probe(handles.len(), externals.len()) {
            handles
                .iter()
                .map(|h| sys::handle_answers_ddc(h.raw))
                .collect()
        } else {
            vec![true; handles.len()]
        };
        let external_pairs = pair_handles_to_displays(&answers, externals.len());
        let used: Vec<usize> = external_pairs
            .iter()
            .map(|&(_, handle_idx)| handle_idx)
            .collect();

        // --- internal fallback: bind to the handles external pairing left over ---
        // The external probe yields a silent eDP handle to the responsive external
        // one, so that eDP handle is exactly a leftover here — the internal panel
        // binds it and is surfaced instead of released. Leftovers are disjoint from
        // `used`, so no handle is bound twice; an internal beyond the leftover
        // count stays unbound (its handle is dropped below, and the id must reach
        // the tray via WMI instead).
        let leftover = leftover_handles(&used, handles.len());

        // handle index -> the display (external or internal) it carries. The
        // external and internal index sets are disjoint, so each handle maps to at
        // most one display and is consumed exactly once in the final loop.
        let mut handle_to_display: BTreeMap<usize, &CorrelatedDisplay> = BTreeMap::new();
        for (display_idx, handle_idx) in external_pairs {
            if let Some(&display) = externals.get(display_idx) {
                handle_to_display.insert(handle_idx, display);
            }
        }
        for (&display, handle_idx) in internals.iter().zip(leftover) {
            handle_to_display.insert(handle_idx, display);
        }

        // Consume every handle exactly once: a bound handle is MOVED into its
        // `DdcDisplay`; an unbound one (a handle no external answered on and no
        // internal claimed) is dropped at the end of its turn, releasing the
        // physical monitor. `pair_handles_to_displays` binds responsive handles
        // first, so a handle that answered DDC on any attempt is never the one
        // dropped here.
        for (handle_idx, handle) in handles.into_iter().enumerate() {
            if let Some(display) = handle_to_display.get(&handle_idx) {
                displays.push(DdcDisplay {
                    id: display.id.clone(),
                    name: display.name.clone(),
                    edid: display.edid.clone(),
                    bounds,
                    gdi_device: gdi.clone(),
                    is_internal: display.is_internal,
                    handle,
                    sort_key: display.sort_key.clone(),
                });
            }
        }
    }

    displays.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    Ok(displays)
}

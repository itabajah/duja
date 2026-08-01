//! The macOS DDC/CI backend: monitor enumeration, EDID identity, and the two
//! I2C transports (Apple Silicon `IOAVService`, Intel `IOI2CInterface`).
//!
//! [`enumerate`] discovers the attached **external** monitors via CoreGraphics
//! (the internal panel is skipped — it belongs to `duja-panel`), recovers a
//! stable EDID-derived identity for each, resolves its I2C service, and returns
//! a [`DdcDisplay`] per controllable monitor. [`DdcDisplay::into_controller`]
//! turns one into a thread-owned [`DdcController`] over the cross-platform
//! [`DdcCiTransport`].
//!
//! This mirrors the Windows `win` module surface. The one shape difference:
//! there is no GDI device on macOS, and the single Windows device name does two
//! jobs — addressing a display and naming its framebuffer — that macOS splits, so
//! [`DdcDisplay`] exposes **two** CoreGraphics ids where Windows has one string
//! (see `cg_display_id` and `surface_id`). All `unsafe` FFI is confined to the
//! [`sys`] submodule.
//!
//! # Experimental — hardware-unverified
//! Duja has no macOS hardware and CI's mac runners are virtualized (no external
//! DDC display), so this whole module has **never executed against a real
//! monitor**. `enumerate` returns an empty list there and degrades gracefully
//! everywhere a capability is absent. DDC-on-mac is experimental until there
//! are at least three independent community confirmations per architecture
//! (plan §P6). See ADR-0013 and the crate-level docs.

// RATIONALE: the backend's public vocabulary (`DdcDisplay`, `DdcError`)
// namespaces the crate concept; the qualified names read best at call sites and
// the surface is small — identical policy to the Windows `win` module.
#![allow(clippy::module_name_repetitions)]
// RATIONALE: the docs name Apple frameworks/APIs (CoreGraphics, CoreDisplay,
// IOAVService, IOI2CInterface, CGDisplayBounds) in running prose; backticking
// every mention would hurt readability.
#![allow(clippy::doc_markdown)]

mod sys;

use duja_core::dimmer::DisplayBounds;
use duja_core::id::{EdidInfo, StableDisplayId};
use duja_core::quirks::QuirkDb;

use crate::clock::SystemClock;
use crate::controller::DdcController;
use crate::ddcci::DdcCiTransport;

pub use sys::MacI2cBus;

/// A failure enumerating the attached displays.
///
/// Enumeration is otherwise best-effort — a monitor whose EDID or I2C service
/// cannot be recovered is silently skipped, never surfaced as an error — so the
/// only hard failure is CoreGraphics refusing to list displays at all.
#[derive(Debug, thiserror::Error)]
pub enum DdcError {
    /// The CoreGraphics active-display query failed, with the raw `CGError`.
    #[error("the CoreGraphics display enumeration call failed (CGError {0})")]
    CoreGraphics(i32),
}

/// One enumerated external monitor: its stable identity, friendly name, raw
/// EDID, point-space bounds, the CoreGraphics id that addresses it, the surface
/// token that names its framebuffer, and the owned I2C bus needed to control it.
///
/// Turn it into a controller with [`into_controller`](Self::into_controller);
/// dropping it without doing so releases the underlying I2C service handle.
#[derive(Debug)]
pub struct DdcDisplay {
    /// Durable EDID-derived identity.
    pub id: StableDisplayId,
    /// Human-readable name (EDID monitor-name descriptor), if one was recovered.
    pub name: Option<String>,
    /// The raw EDID bytes read from CoreDisplay.
    pub edid: Vec<u8>,
    /// Bounds of this monitor in the global display coordinate space.
    ///
    /// These come from `CGDisplayBounds`, which returns **points, not physical
    /// pixels** (a Retina display reports its logical point size). No conversion
    /// to physical pixels is owed: `duja-dimmer`'s macOS overlay backend
    /// (`MacDimmer`, which *is* its `PlatformDimmer` there) positions windows with
    /// `NSWindow` frames, which are points as well, so this value flows to the
    /// dimmer unconverted. That crate's `mac_geom` module owns the unit contract
    /// and the Cocoa y-flip; the divergence from the Windows backend's
    /// physical-pixel bounds is a property of the platform, not a pending fixup.
    /// See ADR-0013.
    pub bounds: DisplayBounds,
    /// This display's own CoreGraphics id — the value that **addresses** it.
    ///
    /// The macOS analogue of the Windows backend's `gdi_device` for the
    /// addressing half of that field's job: the gamma channel resolves a display
    /// to this and hands it to `CGSetDisplayTransferByFormula`. It is *not* the
    /// value to group mirrored panels by — that is
    /// [`surface_id`](Self::surface_id), and the two differ exactly when this
    /// display is a mirror clone.
    ///
    /// Not stable across replug (that is what [`id`](Self::id) is for) — it is a
    /// live token for this enumeration.
    pub cg_display_id: u32,
    /// The **display-surface token**: which framebuffer this display draws from,
    /// and the value mirrored panels are grouped by.
    ///
    /// `CGDisplayMirrorsDisplay(id)` when this display is a mirror clone, else
    /// its own `CGDirectDisplayID`, so every panel sharing one framebuffer
    /// reports the same value. Grouping on the raw per-display id instead is the
    /// `#66` double-overlay defect: it is unique by construction, so two mirrored
    /// Macs would produce two tokens at identical bounds and two overlays on one
    /// surface.
    ///
    /// **A key, not an address.** A clone's value is another display's id, and
    /// that display need not be one this backend returned — the built-in panel is
    /// filtered out (`CGDisplayIsBuiltin`) yet is the master whenever a `MacBook`
    /// mirrors its own screen to a projector. Comparing it is correct; passing it
    /// to a per-display call is not. The full rule, and why it is shared with
    /// `duja-panel` rather than copied, is on [`duja_core::macos`].
    pub surface_id: u32,
    bus: MacI2cBus,
    sort_key: u32,
}

impl DdcDisplay {
    /// Consume this display and build a thread-owned [`DdcController`] over the
    /// DDC/CI transport, with quirks resolved from the embedded database.
    #[must_use]
    pub fn into_controller(self) -> DdcController<DdcCiTransport<MacI2cBus>, SystemClock> {
        let quirks = QuirkDb::embedded().resolve(&self.id);
        let transport = DdcCiTransport::new(self.bus);
        DdcController::with_parts(transport, quirks, SystemClock)
    }
}

/// Enumerate the attached DDC-capable external monitors, in a deterministic
/// order (sorted by `CGDirectDisplayID`).
///
/// The internal panel is skipped (`CGDisplayIsBuiltin`). Identity is recovered
/// from each monitor's EDID (via CoreDisplay); a monitor whose EDID cannot be
/// read or parsed, or whose I2C service cannot be resolved, is **skipped**
/// rather than given a fabricated identity — so every returned [`DdcDisplay`]
/// has a genuine EDID-derived [`StableDisplayId`] and a live bus.
///
/// On a machine (or CI runner) with no external DDC display this returns an
/// empty list, never an error.
///
/// # Errors
/// [`DdcError::CoreGraphics`] only if the CoreGraphics active-display query
/// itself fails.
pub fn enumerate() -> Result<Vec<DdcDisplay>, DdcError> {
    let mut displays = Vec::new();
    for raw in sys::enumerate_displays()? {
        let Ok(id) = StableDisplayId::from_edid(&raw.edid) else {
            continue;
        };
        let name = EdidInfo::parse(&raw.edid)
            .ok()
            .and_then(|info| info.monitor_name);
        displays.push(DdcDisplay {
            id,
            name,
            edid: raw.edid,
            bounds: raw.bounds,
            cg_display_id: raw.mirror.display_id,
            // CAUTION: these two lines are the one place the addressing token and
            // the surface token are written side by side, and both are `u32`, so
            // swapping them compiles and lints clean. `MirrorState` protects the
            // FFI *read* site — the pair is assembled once, where both values are
            // fetched — but it does not protect this *assignment*, and the P6 gate
            // measured the gap: the swap leaves the whole suite green, because
            // this module is `cfg(target_os = "macos")`, has no test module, and
            // `enumerate()` returns empty on CI's virtualized runners.
            //
            // `duja-panel` does not have this hole: its equivalent assembly is the
            // pure `panel_geometry`, tested on every lane, where the same swap
            // reds. Hoisting this one the same way is tracked in `docs/debt.md`.
            //
            // Swapped, a `MacBook` mirroring to a projector would drive gamma on
            // the built-in panel — a display this backend deliberately never
            // enumerates — while the monitor the user is dragging does not move.
            surface_id: duja_core::macos::surface_id(raw.mirror),
            bus: raw.bus,
            sort_key: raw.mirror.display_id,
        });
    }
    displays.sort_by_key(|d| d.sort_key);
    Ok(displays)
}

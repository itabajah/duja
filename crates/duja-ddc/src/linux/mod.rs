//! The Linux DDC/CI backend: DRM connector enumeration and the `/dev/i2c` bus.
//!
//! [`enumerate`] walks the DRM connector tree (the pure, root-injected
//! [`duja_core::linux::drm`] scanner), recovers a stable EDID-derived identity for each
//! connected display, opens the I2C adapter the kernel bound to it, and returns
//! a [`DdcDisplay`] per controllable monitor.
//! [`DdcDisplay::into_controller`] turns one into a thread-owned
//! [`DdcController`] over the cross-platform [`DdcCiTransport`].
//!
//! This mirrors the `win` and `mac` module surfaces with one shape difference,
//! and it is the important one: **a Linux [`DdcDisplay`] carries no bounds.**
//! Sysfs knows which monitors exist and how to talk to them; it does not know
//! where the desktop puts them, because on Linux that is the display server's
//! answer and there may not be one. The [`connector`](DdcDisplay::connector)
//! name is the join key that supplies it later: on the modern stack (the
//! modesetting DDX, DRM-backed Wayland compositors) X11 `RandR` output names and
//! Wayland `xdg_output` names are the same `DP-1` / `HDMI-A-2` strings sysfs
//! uses, minus the `card<N>-` prefix [`duja_core::linux::drm`] already strips.
//! Not universally — that module records the two drivers it is reported not to
//! hold for, and why wave 4 owes the join a fallback.
//!
//! # Hardware-unverified
//!
//! Duja has no Linux machine and a GitHub runner has no monitor, no DRM
//! connector with an EDID, and no `/dev/i2c-*`. [`enumerate`] therefore returns
//! an empty list in CI and the ioctl path in [`sys`] has never executed against
//! a real display. What *is* verified on every lane is everything above the
//! syscall: the connector scanning and its rules ([`duja_core::linux::drm`]) and the
//! DDC/CI framing ([`crate::ddcci`]), which is the same codec the Intel macOS
//! path uses.

// RATIONALE: `LinuxI2cBus` repeats the crate's I2C vocabulary in a module already
// named `linux`. The qualified name reads best at call sites and matches
// `MacI2cBus` beside it; the surface is small and frozen by the trait it
// implements. (`DdcDisplay`/`DdcError` are re-exported under the crate root where
// the stem is the crate's own, as on the `win` and `mac` modules.)
#![allow(clippy::module_name_repetitions)]

mod sys;

use std::io;
use std::path::Path;

use duja_core::id::{EdidInfo, StableDisplayId};
use duja_core::linux::drm;
use duja_core::quirks::QuirkDb;

use crate::clock::SystemClock;
use crate::controller::DdcController;
use crate::ddcci::DdcCiTransport;

pub use sys::LinuxI2cBus;

/// The filesystem root the DRM tree is read from in production.
const SYSFS_ROOT: &str = "/";

/// A failure enumerating the attached displays.
///
/// Enumeration is otherwise best-effort — a connector with no EDID, no I2C
/// adapter, or a bus that cannot be opened is skipped rather than surfaced — so
/// the only hard failure is the DRM tree itself being unreadable.
#[derive(Debug, thiserror::Error)]
pub enum DdcError {
    /// `/sys/class/drm` exists but could not be read: `/sys` is not mounted,
    /// or the process cannot traverse it.
    ///
    /// Deliberately *not* the same answer as the tree being absent, which is an
    /// ordinary empty list (a container, a headless server). Reporting a
    /// mounting fault as "no monitors found" would send a user looking for a
    /// hardware problem they do not have.
    #[error("the DRM connector tree could not be read: {0}")]
    Sysfs(#[source] io::Error),
}

/// One enumerated external monitor: its stable identity, friendly name, raw
/// EDID, the DRM connector it hangs off, and the owned I2C bus needed to control
/// it.
///
/// Turn it into a controller with [`into_controller`](Self::into_controller);
/// dropping it without doing so closes the underlying `/dev/i2c-*` descriptor.
#[derive(Debug)]
pub struct DdcDisplay {
    /// Durable EDID-derived identity.
    pub id: StableDisplayId,
    /// Human-readable name (EDID monitor-name descriptor), if one was recovered.
    pub name: Option<String>,
    /// The raw EDID bytes read from the connector.
    pub edid: Vec<u8>,
    /// The DRM connector name with its `card<N>-` prefix stripped: `DP-1`,
    /// `HDMI-A-2`.
    ///
    /// The join key to the display server's idea of this monitor, and therefore
    /// the route by which this display eventually acquires a desktop rectangle.
    /// `RandR` and `xdg_output` use the same strings on the modern stack, with
    /// the caveats [`duja_core::linux::drm`] records. Not stable across a replug
    /// into a different port (that is what [`id`](Self::id) is for).
    pub connector: String,
    bus: LinuxI2cBus,
}

impl DdcDisplay {
    /// Consume this display and build a thread-owned [`DdcController`] over the
    /// DDC/CI transport, with quirks resolved from the embedded database.
    #[must_use]
    pub fn into_controller(self) -> DdcController<DdcCiTransport<LinuxI2cBus>, SystemClock> {
        let quirks = QuirkDb::embedded().resolve(&self.id);
        let transport = DdcCiTransport::new(self.bus);
        DdcController::with_parts(transport, quirks, SystemClock)
    }
}

/// Enumerate the attached DDC-capable external monitors, in a deterministic
/// order (sorted by DRM connector name).
///
/// Built-in panels are skipped: DDC/CI cannot reach one, so an eDP connector has
/// no bus to open even where the driver publishes an adapter for it. That is a
/// property of the hardware, unlike the Windows backend's internal fallback,
/// which exists because WMI sometimes cannot see a panel the DDC path can.
///
/// A connector whose EDID cannot be parsed, whose driver publishes no I2C
/// adapter, or whose `/dev/i2c-*` cannot be opened is **skipped** rather than
/// given a fabricated identity or a dead bus. The last of those is the common
/// one: `i2c-dev` ships no udev rule, so on a stock system those nodes are
/// root-only and a user sees no monitors at all. That is why `dujactl doctor`
/// reports the per-connector reason separately instead of leaving the user with
/// an empty list and no explanation.
///
/// On a machine with no DRM tree (a container, a headless server, a CI runner)
/// this returns an empty list, never an error.
///
/// # Errors
/// [`DdcError::Sysfs`] only if `/sys/class/drm` exists and cannot be read.
pub fn enumerate() -> Result<Vec<DdcDisplay>, DdcError> {
    let connectors = drm::scan(Path::new(SYSFS_ROOT)).map_err(DdcError::Sysfs)?;
    let mut displays = Vec::new();
    for connector in connectors {
        if connector.is_internal {
            continue;
        }
        let Ok(index) = connector.i2c else {
            continue;
        };
        let Ok(id) = StableDisplayId::from_edid(&connector.edid) else {
            continue;
        };
        let Ok(bus) = LinuxI2cBus::open(index) else {
            continue;
        };
        let name = EdidInfo::parse(&connector.edid)
            .ok()
            .and_then(|info| info.monitor_name);
        displays.push(DdcDisplay {
            id,
            name,
            edid: connector.edid,
            connector: connector.name,
            bus,
        });
    }
    Ok(displays)
}

//! The Linux internal-panel backend: `/sys/class/backlight` for reading, logind
//! for writing, and a direct sysfs write as the fallback.
//!
//! [`enumerate`] reports the machine's built-in panel, if it has one that
//! exposes brightness control. Two sysfs trees answer together, because neither
//! is sufficient alone:
//!
//! - `/sys/class/backlight` has the **control**: a step count and a current
//!   level. It has no identity at all — a device name like `intel_backlight` is
//!   a driver's name, not a display's, and it is the same string on every laptop
//!   with that chipset.
//! - `/sys/class/drm`'s internal connector (`eDP-1`, `LVDS-1`) has the
//!   **identity**: the panel's EDID, which is what every other Duja backend
//!   derives a [`StableDisplayId`] from.
//!
//! Both scans are the root-injected, testable-everywhere ones in
//! [`duja_core::linux::drm`] and [`crate::backlight`]; this module is the join,
//! plus the write channel.
//!
//! # A panel with no EDID is not reported
//!
//! If the internal connector publishes no EDID — some panels do not, and a
//! `simpledrm` framebuffer never does — then there is a controllable backlight
//! with no durable name to file it under. Duja reports nothing rather than
//! inventing one, which is the same rule the DDC backends follow, and it costs
//! that machine its panel control. Recorded in `docs/debt.md` rather than
//! papered over with a fabricated id, because an id that is not derived from the
//! hardware collides the moment a second machine runs the same code.
//!
//! # Writing: logind first
//!
//! `/sys/class/backlight/<dev>/brightness` is root-owned on a stock system;
//! whether an ordinary user can write it depends on a udev rule the
//! distribution may or may not ship. logind's `SetBrightness` is designed for
//! exactly this and works for the active session without one, so it is tried
//! first and the direct write is the fallback (ADR-0022). A panel that neither
//! channel can drive is reported as read-only rather than hidden.

// The write channel and the transport that drives it. Linux-only, because
// `logind` is a D-Bus client and `zbus` is a Linux-target dependency. Everything
// *above* them in this file is not: the join between the two sysfs trees is
// plain path arithmetic over an injected root, so it compiles and its tests run
// on all three CI lanes, in the pattern `mac_events` / `mac_geom` established.
#[cfg(target_os = "linux")]
mod logind;
#[cfg(target_os = "linux")]
mod sysfs;

use std::path::Path;

use duja_core::id::{EdidInfo, StableDisplayId};
use duja_core::linux::drm;

use crate::backlight::{self, Backlight};
use crate::error::PanelError;
use crate::{PanelDisplay, PanelGeometry};

#[cfg(target_os = "linux")]
use crate::controller::PanelController;

#[cfg(target_os = "linux")]
pub use sysfs::LinuxPanelTransport;

/// The filesystem root both sysfs trees are read from in production.
#[cfg(target_os = "linux")]
const SYSFS_ROOT: &str = "/";

/// Enumerate the internal panel, if this machine has a controllable one.
///
/// Returns an empty list on every machine with no backlight device (all
/// desktops), on one whose internal connector publishes no EDID (see the module
/// docs), and on any non-Linux host — never an error. A machine with several
/// backlight devices reports **one** panel, driven through the best of them
/// (see [`crate::backlight`] for the ordering): they are all the same physical
/// screen, and reporting three rows for one panel would let a user set three
/// different brightnesses on it.
#[cfg(target_os = "linux")]
pub(crate) fn enumerate() -> Vec<PanelDisplay> {
    enumerate_from(Path::new(SYSFS_ROOT))
}

/// [`enumerate`] against an injected root.
fn enumerate_from(root: &Path) -> Vec<PanelDisplay> {
    let Some(device) = backlight::scan(root).into_iter().next() else {
        return Vec::new();
    };
    let Some(panel) = internal_connector(root) else {
        return Vec::new();
    };
    let Ok(id) = StableDisplayId::from_edid(&panel.edid) else {
        return Vec::new();
    };
    let name = EdidInfo::parse(&panel.edid)
        .ok()
        .and_then(|info| info.monitor_name)
        .unwrap_or_else(|| "Internal Display".to_owned());
    vec![PanelDisplay {
        id,
        name,
        // The backlight device name, which is both the handle `open` re-scans
        // for and the second argument logind's `SetBrightness` takes. Documented
        // opaque like every other backend's, so no caller may parse it.
        instance_name: device.name,
        // No bounds: sysfs knows the panel exists, not where the desktop puts
        // it. That answer belongs to the display server, and there may not be
        // one. `None` means "this backend cannot say", exactly as on Windows.
        geometry: None::<PanelGeometry>,
    }]
}

/// The built-in panel's DRM connector: the first internal one carrying an EDID.
///
/// "First" is by connector name and therefore deterministic. More than one
/// internal connector on a machine is a dual-screen laptop, which Duja has never
/// seen and cannot reason about honestly; taking the first is a documented
/// arbitrary choice rather than a claim about which panel the backlight drives.
fn internal_connector(root: &Path) -> Option<drm::DrmConnector> {
    drm::scan(root)
        .ok()?
        .into_iter()
        .find(|connector| connector.is_internal)
}

/// Open a brightness controller for an enumerated panel.
///
/// Re-scans rather than caching the [`Backlight`] from enumeration: the level
/// moves under Duja's feet (a brightness key, a power-profile daemon), and a
/// stale `current` would be reported to the user as the truth.
///
/// # Errors
/// [`PanelError::Disconnected`] if the named backlight device is no longer
/// present: the panel was enumerated and has since gone, which on a laptop means
/// a driver was unloaded or the device was renamed by a kernel upgrade.
#[cfg(target_os = "linux")]
pub(crate) fn open(
    instance_name: &str,
) -> Result<PanelController<LinuxPanelTransport>, PanelError> {
    let root = Path::new(SYSFS_ROOT);
    let device = find_device(root, instance_name)?;
    Ok(PanelController::new(LinuxPanelTransport::new(device)))
}

/// Locate the enumerated backlight device by name under `root`.
fn find_device(root: &Path, instance_name: &str) -> Result<Backlight, PanelError> {
    backlight::scan(root)
        .into_iter()
        .find(|device| device.name == instance_name)
        .ok_or(PanelError::Disconnected)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    /// The fixed 8-byte EDID header.
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

    /// One EDID base block, before its checksum byte.
    const EDID_BODY_LEN: usize = 127;

    /// A 128-byte EDID base block for manufacturer `GSM` (LG), product `0x5B09`,
    /// with no serial-number descriptor and a correct checksum — enough for
    /// `StableDisplayId::from_edid` to accept it, which is all this module needs
    /// of it. Built by appending rather than by index, so the test module stays
    /// clean under `indexing_slicing`.
    fn edid() -> Vec<u8> {
        let mut edid = Vec::with_capacity(EDID_BODY_LEN.saturating_add(1));
        edid.extend_from_slice(&HEADER);
        // Manufacturer id "GSM": ('G'=7, 'S'=19, 'M'=13) packed 5 bits each into
        // a big-endian u16, so 0x1E6D.
        edid.extend_from_slice(&[0x1E, 0x6D]);
        // Product code 0x5B09, little-endian on the wire.
        edid.extend_from_slice(&[0x09, 0x5B]);
        // Serial number (0 = absent) plus manufacture week and year.
        edid.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        // EDID version 1.4.
        edid.extend_from_slice(&[1, 4]);
        edid.resize(EDID_BODY_LEN, 0);
        let sum = edid.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
        edid.push(0u8.wrapping_sub(sum));
        edid
    }

    /// The single panel a fixture is expected to produce, or a failed assertion.
    fn only(panels: &[PanelDisplay]) -> &PanelDisplay {
        assert_eq!(panels.len(), 1, "expected exactly one panel");
        let Some(panel) = panels.first() else {
            panic!("expected exactly one panel");
        };
        panel
    }

    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                dir: tempfile::tempdir().expect("tempdir"),
            }
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn backlight(&self, name: &str, kind: &str, max: u32, current: u32) {
            let dir = self.dir.path().join("sys/class/backlight").join(name);
            fs::create_dir_all(&dir).expect("backlight dir");
            fs::write(dir.join("type"), kind).expect("type");
            fs::write(dir.join("max_brightness"), max.to_string()).expect("max");
            fs::write(dir.join("brightness"), current.to_string()).expect("current");
        }

        fn connector(&self, entry: &str, edid: &[u8]) {
            let dir = self.dir.path().join("sys/class/drm").join(entry);
            fs::create_dir_all(&dir).expect("connector dir");
            fs::write(dir.join("status"), "connected\n").expect("status");
            fs::write(dir.join("edid"), edid).expect("edid");
        }
    }

    #[test]
    fn a_laptop_reports_one_panel_identified_by_its_edid() {
        let fixture = Fixture::new();
        fixture.backlight("intel_backlight", "raw", 96_000, 48_000);
        fixture.connector("card0-eDP-1", &edid());

        let panels = enumerate_from(fixture.root());

        let panel = only(&panels);
        assert!(panel.id().as_str().starts_with("GSM-5B09"));
        assert_eq!(panel.instance_name(), "intel_backlight");
        // Sysfs cannot place the panel on the desktop; that is the display
        // server's answer, and `None` means "this backend cannot say".
        assert!(panel.geometry().is_none());
    }

    #[test]
    fn a_desktop_with_no_backlight_reports_nothing() {
        let fixture = Fixture::new();
        fixture.connector("card0-DP-1", &edid());

        assert!(enumerate_from(fixture.root()).is_empty());
    }

    /// A controllable backlight with no identity is not reported, because the
    /// alternative is a fabricated id that collides across machines. This costs
    /// that laptop its panel control, which is why it is a test rather than an
    /// unstated consequence.
    #[test]
    fn a_backlight_with_no_internal_connector_edid_reports_nothing() {
        let fixture = Fixture::new();
        fixture.backlight("intel_backlight", "raw", 100, 50);
        // An external monitor is present and identifiable; the internal
        // connector is not, and it is the internal one that matters.
        fixture.connector("card0-DP-1", &edid());
        fixture.connector("card0-eDP-1", &[]);

        assert!(enumerate_from(fixture.root()).is_empty());
    }

    /// Three backlight devices are three drivers for one screen. Reporting one
    /// row per device would let a user set three brightnesses on one panel, and
    /// the row Duja keeps must be the preferred device rather than whichever the
    /// filesystem listed first.
    #[test]
    fn several_backlight_devices_still_report_exactly_one_panel() {
        let fixture = Fixture::new();
        fixture.backlight("intel_backlight", "raw", 100, 50);
        fixture.backlight("acpi_video0", "firmware", 15, 7);
        fixture.backlight("dell_backlight", "platform", 100, 50);
        fixture.connector("card0-eDP-1", &edid());

        let panels = enumerate_from(fixture.root());

        assert_eq!(only(&panels).instance_name(), "acpi_video0");
    }

    #[test]
    fn opening_a_device_that_has_gone_is_unsupported_not_a_panic() {
        let fixture = Fixture::new();
        fixture.backlight("intel_backlight", "raw", 100, 50);

        assert!(find_device(fixture.root(), "intel_backlight").is_ok());
        assert!(matches!(
            find_device(fixture.root(), "acpi_video0"),
            Err(PanelError::Disconnected)
        ));
    }
}

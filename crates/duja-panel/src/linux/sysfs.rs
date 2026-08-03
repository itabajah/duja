//! The Linux [`PanelTransport`]: read from sysfs, write through logind with a
//! direct sysfs write as the fallback.

use std::fs;

use crate::backlight::{self, Backlight};
use crate::error::PanelError;
use crate::transport::{PanelBrightness, PanelTransport};

use super::logind;

/// Which channel a write goes down, and what is known about it.
///
/// Resolved lazily on the first write rather than at open: connecting to the
/// system bus starts a thread, and a process that only ever *reads* brightness
/// (`dujactl list`, the tray's startup enumeration) should not pay for one.
///
/// Resolved **once**, and deliberately not re-probed. A logind that appears after
/// sysfs has worked is not picked up, which is the right trade: the case exists
/// (a session activating on a VT switch) but re-probing means a connect attempt
/// per slider tick to catch it, and the sysfs write that is already working
/// remains correct.
#[derive(Debug)]
enum Channel {
    /// No write has been attempted yet.
    Unresolved,
    /// logind answered a write; keep using it.
    Logind(logind::Session),
    /// There is no logind, or it refused, and the direct sysfs write works.
    Sysfs,
}

/// Brightness control for one Linux backlight device.
#[derive(Debug)]
pub struct LinuxPanelTransport {
    device: Backlight,
    channel: Channel,
}

impl LinuxPanelTransport {
    /// Bind a transport to an enumerated backlight device.
    #[must_use]
    pub(crate) fn new(device: Backlight) -> Self {
        LinuxPanelTransport {
            device,
            channel: Channel::Unresolved,
        }
    }

    /// Write `raw` straight to `<device>/brightness`.
    fn write_sysfs(&self, raw: u32) -> Result<(), PanelError> {
        fs::write(self.device.dir.join("brightness"), raw.to_string()).map_err(|e| {
            PanelError::Backlight {
                context: "write brightness",
                detail: format!("{}: {e}", self.device.dir.display()),
            }
        })
    }
}

impl PanelTransport for LinuxPanelTransport {
    fn query(&mut self) -> Result<PanelBrightness, PanelError> {
        // Re-read rather than reporting the level captured at enumeration: a
        // brightness key, a power-profile daemon, or a lid-open event all move
        // it, and a stale value would be shown to the user as the truth.
        //
        // `brightness` and not `actual_brightness`: the first is the level the
        // kernel has been asked for, which is what a read-back after a Duja
        // write must return, while the second is a hardware sample that can lag
        // a fade or round differently. A device that has neither is one that has
        // gone.
        let current = backlight::read_level(&self.device.dir).ok_or(PanelError::Disconnected)?;
        Ok(PanelBrightness {
            current: backlight::raw_to_percent(current, self.device.max),
            levels: self.device.levels(),
        })
    }

    fn set_brightness(&mut self, percent: u8) -> Result<(), PanelError> {
        let raw = backlight::percent_to_raw(percent, self.device.max);
        match &self.channel {
            Channel::Logind(session) => {
                // A logind that has worked once and now fails is not a reason to
                // give up on the write: the session may have been deactivated
                // (the user switched to another VT), and the direct write may
                // still be permitted. Fall through rather than reporting a
                // failure the other channel could have avoided.
                if session.set_brightness(&self.device.name, raw).is_ok() {
                    return Ok(());
                }
                self.write_sysfs(raw)
            }
            Channel::Sysfs => self.write_sysfs(raw),
            Channel::Unresolved => {
                if let Some(session) = logind::Session::connect()
                    && session.set_brightness(&self.device.name, raw).is_ok()
                {
                    self.channel = Channel::Logind(session);
                    return Ok(());
                }
                // Either there is no system bus or logind refused. Both are
                // ordinary (a container, an `ssh` session, a machine without
                // systemd), so the fallback is taken silently and remembered.
                //
                // Latched **regardless of whether this write succeeds**, which is
                // the whole point: a machine with no bus and an unwritable
                // `brightness` is exactly the case where staying `Unresolved`
                // would mean a fresh `Connection::system()` — a thread spawn and a
                // SASL handshake — on every slider tick, forever. The write's own
                // error is the thing worth reporting; the channel is settled
                // either way.
                self.channel = Channel::Sysfs;
                self.write_sysfs(raw)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;

    /// A transport over a fixture device, pinned to the sysfs channel so the
    /// test never touches a real system bus.
    fn transport(dir: &Path, max: u32, current: u32) -> LinuxPanelTransport {
        fs::create_dir_all(dir).expect("device dir");
        fs::write(dir.join("type"), "raw").expect("type");
        fs::write(dir.join("max_brightness"), max.to_string()).expect("max");
        fs::write(dir.join("brightness"), current.to_string()).expect("current");
        let device = backlight::scan(
            dir.parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .and_then(Path::parent)
                .expect("fixture root"),
        )
        .into_iter()
        .next()
        .expect("one device");
        LinuxPanelTransport {
            device,
            channel: Channel::Sysfs,
        }
    }

    fn fixture() -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let device = dir.path().join("sys/class/backlight/intel_backlight");
        (dir, device)
    }

    #[test]
    fn a_write_lands_in_the_brightness_file_as_a_raw_level() {
        let (root, device) = fixture();
        let mut transport = transport(&device, 100, 50);

        transport.set_brightness(75).expect("write");

        let written = fs::read_to_string(device.join("brightness")).expect("read back");
        assert_eq!(written, "75");
        drop(root);
    }

    /// The percent the caller set must be the percent it reads back. This is the
    /// pair `raw_to_percent`/`percent_to_raw` guarantee, asserted here through
    /// the real files so a transport that wrote to the wrong attribute, or read
    /// a different one than it wrote, cannot pass.
    #[test]
    fn what_was_written_is_what_is_read_back() {
        let (root, device) = fixture();
        let mut transport = transport(&device, 96_000, 0);

        for percent in [0u8, 1, 33, 50, 99, 100] {
            transport.set_brightness(percent).expect("write");
            assert_eq!(transport.query().expect("query").current, percent);
        }
        drop(root);
    }

    /// A coarse panel reports only the levels it can reach, so the UI does not
    /// promise precision the hardware lacks.
    #[test]
    fn the_reported_levels_come_from_the_hardware_step_count() {
        let (root, device) = fixture();
        let mut transport = transport(&device, 3, 1);

        assert_eq!(transport.query().expect("query").levels, [0, 33, 67, 100]);
        drop(root);
    }

    /// A device that has gone (driver unloaded, hot-removed) must surface as
    /// `Disconnected`, which the controller maps to a terminal error, rather
    /// than as a backend fault it would retry forever.
    #[test]
    fn a_vanished_device_reads_back_as_disconnected() {
        let (root, device) = fixture();
        let mut transport = transport(&device, 100, 50);
        fs::remove_file(device.join("brightness")).expect("remove");

        assert!(matches!(transport.query(), Err(PanelError::Disconnected)));
        drop(root);
    }

    /// An unwritable device must report a backend failure naming the path, not
    /// silently succeed. Simulated by removing the directory, which is the one
    /// way to make the write fail identically on all three CI lanes (a
    /// read-only file is not enough on Windows, and CI runs as root in some
    /// containers, where permissions do not stop a write at all).
    #[test]
    fn a_write_that_cannot_land_is_reported_with_its_path() {
        let (root, device) = fixture();
        let mut transport = transport(&device, 100, 50);
        fs::remove_dir_all(&device).expect("remove");

        let err = transport.set_brightness(50).expect_err("no device");

        assert!(matches!(err, PanelError::Backlight { .. }));
        assert!(err.to_string().contains("write brightness"));
        drop(root);
    }
}

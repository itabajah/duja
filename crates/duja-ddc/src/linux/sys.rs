//! The Linux I2C bus: `/dev/i2c-<N>` plus the one ioctl that binds a file
//! descriptor to the display's DDC/CI slave address.
//!
//! This is the only part of the Linux backend that cannot run on another host,
//! and it is deliberately the smallest part. Everything above it is shared: the
//! packet framing is the cross-platform [`crate::ddcci`] codec (Linux uses the
//! same [`DdcWire::Intel`] framing the Intel Mac path does, because
//! `i2c-dev` carries the slave address out of band exactly as `IOI2CSendRequest`
//! does), and the connector discovery is the root-injected
//! [`duja_core::linux::drm`] scanner.
//!
//! # Why a plain `write`/`read` rather than `I2C_RDWR`
//!
//! `i2c-dev` offers two ways to talk to a device: `I2C_RDWR` with an array of
//! `i2c_msg` structs, or `I2C_SLAVE` once followed by ordinary `write(2)` and
//! `read(2)`. Duja uses the second. DDC/CI is not a register protocol — a
//! request and its reply are two independent transactions separated by a
//! mandatory delay (see [`READ_DELAY`]) — so the combined-transaction form
//! `I2C_RDWR` exists to provide buys nothing here, while the plain form needs no
//! `repr(C)` struct definition and no pointer passed through an ioctl.
//!
//! # Safety policy
//!
//! One `unsafe` call, `libc::ioctl`, carrying its own `// SAFETY:`. Everything
//! else is safe `std::fs`.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::ddcci::{DDC_I2C_ADDRESS, DdcWire, I2cBus};
use crate::transport::TransportError;

/// `I2C_SLAVE`: bind this descriptor to a 7-bit slave address.
///
/// From the kernel's `include/uapi/linux/i2c-dev.h`. Not exported by the `libc`
/// crate, which is why it is spelled out here with its source.
///
/// Typed `libc::Ioctl` rather than `u32` and converted at the call, because that
/// alias is `c_ulong` on glibc and `c_int` on musl and Android — and `i32` has no
/// `From<u32>`, so *any* conversion form compiles on one and not the other. An
/// integer literal infers to either.
const I2C_SLAVE: libc::Ioctl = 0x0703;

/// `I2C_SLAVE_FORCE`: the same, permitted even when a kernel driver already
/// claims the address.
///
/// DDC/CI's `0x37` is claimed on some systems by the `ddcci-backlight` driver
/// (which drives monitors as if they were backlights). [`I2C_SLAVE`] then fails
/// with `EBUSY`, and this is the documented escape hatch.
const I2C_SLAVE_FORCE: libc::Ioctl = 0x0706;

/// Settle time after writing a request before anything else touches the bus.
/// Mirrors the macOS backend's constant of the same name.
const WRITE_SETTLE: Duration = Duration::from_millis(10);

/// Delay between a request and reading its reply.
///
/// VESA DDC/CI specifies a minimum of 40 ms for a display to prepare a reply;
/// 50 ms is the margin `ddcutil` also defaults to. Reading sooner returns a
/// short or empty buffer that the codec rejects as malformed, which the
/// controller then retries — so a too-small value costs latency and looks like
/// flaky hardware.
const READ_DELAY: Duration = Duration::from_millis(50);

/// One display's DDC/CI channel, as an open `/dev/i2c-<N>` already bound to
/// [`DDC_I2C_ADDRESS`].
///
/// Owned by exactly one [`DdcController`](crate::controller::DdcController) on
/// one worker thread; the descriptor closes when this drops.
pub struct LinuxI2cBus {
    device: File,
    /// The device path. Kept because the `Debug` line is the only place a failure
    /// names *which* adapter it was on: `classify` deliberately produces a bare
    /// [`TransportError::Timeout`] rather than a string payload, because a payload
    /// would make it a `Backend` error and `duja-app` reads those as "no
    /// hardware".
    path: PathBuf,
}

/// Written out rather than derived, because the derive would not *read* `path`
/// and the whole reason that field exists is this line: `classify` produces a
/// bare [`TransportError::Timeout`] with no payload (a payload would make it a
/// `Backend` error, which `duja-app` reads as "no hardware"), so the `Debug`
/// rendering is the only place a failure can say which adapter it was on.
impl fmt::Debug for LinuxI2cBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinuxI2cBus")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl LinuxI2cBus {
    /// Open `/dev/i2c-<index>` and bind it to the DDC/CI slave address.
    ///
    /// # Errors
    ///
    /// The `io::Error` the open or the ioctl produced. The common one by far is
    /// `EACCES`, and the usual advice about it is incomplete: `i2c-dev` ships
    /// **no** udev rule of its own, so on a stock system the nodes are
    /// `root:root 0600` and there is no `i2c` group to join. The familiar
    /// `root:i2c 0660` comes from a *package* — `i2c-tools` on Debian, Ubuntu and
    /// Arch, or `ddcutil`'s own rule — which creates the group and the rule
    /// together. So the remedy is "install one of those, then join the group",
    /// not "join the group". `dujactl doctor` says so, which is why this returns
    /// the error rather than swallowing it into "no monitors".
    pub(crate) fn open(index: u32) -> Result<Self, io::Error> {
        let path = PathBuf::from(format!("/dev/i2c-{index}"));
        let device = OpenOptions::new().read(true).write(true).open(&path)?;
        bind_slave_address(&device)?;
        Ok(LinuxI2cBus { device, path })
    }
}

/// The framing this backend puts on the wire.
///
/// A named constant rather than a literal in `wire()` so a test can pin it. That
/// is not ceremony: the P6 gate found a macOS backend where **every** request was
/// malformed and the suite stayed green, and the framing choice is the one bit of
/// this file that is wrong-or-right rather than present-or-absent.
const WIRE: DdcWire = DdcWire::Intel;

/// The order the address-binding ioctls are tried in.
///
/// [`I2C_SLAVE`] first and [`I2C_SLAVE_FORCE`] only as a fallback, because
/// forcing means **sharing** `0x37` with whatever already holds it — in practice
/// `ddcci-backlight`, which drives monitors as backlights and will interleave its
/// own transactions with Duja's on the same bus. The reference implementation
/// (`ddc-i2c`) never forces at all. Duja does, because refusing outright would
/// leave a user with that module loaded unable to control any monitor; the order
/// is the whole safeguard, so it is pinned by a test.
const BIND_ORDER: [libc::Ioctl; 2] = [I2C_SLAVE, I2C_SLAVE_FORCE];

/// Classify an I2C I/O failure for the controller.
///
/// **Nothing here is `Backend`, and that is the point.** `duja-app`'s worker
/// treats an opaque `Backend` error from a write as a *positive* "this display has
/// no hardware brightness" signal and latches a permanent software-only downgrade
/// (`is_no_hardware_error`). On Linux that is fatal rather than merely wrong: a
/// Linux display reports no bounds, so there is no overlay to fall back **to**.
/// A flaky bus must not be able to say "no hardware".
///
/// `Disconnected` is equally narrow, for the opposite reason: it is terminal and
/// stops the controller retrying, so it is reserved for the device node genuinely
/// going away. In particular **`ENXIO` is not disconnection** — it is the errno an
/// I2C adapter returns for an address NAK, which is what a monitor in DPMS
/// standby, a monitor with DDC/CI switched off in its OSD, and a monitor still
/// busy from the previous command all produce; `i2cdetect` uses exactly that
/// errno to decide an address is empty. Reporting it as terminal would tell a
/// user their plugged-in, sleeping monitor is disconnected.
///
/// Everything else is [`TransportError::Timeout`], the transient the controller
/// is built to retry — the same choice the macOS bus makes for every non-success
/// `IOReturn`. A display with nothing to say never reaches here at all: it
/// answers the `read(2)` successfully with filler that the codec rejects as
/// malformed.
fn classify(err: &io::Error) -> TransportError {
    match err.raw_os_error() {
        // The device node itself is gone: the adapter left with the GPU, or
        // `/dev/i2c-*` was unlinked under us.
        Some(libc::ENODEV | libc::ENOENT) => TransportError::Disconnected,
        _ => TransportError::Timeout,
    }
}

/// Bind `device` to [`DDC_I2C_ADDRESS`], falling back to the forcing variant
/// when a kernel driver already holds it.
fn bind_slave_address(device: &File) -> Result<(), io::Error> {
    let mut last = Ok(());
    for request in BIND_ORDER {
        last = set_address(device, request);
        if last.is_ok() {
            return Ok(());
        }
    }
    // Only `EBUSY` distinguishes "claimed by a driver" from the rest; a bad
    // descriptor or an adapter that does not support the ioctl fails identically
    // both times, so reporting the last error loses nothing.
    last
}

/// Issue one address-binding ioctl.
fn set_address(device: &File, request: libc::Ioctl) -> Result<(), io::Error> {
    // SAFETY: `I2C_SLAVE`/`I2C_SLAVE_FORCE` take their argument by value rather
    // than by pointer, so no memory is read or written by the kernel and the
    // only requirement is a valid descriptor — guaranteed by the `&File` borrow,
    // which also keeps it open for the duration of the call. The address is a
    // 7-bit constant well inside the range the ioctl accepts.
    let rc = unsafe {
        libc::ioctl(
            device.as_raw_fd(),
            request,
            libc::c_ulong::from(DDC_I2C_ADDRESS),
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl I2cBus for LinuxI2cBus {
    fn wire(&self) -> DdcWire {
        // `i2c-dev` carries the slave address out of band, exactly as Intel
        // macOS's `IOI2CSendRequest` does, so the packet on the wire is the
        // standard MCCS framing including its `0x51` host source byte.
        WIRE
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        // One `write(2)` is one I2C transaction: a partial write would put half a
        // DDC/CI packet on the bus, so `write_all`'s retry loop would be actively
        // wrong here. `i2cdev_write` returns `i2c_master_send`, which answers the
        // full count or a negative errno and never a partial one, so the length
        // check below is unreachable defence rather than a case.
        let written = (&self.device).write(data).map_err(|e| classify(&e))?;
        if written != data.len() {
            // Not `Backend`: see `classify`. A partial write is a transient as far
            // as the controller is concerned, and it must not be able to latch a
            // software-only downgrade.
            return Err(TransportError::Timeout);
        }
        thread::sleep(WRITE_SETTLE);
        Ok(())
    }

    fn read(&mut self, len: usize) -> Result<Vec<u8>, TransportError> {
        thread::sleep(READ_DELAY);
        let mut buf = vec![0u8; len];
        // Same reasoning as `write`: one `read(2)`, one I2C transaction, and
        // `i2cdev_read` returns `i2c_master_recv`, which is likewise all-or-error.
        // A display with less to say does not short-read — the adapter clocks the
        // full count and the display sends filler, which `parse_reply` skips past
        // by locating the frame from its own length byte. The truncate is
        // belt-and-braces for an adapter that does something else.
        let read = (&self.device).read(&mut buf).map_err(|e| classify(&e))?;
        buf.truncate(read);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reply delay is a protocol minimum, not a tuning knob: below 40 ms the
    /// display is still preparing its answer and every read returns a frame the
    /// codec rejects, which the controller then retries — flaky hardware from
    /// the outside, a constant from the inside.
    #[test]
    fn the_reply_delay_clears_the_protocol_minimum() {
        assert!(READ_DELAY >= Duration::from_millis(40));
        assert!(WRITE_SETTLE < READ_DELAY);
    }

    /// Spelled out from `i2c-dev.h` because `libc` does not export them; a
    /// transposed digit here would bind the wrong ioctl and fail at runtime on
    /// hardware CI does not have.
    #[test]
    fn the_ioctl_numbers_match_the_kernel_header() {
        assert_eq!(I2C_SLAVE, 0x0703);
        assert_eq!(I2C_SLAVE_FORCE, 0x0706);
    }

    /// Forcing shares `0x37` with whatever already holds it, so the polite ioctl
    /// must be tried first. The reference implementation never forces at all;
    /// Duja does, and the order is the entire safeguard, so "always force"
    /// must not be able to pass.
    #[test]
    fn the_polite_bind_is_tried_before_the_forcing_one() {
        assert_eq!(BIND_ORDER, [I2C_SLAVE, I2C_SLAVE_FORCE]);
    }

    /// The framing is the one bit of this file that is wrong-or-right rather than
    /// present-or-absent, and the P6 gate proved that a wrong one leaves the
    /// suite green. Pinned as bytes, not as an enum variant, so the assertion
    /// fails for the reason a monitor would: `i2c-dev` carries the slave address
    /// out of band, so the packet must still carry its `0x51` host source byte.
    #[test]
    fn the_wire_is_the_framing_that_keeps_the_host_source_byte() {
        assert_eq!(WIRE, DdcWire::Intel);
        // Get VCP 0x10 (brightness), as `ddcutil` and `ddc-i2c` put it on a
        // Linux bus: source, length|0x80, opcode, feature, checksum.
        assert_eq!(WIRE.encode_get_vcp(0x10), [0x51, 0x82, 0x01, 0x10, 0xAC]);
    }

    /// `ENXIO` is an address NAK, which a monitor in standby or with DDC/CI off
    /// in its OSD produces on every request. Classifying it as `Disconnected`
    /// would report a plugged-in display as gone and stop the controller
    /// retrying it.
    #[test]
    fn a_nak_is_a_transient_and_not_a_disconnection() {
        assert!(matches!(
            classify(&io::Error::from_raw_os_error(libc::ENXIO)),
            TransportError::Timeout
        ));
    }

    /// Only the device node going away is terminal.
    #[test]
    fn a_vanished_device_node_is_terminal() {
        for errno in [libc::ENODEV, libc::ENOENT] {
            assert!(
                matches!(
                    classify(&io::Error::from_raw_os_error(errno)),
                    TransportError::Disconnected
                ),
                "errno {errno}"
            );
        }
    }

    /// **No I2C failure may be a `Backend` error.** `duja-app` reads an opaque
    /// `Backend` from a write as a positive "this display has no hardware
    /// brightness" and latches a permanent software-only downgrade — which on
    /// Linux is fatal, because a Linux display has no bounds and so no overlay to
    /// fall back to. This is the assertion that keeps a flaky bus from being able
    /// to say "no hardware".
    #[test]
    fn no_bus_failure_can_claim_the_display_has_no_hardware() {
        let errnos = [
            libc::EIO,
            libc::ENXIO,
            libc::EBUSY,
            libc::EAGAIN,
            libc::EINVAL,
            libc::EACCES,
            libc::ETIMEDOUT,
            libc::EREMOTEIO,
            libc::ENODEV,
            libc::ENOENT,
        ];
        for errno in errnos {
            let classified = classify(&io::Error::from_raw_os_error(errno));
            assert!(
                !matches!(classified, TransportError::Backend(_)),
                "errno {errno} became a Backend error: {classified:?}"
            );
        }
    }
}

//! Pure classification of the two Linux event sources into [`PlatformEvent`]s.
//!
//! The Linux pump has two inputs and neither of them is an OS *API* in the sense
//! the Windows and macOS pumps use one:
//!
//! - **Display hot-plug** arrives as a `NETLINK_KOBJECT_UEVENT` datagram: a blob
//!   of NUL-separated `KEY=VALUE` lines the kernel broadcasts on every device
//!   change in the system. Duja wants a handful of them and must ignore the
//!   rest, which on a busy machine is nearly all of them.
//! - **Suspend and resume** arrive as one logind D-Bus signal, `PrepareForSleep`,
//!   carrying a single boolean.
//!
//! Both classifications are total functions over plain data — bytes in, an
//! `Option<PlatformEvent>` out — so this module is
//! `cfg(any(test, target_os = "linux"))` and its rules run on **every** CI lane,
//! in the pattern `mac_events` established (named in backticks, not linked: that
//! module is `cfg(any(test, target_os = "macos"))`, so an intra-doc link to it
//! fails `rustdoc -D warnings` on the very Linux lane this module ships for). That matters more here than
//! it did there: a GitHub runner has no `drm` device to unplug, so the socket
//! itself can never be exercised in CI, and this is the only part of hot-plug
//! detection any test can reach.

use crate::PlatformEvent;

/// The uevent subsystem that carries display topology changes.
const SUBSYSTEM_DRM: &str = "drm";

/// Classify one raw `NETLINK_KOBJECT_UEVENT` datagram.
///
/// Returns [`PlatformEvent::DisplaysChanged`] for a `drm` event that plausibly
/// changed the display set, and `None` for everything else — which is the
/// overwhelming majority, since this socket carries every device event on the
/// machine (USB, block devices, network, power supply, input).
///
/// # What counts as a display change
///
/// A `drm` `change` event is the connector hot-plug notification: the kernel
/// re-probes and tells userspace to re-read the connector's `status`. `add` and
/// `remove` are the card itself appearing or going away, which also changes the
/// set. Everything else on the `drm` subsystem (`bind`, `unbind`, `move`) is
/// passed over: it is a driver-lifecycle event, and treating it as a topology
/// change would re-enumerate on module load for no benefit.
///
/// The pump does **no** debouncing, exactly as on the other two platforms — a
/// single plug produces a burst — and `duja_core`'s `Debouncer` collapses it.
pub(crate) fn classify_uevent(datagram: &[u8]) -> Option<PlatformEvent> {
    let mut subsystem = None;
    let mut action = None;
    for (key, value) in fields(datagram) {
        match key {
            "SUBSYSTEM" => subsystem = Some(value),
            "ACTION" => action = Some(value),
            _ => {}
        }
    }
    if subsystem? != SUBSYSTEM_DRM {
        return None;
    }
    match action? {
        "change" | "add" | "remove" => Some(PlatformEvent::DisplaysChanged),
        _ => None,
    }
}

/// Split a uevent datagram into its `KEY=VALUE` pairs.
///
/// The wire format is a header line (`change@/devices/…`, which carries no `=`
/// and is skipped for free) followed by NUL-separated `KEY=VALUE` entries.
/// Non-UTF-8 fields are dropped rather than lossily converted: every key this
/// module reads is ASCII by the kernel's own construction, so a field that is
/// not valid UTF-8 cannot be one of them, and inventing a replacement character
/// could only ever create a false match.
fn fields(datagram: &[u8]) -> impl Iterator<Item = (&str, &str)> {
    datagram
        .split(|b| *b == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter_map(|entry| entry.split_once('='))
}

/// Classify logind's `PrepareForSleep` signal payload.
///
/// The boolean is `true` when the system is *about to* suspend and `false` when
/// it has resumed, so the two map onto [`PlatformEvent::Suspending`] and
/// [`PlatformEvent::Resumed`]. Total, and deliberately not an `Option`: this
/// signal has exactly two meanings and no third.
///
/// Note the asymmetry the name hides. The `true` arrives *before* the machine
/// goes down and logind waits for delay-inhibitor holders, so a handler has real
/// time to act; the `false` arrives after everything is already back. Duja takes
/// no inhibitor lock, so its `Suspending` is a notification rather than a
/// window — the same contract the Windows and macOS pumps offer.
pub(crate) fn classify_prepare_for_sleep(going_to_sleep: bool) -> PlatformEvent {
    if going_to_sleep {
        PlatformEvent::Suspending
    } else {
        PlatformEvent::Resumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a uevent datagram in the kernel's wire format: a header line, then
    /// NUL-separated `KEY=VALUE` entries, with a trailing NUL.
    fn datagram(header: &str, fields: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(header.as_bytes());
        out.push(0);
        for field in fields {
            out.extend_from_slice(field.as_bytes());
            out.push(0);
        }
        out
    }

    /// A real-shaped connector hot-plug event, as the kernel emits it when a
    /// monitor is plugged into or unplugged from a running system.
    fn drm_hotplug() -> Vec<u8> {
        datagram(
            "change@/devices/pci0000:00/0000:00:02.0/drm/card0",
            &[
                "ACTION=change",
                "DEVPATH=/devices/pci0000:00/0000:00:02.0/drm/card0",
                "SUBSYSTEM=drm",
                "HOTPLUG=1",
                "CONNECTOR=95",
                "DEVNAME=dri/card0",
                "DEVTYPE=drm_minor",
                "SEQNUM=4821",
            ],
        )
    }

    #[test]
    fn a_drm_hotplug_is_a_display_change() {
        assert_eq!(
            classify_uevent(&drm_hotplug()),
            Some(PlatformEvent::DisplaysChanged)
        );
    }

    #[test]
    fn a_card_appearing_or_going_away_is_also_a_display_change() {
        for action in ["add", "remove"] {
            let raw = datagram(
                "add@/devices/pci0000:00/0000:00:02.0/drm/card1",
                &[&format!("ACTION={action}"), "SUBSYSTEM=drm"],
            );
            assert_eq!(
                classify_uevent(&raw),
                Some(PlatformEvent::DisplaysChanged),
                "drm {action}"
            );
        }
    }

    /// This socket carries every device event on the machine. Duja re-enumerates
    /// every monitor on a `DisplaysChanged`, which costs DDC traffic, so a
    /// classifier that let the rest through would put the whole system's device
    /// churn on the I2C bus.
    #[test]
    fn events_from_other_subsystems_are_ignored() {
        for subsystem in ["usb", "block", "net", "power_supply", "input", "hidraw"] {
            let raw = datagram(
                "add@/devices/whatever",
                &["ACTION=add", &format!("SUBSYSTEM={subsystem}")],
            );
            assert_eq!(classify_uevent(&raw), None, "subsystem {subsystem}");
        }
    }

    /// `bind` and `unbind` fire when a driver attaches to a device, which happens
    /// on module load and on resume. They do not change the display set, and
    /// re-enumerating there would cost DDC traffic for nothing.
    #[test]
    fn driver_lifecycle_events_on_drm_are_not_topology_changes() {
        for action in ["bind", "unbind", "move", "online", "offline"] {
            let raw = datagram(
                "bind@/devices/pci0000:00/0000:00:02.0/drm/card0",
                &[&format!("ACTION={action}"), "SUBSYSTEM=drm"],
            );
            assert_eq!(classify_uevent(&raw), None, "drm {action}");
        }
    }

    /// The socket is a firehose of other processes' traffic and the kernel is not
    /// the only thing that can write to it (`udev` broadcasts its own on a
    /// different group). Nothing here may panic or index out of bounds on a
    /// datagram that is empty, truncated mid-field, or not text at all.
    #[test]
    fn malformed_datagrams_are_ignored_rather_than_fatal() {
        assert_eq!(classify_uevent(&[]), None);
        assert_eq!(classify_uevent(&[0]), None);
        assert_eq!(classify_uevent(b"change@/devices/x"), None);
        // A header with no fields at all.
        assert_eq!(classify_uevent(&datagram("change@/devices/x", &[])), None);
        // Keys with no value, and values with no key.
        assert_eq!(
            classify_uevent(&datagram("x", &["SUBSYSTEM", "=drm", "ACTION"])),
            None
        );
        // Invalid UTF-8 in a field must not stop the valid fields being read, and
        // must not itself match anything.
        let mut raw = datagram("change@/devices/x", &["ACTION=change"]);
        raw.extend_from_slice(&[0xFF, 0xFE, 0x00]);
        raw.extend_from_slice(b"SUBSYSTEM=drm\0");
        assert_eq!(classify_uevent(&raw), Some(PlatformEvent::DisplaysChanged));
    }

    /// A datagram missing either key it needs is not a display change. Asserted
    /// separately from the malformed case because this one is well-formed: it is
    /// what an event from a subsystem that omits `ACTION` looks like, and reading
    /// an absent key as a match would fire on it.
    #[test]
    fn both_keys_are_required() {
        assert_eq!(
            classify_uevent(&datagram("change@/x", &["SUBSYSTEM=drm"])),
            None,
            "no ACTION"
        );
        assert_eq!(
            classify_uevent(&datagram("change@/x", &["ACTION=change"])),
            None,
            "no SUBSYSTEM"
        );
    }

    /// A later `SUBSYSTEM` must not be shadowed by an earlier unrelated key, and
    /// field order is not guaranteed by the kernel.
    #[test]
    fn field_order_does_not_matter() {
        let raw = datagram(
            "change@/x",
            &[
                "SEQNUM=1",
                "SUBSYSTEM=drm",
                "DEVTYPE=drm_minor",
                "ACTION=change",
            ],
        );
        assert_eq!(classify_uevent(&raw), Some(PlatformEvent::DisplaysChanged));
    }

    /// The boolean's polarity is the whole content of this mapping, and getting
    /// it backwards would park state on resume and re-apply it on the way down —
    /// silently, since both values are legitimate events.
    #[test]
    fn prepare_for_sleep_true_is_going_down_and_false_is_coming_back() {
        assert_eq!(classify_prepare_for_sleep(true), PlatformEvent::Suspending);
        assert_eq!(classify_prepare_for_sleep(false), PlatformEvent::Resumed);
    }
}

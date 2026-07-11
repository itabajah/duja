//! Pure translation of macOS raw notification codes into [`PlatformEvent`]s.
//!
//! The [`mac`](crate::mac) event pump cannot be exercised without real display
//! and power hardware (and even a macOS CI runner cannot script a display
//! reconfiguration), so the *decision* logic — "given these raw flags, which
//! event, if any, do we emit?" — is kept here as small, side-effect-free
//! functions with plain integer inputs. That lets the mapping be unit-tested on
//! **every** host (Windows/Linux included), independent of the FFI in
//! [`mac::sys`](crate::mac::sys) that feeds it.
//!
//! The constants mirror the C headers:
//! - display flags: `CGDisplayChangeSummaryFlags` (`CGDisplayConfiguration.h`);
//! - power messages: `kIOMessage*` (`IOKit/IOMessage.h`), where
//!   `sys_iokit | sub_iokit_common | code` resolves to `0xe000_0000 | code`.
//!
//! This module is compiled on macOS (where [`mac::sys`](crate::mac::sys) calls
//! it) and, under `cfg(test)`, on every host (so the tests below run in the
//! ordinary `cargo test`).

use crate::PlatformEvent;

// -- CGDisplayChangeSummaryFlags ------------------------------------------

/// Pre-notification: the callback fires once with this flag *before* a
/// reconfiguration, then again *after* with the real change flags. A callback
/// carrying only this bit is the "about to change" phase and maps to nothing.
// RATIONALE (dead_code): documents the pre-notification bit for completeness and
// is exercised by the unit tests; it is intentionally excluded from the
// `CG_MEANINGFUL` mask, so the non-test build never references it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const CG_DISPLAY_BEGIN_CONFIGURATION_FLAG: u32 = 1 << 0;
/// A display's origin moved in the global display space.
pub(crate) const CG_DISPLAY_MOVED_FLAG: u32 = 1 << 1;
/// A display became the main display.
pub(crate) const CG_DISPLAY_SET_MAIN_FLAG: u32 = 1 << 2;
/// A display's mode (resolution/refresh) changed.
pub(crate) const CG_DISPLAY_SET_MODE_FLAG: u32 = 1 << 3;
/// A display was added (connected / enabled).
pub(crate) const CG_DISPLAY_ADD_FLAG: u32 = 1 << 4;
/// A display was removed (disconnected / disabled).
pub(crate) const CG_DISPLAY_REMOVE_FLAG: u32 = 1 << 5;
/// A display was enabled.
pub(crate) const CG_DISPLAY_ENABLED_FLAG: u32 = 1 << 8;
/// A display was disabled.
pub(crate) const CG_DISPLAY_DISABLED_FLAG: u32 = 1 << 9;
/// A display began mirroring another.
pub(crate) const CG_DISPLAY_MIRROR_FLAG: u32 = 1 << 10;
/// A display stopped mirroring another.
pub(crate) const CG_DISPLAY_UNMIRROR_FLAG: u32 = 1 << 11;
/// The union of the desktop shape changed.
pub(crate) const CG_DISPLAY_DESKTOP_SHAPE_CHANGED_FLAG: u32 = 1 << 12;

/// Every flag that represents a *completed* topology change worth re-enumerating
/// for. Deliberately excludes [`CG_DISPLAY_BEGIN_CONFIGURATION_FLAG`] (the
/// pre-notification phase carries no actionable change on its own).
const CG_MEANINGFUL: u32 = CG_DISPLAY_MOVED_FLAG
    | CG_DISPLAY_SET_MAIN_FLAG
    | CG_DISPLAY_SET_MODE_FLAG
    | CG_DISPLAY_ADD_FLAG
    | CG_DISPLAY_REMOVE_FLAG
    | CG_DISPLAY_ENABLED_FLAG
    | CG_DISPLAY_DISABLED_FLAG
    | CG_DISPLAY_MIRROR_FLAG
    | CG_DISPLAY_UNMIRROR_FLAG
    | CG_DISPLAY_DESKTOP_SHAPE_CHANGED_FLAG;

/// Map a `CGDisplayChangeSummaryFlags` value to an event.
///
/// Returns `Some(DisplaysChanged)` for any completed topology change and `None`
/// for the pre-notification phase (`begin`-only) or an empty/unknown flag set.
/// Like the Windows `WM_DISPLAYCHANGE` handler this is bursty; the consumer
/// debounces, so an occasional extra emission is harmless.
pub(crate) fn map_display_flags(flags: u32) -> Option<PlatformEvent> {
    if flags & CG_MEANINGFUL != 0 {
        Some(PlatformEvent::DisplaysChanged)
    } else {
        None
    }
}

// -- IOKit system-power messages ------------------------------------------

/// Idle sleep is being *considered*; a client must acknowledge (allow or veto)
/// or the system waits. We always allow, but emit nothing yet.
pub(crate) const IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
/// The system *will* sleep (point of no return); acknowledge, then park state.
pub(crate) const IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
/// The system has fully powered on after a wake; re-apply state.
pub(crate) const IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

/// Map an `IOKit` power `messageType` to an event.
///
/// `WILL_SLEEP` → [`PlatformEvent::Suspending`]; `HAS_POWERED_ON` →
/// [`PlatformEvent::Resumed`]. Every other message (including the
/// `CAN_SYSTEM_SLEEP` query and the early `WILL_POWER_ON`) maps to `None`.
pub(crate) fn map_power_message(message_type: u32) -> Option<PlatformEvent> {
    match message_type {
        IO_MESSAGE_SYSTEM_WILL_SLEEP => Some(PlatformEvent::Suspending),
        IO_MESSAGE_SYSTEM_HAS_POWERED_ON => Some(PlatformEvent::Resumed),
        _ => None,
    }
}

/// Whether a power `messageType` demands an `IOAllowPowerChange` acknowledgement.
///
/// Both the `CAN_SYSTEM_SLEEP` query and the `WILL_SLEEP` point-of-no-return
/// must be acknowledged or the system stalls for ~30 s waiting on us; wake
/// messages must not be acknowledged.
pub(crate) fn power_message_needs_ack(message_type: u32) -> bool {
    matches!(
        message_type,
        IO_MESSAGE_CAN_SYSTEM_SLEEP | IO_MESSAGE_SYSTEM_WILL_SLEEP
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_configuration_alone_maps_to_nothing() {
        // The "about to reconfigure" pre-notification carries no actionable
        // change on its own.
        assert_eq!(map_display_flags(CG_DISPLAY_BEGIN_CONFIGURATION_FLAG), None);
    }

    #[test]
    fn empty_flags_map_to_nothing() {
        assert_eq!(map_display_flags(0), None);
    }

    #[test]
    fn add_and_remove_map_to_displays_changed() {
        assert_eq!(
            map_display_flags(CG_DISPLAY_ADD_FLAG),
            Some(PlatformEvent::DisplaysChanged)
        );
        assert_eq!(
            map_display_flags(CG_DISPLAY_REMOVE_FLAG),
            Some(PlatformEvent::DisplaysChanged)
        );
    }

    #[test]
    fn mode_and_geometry_changes_map_to_displays_changed() {
        for flag in [
            CG_DISPLAY_MOVED_FLAG,
            CG_DISPLAY_SET_MAIN_FLAG,
            CG_DISPLAY_SET_MODE_FLAG,
            CG_DISPLAY_ENABLED_FLAG,
            CG_DISPLAY_DISABLED_FLAG,
            CG_DISPLAY_MIRROR_FLAG,
            CG_DISPLAY_UNMIRROR_FLAG,
            CG_DISPLAY_DESKTOP_SHAPE_CHANGED_FLAG,
        ] {
            assert_eq!(
                map_display_flags(flag),
                Some(PlatformEvent::DisplaysChanged),
                "flag {flag:#x} should emit DisplaysChanged"
            );
        }
    }

    #[test]
    fn begin_plus_real_change_still_emits() {
        // The post-reconfiguration callback can carry begin|add together; the
        // real change bit must win.
        let flags = CG_DISPLAY_BEGIN_CONFIGURATION_FLAG | CG_DISPLAY_ADD_FLAG;
        assert_eq!(
            map_display_flags(flags),
            Some(PlatformEvent::DisplaysChanged)
        );
    }

    #[test]
    fn will_sleep_maps_to_suspending() {
        assert_eq!(
            map_power_message(IO_MESSAGE_SYSTEM_WILL_SLEEP),
            Some(PlatformEvent::Suspending)
        );
    }

    #[test]
    fn powered_on_maps_to_resumed() {
        assert_eq!(
            map_power_message(IO_MESSAGE_SYSTEM_HAS_POWERED_ON),
            Some(PlatformEvent::Resumed)
        );
    }

    #[test]
    fn can_sleep_query_emits_nothing_but_needs_ack() {
        assert_eq!(map_power_message(IO_MESSAGE_CAN_SYSTEM_SLEEP), None);
        assert!(power_message_needs_ack(IO_MESSAGE_CAN_SYSTEM_SLEEP));
    }

    #[test]
    fn will_sleep_needs_ack_wake_does_not() {
        assert!(power_message_needs_ack(IO_MESSAGE_SYSTEM_WILL_SLEEP));
        assert!(!power_message_needs_ack(IO_MESSAGE_SYSTEM_HAS_POWERED_ON));
        assert!(!power_message_needs_ack(IO_MESSAGE_CAN_SYSTEM_SLEEP + 1));
    }

    #[test]
    fn unknown_power_message_is_ignored() {
        assert_eq!(map_power_message(0), None);
        assert!(!power_message_needs_ack(0));
    }
}

//! OS integration: display reconfiguration events, power/session events,
//! single-instance enforcement, autostart registration.
//!
//! Event sources (P3+): Windows — hidden **top-level** window (message-only
//! `HWND`s do not receive `WM_DISPLAYCHANGE`) + `RegisterDeviceNotification` +
//! `WM_POWERBROADCAST` + session unlock; macOS —
//! `CGDisplayRegisterReconfigurationCallback`; Linux — udev `drm` monitor.
//! All sources emit normalized events into `duja_core`'s manager.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

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
}

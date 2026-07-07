//! Internal laptop-panel brightness control.
//!
//! DDC/CI cannot reach internal panels; each OS has a distinct native API:
//! Windows `WmiMonitorBrightnessMethods` (root\wmi), macOS private
//! `DisplayServicesSetBrightness` (dlopen'd, graceful fallback), Linux
//! logind D-Bus `SetBrightness` with a `/sys/class/backlight` write fallback.
//! Backends implement `duja_core`'s `BrightnessController` trait. (P3+.)

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

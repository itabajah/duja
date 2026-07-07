//! Slint UI components and their view-models.
//!
//! **Hard boundary (test architecture):** all UI logic lives in plain-Rust
//! view-models (`FlyoutVm`, `SettingsVm`) — display snapshots in, commands
//! out, zero Slint types in their signatures — so the logic is fully
//! unit-testable. `.slint` files are a thin rendering skin. (P4+.)
//!
//! Idle budget rule: zero Slint timers/animations while the flyout is hidden.

#![forbid(unsafe_code)]
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

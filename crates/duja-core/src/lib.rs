//! Duja core domain logic.
//!
//! This crate is **pure**: no OS APIs, no I/O, no `unsafe`. Everything here is
//! unit-testable and platform-independent. OS backends implement the traits
//! defined here; the UI consumes the models defined here.
//!
//! Module map (built test-first in phase P2 — see the project plan):
//! - `id` — stable EDID-derived display identity
//! - `model` — `Display`, capabilities, features
//! - `controller` — the `BrightnessController` trait every backend implements
//! - `manager` — `DisplayManager`: enumeration diffing, state, restore
//! - `continuum` — one user slider mapped onto hardware + software dimming
//! - `debounce` — pure debounce/coalesce state machines
//! - `sync` — multi-monitor sync groups
//! - `config` — versioned, migratable, atomically-persisted configuration
//! - `quirks` — per-monitor quirk database and matching
//! - `caps` — MCCS capability-string parser

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod controller;
pub mod id;
pub mod model;

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_workspace_package() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }
}

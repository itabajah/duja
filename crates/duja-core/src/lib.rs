//! Duja core domain logic.
//!
//! This crate is **pure**: no OS APIs, no I/O, no `unsafe`. Everything here is
//! unit-testable and platform-independent. OS backends implement the traits
//! defined here; the UI consumes the models defined here.
//!
//! # Module map
//!
//! Implemented (first wave, built test-first in phase P2):
//! - [`id`] — stable EDID-derived display identity ([`id::StableDisplayId`])
//! - [`model`] — features, capabilities, and the UI-facing
//!   [`model::DisplaySnapshot`]
//! - [`controller`] — the [`controller::BrightnessController`] trait every
//!   backend implements, and [`controller::ControlError`]
//! - [`continuum`] — one user slider mapped onto hardware + software dimming
//! - [`debounce`] — pure debounce / coalesce state machines
//! - [`caps`] — total MCCS capability-string parser ([`caps::ParsedCaps`])
//! - [`quirks`] — quirk database + stable-id matcher ([`quirks::QuirkDb`])
//! - `testing` (feature `test-support`) — fakes + the controller contract suite
//!
//! Planned (later waves): `manager` (enumeration diffing, state, restore),
//! `sync` (multi-monitor groups), `config`.
//!
//! # Example
//!
//! ```
//! use duja_core::continuum::{map_user_level, ContinuumConfig};
//! use duja_core::model::DimMode;
//!
//! // A display that dims via overlay below a 30% hardware floor.
//! let cfg = ContinuumConfig::hardware(30, DimMode::Overlay);
//!
//! // Above the floor, the slider drives the hardware directly.
//! assert_eq!(map_user_level(70, &cfg).hardware_pct, Some(70));
//!
//! // Below it, hardware pins at the floor and the overlay engages.
//! let dim = map_user_level(15, &cfg);
//! assert_eq!(dim.hardware_pct, Some(30));
//! assert!(dim.overlay_alpha > 0.0);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod caps;
pub mod continuum;
pub mod controller;
pub mod debounce;
pub mod id;
pub mod model;
pub mod quirks;

/// Deterministic fakes and the reusable controller contract suite.
///
/// Available for this crate's own tests, and for downstream crates via the
/// `test-support` feature. Never part of a release build.
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

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

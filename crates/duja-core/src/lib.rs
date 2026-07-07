//! Duja core domain logic.
//!
//! This crate is **pure**: no OS-specific APIs and no `unsafe`. The only
//! filesystem I/O lives in [`config::persist`], the crash-safe reader/writer
//! behind the config and state files. Everything else is unit-testable and
//! platform-independent. OS backends implement the traits defined here; the UI
//! consumes the models defined here.
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
//! - [`manager`] — hot-plug enumeration diffing, per-display state and level
//!   restore ([`manager::DisplayManager`])
//! - [`sync`] — multi-monitor sync groups with per-member offsets
//!   ([`sync::SyncGroups`])
//! - [`config`] — typed config schema, format-preserving TOML document,
//!   chained migrations, and crash-safe atomic persistence (the only I/O)
//! - `testing` (feature `test-support`) — fakes + the controller contract suite
//!
//! Planned (later waves): `quirks`, `caps` (MCCS parser).
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

pub mod config;
pub mod continuum;
pub mod controller;
pub mod debounce;
pub mod id;
pub mod manager;
pub mod model;
pub mod sync;

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

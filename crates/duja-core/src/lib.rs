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
//! - [`dimmer`] — the cross-platform software-dimming vocabulary
//!   ([`dimmer::DimCommand`], [`dimmer::Dimmer`]); the Windows overlay backend
//!   that implements it lives in the `duja-dimmer` crate
//! - [`debounce`] — pure debounce / coalesce state machines
//! - [`manager`] — hot-plug enumeration diffing, per-display state and level
//!   restore ([`manager::DisplayManager`])
//! - [`sync`] — multi-monitor sync groups with per-member offsets
//!   ([`sync::SyncGroups`])
//! - [`config`] — typed config schema, format-preserving TOML document,
//!   chained migrations, and crash-safe atomic persistence (the only I/O)
//! - [`caps`] — total MCCS capability-string parser ([`caps::ParsedCaps`])
//! - [`input_source`] — MCCS VCP `0x60` input-source code ↔ name mapping
//! - [`quirks`] — quirk database + stable-id matcher ([`quirks::QuirkDb`])
//! - `testing` (feature `test-support`) — fakes + the controller contract suite
//!
//! # Example
//!
//! ```
//! use duja_core::continuum::{map_user_level, ContinuumConfig};
//! use duja_core::model::DimMode;
//!
//! // Perceptual anchor 25 (hardware zero looks ~25% bright), no hardware floor:
//! // the slider position *is* perceived brightness.
//! let cfg = ContinuumConfig::hardware(0, 25, DimMode::Overlay);
//!
//! // The top of the slider is always full hardware.
//! assert_eq!(map_user_level(100, &cfg).hardware_pct, Some(100));
//!
//! // Below the anchor the panel sits at hardware zero and the overlay dims.
//! let dim = map_user_level(10, &cfg);
//! assert_eq!(dim.hardware_pct, Some(0));
//! assert!(dim.overlay_alpha > 0.0);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod caps;
pub mod config;
pub mod continuum;
pub mod controller;
pub mod debounce;
pub mod dimmer;
pub mod id;
pub mod input_source;
pub mod manager;
pub mod model;
pub mod quirks;
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

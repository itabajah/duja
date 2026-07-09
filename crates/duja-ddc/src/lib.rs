//! External-monitor hardware control via DDC/CI.
//!
//! This crate turns a raw per-monitor VCP wire into a robust
//! [`duja_core::controller::BrightnessController`]. It is split so the hard,
//! monitor-specific policy is testable without hardware:
//!
//! - [`transport`] — the [`transport::VcpTransport`] seam: three primitive wire
//!   operations plus a classified error. The real one is the Windows dxva2
//!   transport; tests use a scriptable fake.
//! - [`controller`] — [`controller::DdcController`], which owns *all* the
//!   policy: pacing, retry with back-off, verify-by-readback, capability
//!   parsing, and every quirk from [`duja_core::quirks`]. Time is injected
//!   through [`clock::Clock`] so behaviour is deterministic in tests.
//! - [`clock`] — the [`clock::Clock`] time source ([`clock::SystemClock`] in
//!   production; a virtual clock in tests).
//!
//! On Windows the crate additionally exposes `enumerate` (in the
//! Windows-only `win` module), which discovers the attached external monitors
//! (identity from EDID, quirks from the embedded database) and hands back a
//! `DdcDisplay` per monitor that can be turned into a thread-owned controller.
//!
//! # Safety policy
//! All FFI is confined to the `win::sys` module, where every `unsafe` block
//! carries a `// SAFETY:` justification; the rest of the crate is safe.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod clock;
pub mod controller;
pub mod transport;

#[cfg(windows)]
mod win;

#[cfg(test)]
mod fake;

#[cfg(test)]
mod tests;

pub use clock::{Clock, SystemClock};
pub use controller::{DEFAULT_MIN_GAP, DdcController};
pub use transport::{TransportError, VcpReading, VcpTransport};

#[cfg(windows)]
pub use win::{DdcDisplay, DdcError, Dxva2Transport, enumerate};

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn links_against_core() {
        assert_eq!(version(), duja_core::version());
    }
}

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
//! On Windows the crate additionally exposes `enumerate` (in the Windows-only
//! `win` module), which discovers the attached external monitors (identity from
//! EDID, quirks from the embedded database) and hands back a `DdcDisplay` per
//! monitor that can be turned into a thread-owned controller. macOS exposes the
//! same shape from a `mac` module (see below).
//!
//! - [`ddcci`] — the cross-platform DDC/CI wire codec (packet framing,
//!   checksums, reply parsing) plus the [`ddcci::I2cBus`] seam and the
//!   [`ddcci::DdcCiTransport`] built on it. Windows never touches this (dxva2
//!   frames packets for us); macOS does. It is pure, safe Rust and unit-tested
//!   on every OS.
//!
//! # macOS backend (experimental)
//! The `mac` module implements the same surface as `win` over CoreGraphics
//! enumeration and two I2C transports — the private `IOAVService` symbols on
//! Apple Silicon and `IOI2CInterface` on Intel (see ADR-0013). Because Duja has
//! **no macOS hardware** and the CI mac runners are virtualized (no external
//! DDC display, and the internal panel is skipped), `enumerate` returns an
//! empty list in CI and the transports have never executed against a real
//! monitor. **DDC-on-mac is therefore experimental** until there are **at least
//! three independent community confirmations per architecture** (Apple Silicon
//! and Intel), per plan §P6. The pure protocol codec ([`ddcci`]) *is* fully
//! verified in CI; only the hardware I/O is unproven.
//!
//! # Safety policy
//! All FFI is confined to the platform `sys` modules (`win::sys`, `mac::sys`),
//! where every `unsafe` block carries a `// SAFETY:` justification; the rest of
//! the crate — including the entire [`ddcci`] codec — is safe.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod clock;
pub mod controller;
pub mod ddcci;
pub mod transport;

#[cfg(windows)]
mod win;

#[cfg(target_os = "macos")]
mod mac;

#[cfg(test)]
mod fake;

#[cfg(test)]
mod tests;

pub use clock::{Clock, SystemClock};
pub use controller::{DEFAULT_MIN_GAP, DdcController};
pub use ddcci::{DdcCiError, DdcCiTransport, DdcWire, I2cBus};
pub use transport::{TransportError, VcpReading, VcpTransport};

#[cfg(windows)]
pub use win::{DdcDisplay, DdcError, Dxva2Transport, enumerate};

#[cfg(target_os = "macos")]
pub use mac::{DdcDisplay, DdcError, enumerate};

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

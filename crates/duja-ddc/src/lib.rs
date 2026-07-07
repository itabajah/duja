//! External-monitor hardware control via DDC/CI.
//!
//! Per-OS backends (Windows Monitor Configuration API, macOS `IOAVService` /
//! `IOFramebuffer`, Linux i2c-dev) live behind `cfg` gates in this crate and
//! implement [`duja_core`]'s `BrightnessController` trait.
//!
//! # Safety policy
//! All FFI is confined to `ffi`/`sys` modules with `// SAFETY:` documented
//! invariants; the rest of the crate is safe wrappers. (Backends land in P3+.)

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

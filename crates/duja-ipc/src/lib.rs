//! Local IPC between `dujactl` (and second app instances) and the running app.
//!
//! Protocol: length-prefixed JSON, 64 KiB max frame enforced **before**
//! allocation, versioned envelope, strict parameter validation. Transports
//! (P5): Windows named pipe with explicit user-only DACL and anti-squatting
//! flags; unix sockets with 0600 perms + peer-uid checks. See SECURITY.md.

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

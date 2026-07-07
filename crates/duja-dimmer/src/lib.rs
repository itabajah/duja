//! Software dimming: the fallback layer of Duja's brightness continuum.
//!
//! Primary mechanism: a per-monitor, borderless, always-on-top,
//! **click-through** overlay window with variable-alpha black fill — the only
//! technique that reaches true black on every OS, survives HDR, and works on
//! GNOME Wayland-less-gamma environments. Opt-in gamma-ramp backend exists
//! only where verified safe (never in HDR; ADR-0003).
//!
//! Invariant (security property, QA-checked every release): overlays must
//! NEVER intercept input. (Backends land in P4+.)

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

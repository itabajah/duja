//! Pure raw → percent brightness scaling.
//!
//! Mirrors the engine's internal scaling so the binary's own `--once` level
//! readout agrees with what the engine reports. Kept here (rather than reaching
//! into the engine's private helpers) so it is unit testable in the binary
//! crate.

/// Reflect a raw hardware value back to a percent.
///
/// Guards a zero `max` (returns 0) so it never divides by zero.
pub(crate) fn raw_to_pct(current: u16, max: u16) -> u8 {
    let pct = u32::from(current)
        .saturating_mul(100)
        .checked_div(u32::from(max))
        .unwrap_or(0);
    u8::try_from(pct.min(100)).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    use super::raw_to_pct;

    #[test]
    fn raw_to_pct_inverts_and_guards_zero_max() {
        assert_eq!(raw_to_pct(0, 100), 0);
        assert_eq!(raw_to_pct(50, 100), 50);
        assert_eq!(raw_to_pct(100, 100), 100);
        assert_eq!(raw_to_pct(200, 400), 50);
        assert_eq!(raw_to_pct(10, 0), 0);
    }
}

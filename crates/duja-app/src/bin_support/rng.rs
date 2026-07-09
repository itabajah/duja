//! A tiny dependency-free PRNG (xorshift64) for the stress flood.
//!
//! Not cryptographic and not meant to be — it only needs cheap, repeatable
//! spread of brightness values without pulling in the `rand` crate.

/// A xorshift64 generator.
#[derive(Debug, Clone)]
pub(crate) struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    /// Seed the generator. A zero seed is remapped (xorshift is stuck at 0).
    pub(crate) fn new(seed: u64) -> Self {
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self { state }
    }

    /// Advance and return the next 64-bit value.
    pub(crate) fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// A pseudo-random brightness percent in `0..=100`.
    pub(crate) fn next_pct(&mut self) -> u8 {
        let n = self.next_u64().checked_rem(101).unwrap_or(0);
        u8::try_from(n).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::XorShift64;

    #[test]
    fn percents_stay_in_range() {
        let mut rng = XorShift64::new(42);
        for _ in 0..10_000 {
            assert!(rng.next_pct() <= 100);
        }
    }

    #[test]
    fn is_deterministic_for_a_seed() {
        let mut a = XorShift64::new(7);
        let mut b = XorShift64::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn zero_seed_is_not_stuck() {
        let mut rng = XorShift64::new(0);
        assert_ne!(rng.next_u64(), 0);
    }

    #[test]
    fn produces_varied_values() {
        let mut rng = XorShift64::new(1);
        let first = rng.next_pct();
        // Over many draws we must see at least one value different from the
        // first (guards against a stuck generator).
        assert!((0..1000).any(|_| rng.next_pct() != first));
    }
}

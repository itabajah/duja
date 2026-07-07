//! A deterministic, manually-advanced monotonic clock for testing the
//! clock-passed-in state machines.

use std::time::{Duration, Instant};

/// A fake monotonic clock whose time only moves when [`advance`](Self::advance)
/// is called.
///
/// It is anchored to a single real [`Instant`] at construction (there is no
/// other way to mint an `Instant`), but only elapsed *differences* are
/// meaningful, and those are fully deterministic.
#[derive(Debug, Clone)]
pub struct FakeClock {
    base: Instant,
    elapsed: Duration,
}

impl FakeClock {
    /// Create a clock anchored at "now" with zero elapsed time.
    #[must_use]
    pub fn new() -> Self {
        FakeClock {
            base: Instant::now(),
            elapsed: Duration::ZERO,
        }
    }

    /// The current fake instant (`anchor + total advanced`).
    #[must_use]
    pub fn now(&self) -> Instant {
        self.base.checked_add(self.elapsed).unwrap_or(self.base)
    }

    /// Move the clock forward by `by`. Monotonic: time never goes backwards.
    pub fn advance(&mut self, by: Duration) {
        self.elapsed = self.elapsed.checked_add(by).unwrap_or(self.elapsed);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn advance_moves_time_forward_monotonically() {
        let mut clk = FakeClock::new();
        let t0 = clk.now();
        clk.advance(Duration::from_millis(100));
        let t1 = clk.now();
        assert!(t1 > t0);
        assert_eq!(t1.duration_since(t0), Duration::from_millis(100));
        clk.advance(Duration::from_millis(50));
        assert!(clk.now() > t1);
        assert_eq!(clk.now().duration_since(t0), Duration::from_millis(150));
    }

    #[test]
    fn now_is_stable_without_advancing() {
        let clk = FakeClock::default();
        assert_eq!(clk.now(), clk.now());
    }
}

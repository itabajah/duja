//! A deterministic, manually-advanced monotonic clock for testing the
//! clock-passed-in state machines.

// ---- specs first (TDD); implementation follows in the next commit ----

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

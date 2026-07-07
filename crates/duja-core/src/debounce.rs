//! Pure debounce and coalesce state machines.
//!
//! Neither type reads the clock: the caller passes `now: Instant` into every
//! method. This keeps the thread harness a thin shell and makes the timing
//! logic exhaustively unit-testable with a fake clock.

// ---- specs first (TDD); implementation follows in the next commit ----

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn base() -> Instant {
        Instant::now()
    }

    /// `base + ms` without tripping the arithmetic lint.
    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).unwrap()
    }

    const DELAY_MS: u64 = 100;

    fn debouncer() -> Debouncer {
        Debouncer::new(Duration::from_millis(DELAY_MS))
    }

    #[test]
    fn idle_debouncer_polls_wait() {
        let mut d = debouncer();
        assert_eq!(d.poll(base()), Action::Wait);
    }

    #[test]
    fn on_event_schedules_fire_at_deadline() {
        let b = base();
        let mut d = debouncer();
        assert_eq!(d.on_event(b), Action::FireAt(at(b, DELAY_MS)));
    }

    #[test]
    fn trailing_edge_fires_after_quiet_period() {
        let b = base();
        let mut d = debouncer();
        d.on_event(b);
        assert_eq!(d.poll(at(b, 50)), Action::FireAt(at(b, DELAY_MS)));
        assert_eq!(d.poll(at(b, DELAY_MS)), Action::Fire);
        // Fired once; now quiescent.
        assert_eq!(d.poll(at(b, 101)), Action::Wait);
    }

    #[test]
    fn burst_collapses_to_single_fire() {
        let b = base();
        let mut d = debouncer();
        d.on_event(b);
        d.on_event(at(b, 10));
        d.on_event(at(b, 20));
        // Deadline follows the last event (20 + 100 = 120).
        assert_eq!(d.poll(at(b, 119)), Action::FireAt(at(b, 120)));
        assert_eq!(d.poll(at(b, 120)), Action::Fire);
        assert_eq!(d.poll(at(b, 121)), Action::Wait);
    }

    #[test]
    fn late_event_extends_deadline() {
        let b = base();
        let mut d = debouncer();
        d.on_event(b); // deadline b+100
        d.on_event(at(b, 50)); // deadline now b+150
        // At the original deadline it must NOT fire.
        assert_eq!(d.poll(at(b, DELAY_MS)), Action::FireAt(at(b, 150)));
        assert_eq!(d.poll(at(b, 150)), Action::Fire);
    }

    // --- Coalescer ---

    const GAP_MS: u64 = 100;

    fn coalescer() -> Coalescer<&'static str, u32> {
        Coalescer::new(Duration::from_millis(GAP_MS))
    }

    #[test]
    fn empty_coalescer_yields_nothing() {
        let mut c = coalescer();
        assert_eq!(c.next_ready(base()), None);
    }

    #[test]
    fn first_push_is_immediately_ready_then_drains() {
        let b = base();
        let mut c = coalescer();
        c.push("a", 1, b);
        assert_eq!(c.next_ready(b), Some(("a", 1)));
        assert_eq!(c.next_ready(b), None);
    }

    #[test]
    fn latest_value_wins_per_key() {
        let b = base();
        let mut c = coalescer();
        c.push("a", 1, b);
        c.push("a", 2, b);
        assert_eq!(c.next_ready(b), Some(("a", 2)));
    }

    #[test]
    fn min_gap_enforced_between_emissions() {
        let b = base();
        let mut c = coalescer();
        c.push("a", 1, b);
        assert_eq!(c.next_ready(b), Some(("a", 1)));
        c.push("a", 2, at(b, 50));
        // Within the gap: suppressed.
        assert_eq!(c.next_ready(at(b, 50)), None);
        // Gap elapsed: emits the latest value.
        assert_eq!(c.next_ready(at(b, GAP_MS)), Some(("a", 2)));
    }

    #[test]
    fn per_key_isolation_and_no_cross_coalescing() {
        let b = base();
        let mut c = coalescer();
        c.push("brightness", 30, b);
        c.push("input", 60, b);
        let mut got = vec![c.next_ready(b), c.next_ready(b)];
        got.sort_unstable();
        assert_eq!(got, vec![Some(("brightness", 30)), Some(("input", 60))]);
        assert_eq!(c.next_ready(b), None);
    }

    #[test]
    fn burst_yields_single_emission_of_latest() {
        let b = base();
        let mut c = coalescer();
        for (i, v) in (1u32..=5).enumerate() {
            let ms = u64::try_from(i).unwrap();
            c.push("a", v, at(b, ms));
        }
        assert_eq!(c.next_ready(at(b, 4)), Some(("a", 5)));
        assert_eq!(c.next_ready(at(b, 4)), None);
    }
}

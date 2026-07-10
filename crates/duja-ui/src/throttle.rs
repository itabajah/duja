//! A pure, clock-injected rate gate for UI-side emit throttling.
//!
//! Slider drags fire far faster than a hardware write should be dispatched. The
//! [`ThrottleGate`] caps the *emit rate* out of the shell to one per
//! `min_interval` (the plan's ~60 ms UI event rate). Like
//! [`duja_core::debounce`], it never reads the clock itself: the caller passes
//! `now: Instant` into [`allow`](ThrottleGate::allow), so the logic is
//! exhaustively testable with a fake clock.
//!
//! # Why leading-edge (drop, not buffer)
//!
//! The gate lets the *first* event through and drops the rest of a burst. It
//! carries no trailing timer, because the engine already guarantees the final
//! value lands: `duja_core::debounce::Coalescer` keeps the latest-per-display
//! write and emits it once its own min-gap elapses. Dropping intermediate
//! slider samples at the UI therefore loses no final state — the user pausing
//! on a value lets the next `allow` succeed and that value flow through.

use std::time::{Duration, Instant};

/// A leading-edge rate limiter: admits at most one event per `min_interval`.
#[derive(Debug, Clone)]
pub struct ThrottleGate {
    min_interval: Duration,
    last_admit: Option<Instant>,
}

impl ThrottleGate {
    /// Create a gate that admits an event at most once per `min_interval`.
    ///
    /// A zero `min_interval` admits every event.
    #[must_use]
    pub fn new(min_interval: Duration) -> Self {
        ThrottleGate {
            min_interval,
            last_admit: None,
        }
    }

    /// Decide whether an event at `now` is admitted.
    ///
    /// Returns `true` (and records `now` as the last admission) when at least
    /// `min_interval` has elapsed since the previous admission, or when this is
    /// the first event. Returns `false` — dropping the event — otherwise. A
    /// `now` earlier than the last admission (a non-monotonic clock) is treated
    /// as still within the interval and dropped.
    pub fn allow(&mut self, now: Instant) -> bool {
        let admit = match self.last_admit {
            None => true,
            Some(last) => now
                .checked_duration_since(last)
                .is_some_and(|elapsed| elapsed >= self.min_interval),
        };
        if admit {
            self.last_admit = Some(now);
        }
        admit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Instant {
        Instant::now()
    }

    /// `base + ms` without tripping the arithmetic lint.
    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).unwrap()
    }

    const INTERVAL_MS: u64 = 60;

    fn gate() -> ThrottleGate {
        ThrottleGate::new(Duration::from_millis(INTERVAL_MS))
    }

    #[test]
    fn first_event_is_always_admitted() {
        let mut g = gate();
        assert!(g.allow(base()));
    }

    #[test]
    fn burst_within_interval_is_dropped() {
        let b = base();
        let mut g = gate();
        assert!(g.allow(b));
        assert!(!g.allow(at(b, 10)));
        assert!(!g.allow(at(b, 59)));
    }

    #[test]
    fn admits_again_once_interval_elapses() {
        let b = base();
        let mut g = gate();
        assert!(g.allow(b));
        assert!(!g.allow(at(b, 30)));
        assert!(g.allow(at(b, INTERVAL_MS)));
        // The clock for the next window resets to the last admission.
        assert!(!g.allow(at(b, INTERVAL_MS + 10)));
    }

    #[test]
    fn exact_boundary_is_inclusive() {
        let b = base();
        let mut g = gate();
        assert!(g.allow(b));
        assert!(g.allow(at(b, INTERVAL_MS)));
    }

    #[test]
    fn non_monotonic_now_is_dropped() {
        let b = base();
        let mut g = gate();
        assert!(g.allow(at(b, 100)));
        // A clock that goes backwards must not admit.
        assert!(!g.allow(at(b, 50)));
    }

    #[test]
    fn zero_interval_admits_everything() {
        let b = base();
        let mut g = ThrottleGate::new(Duration::from_millis(0));
        assert!(g.allow(b));
        assert!(g.allow(b));
        assert!(g.allow(at(b, 1)));
    }
}

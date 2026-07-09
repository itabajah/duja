//! The injected time source that makes the controller's pacing and retry
//! back-off deterministically testable.
//!
//! The real controller drives a [`SystemClock`] (wall-clock [`Instant::now`] +
//! [`std::thread::sleep`]). Tests substitute a fake clock that advances only
//! when the controller asks it to sleep, so no test ever blocks a real thread
//! yet every pacing decision is observable.

use std::time::{Duration, Instant};

/// A monotonic time source with a cooperative sleep.
///
/// `now` must never move backwards. `sleep` may block real time (the system
/// clock) or merely advance a virtual clock (a test clock); either way the
/// controller treats the elapsed span between two `now` calls as authoritative
/// when deciding whether the minimum inter-operation gap has passed.
pub trait Clock: Send + std::fmt::Debug {
    /// The current instant. Monotonic across calls.
    fn now(&self) -> Instant;

    /// Wait for `dur`. A real clock blocks the thread; a virtual clock advances
    /// its notion of "now" so the next [`now`](Clock::now) reflects the wait.
    fn sleep(&mut self, dur: Duration);
}

/// The production [`Clock`]: real monotonic time and real thread sleeps.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, dur: Duration) {
        if !dur.is_zero() {
            std::thread::sleep(dur);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_is_monotonic() {
        let clk = SystemClock;
        let a = clk.now();
        let b = clk.now();
        assert!(b >= a);
    }

    #[test]
    fn system_clock_zero_sleep_is_a_noop() {
        let mut clk = SystemClock;
        let before = clk.now();
        clk.sleep(Duration::ZERO);
        // A zero sleep must not panic and returns effectively immediately.
        assert!(clk.now() >= before);
    }
}

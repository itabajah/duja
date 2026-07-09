//! A [`BrightnessController`] decorator that tallies the hardware calls made
//! through it, used by the stress harness to prove write-coalescing.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use duja_core::controller::{BrightnessController, ControlError};
use duja_core::model::{Capabilities, Feature, FeatureRange};

/// Thread-safe call tallies shared between a [`CountingController`] and the
/// stress harness that reads them after the run.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    /// Number of `set` (hardware write) calls performed.
    sets: AtomicU64,
    /// Number of `get` (hardware read) calls performed.
    gets: AtomicU64,
    /// Number of `probe` calls performed.
    probes: AtomicU64,
    /// Number of operations that returned an error.
    errors: AtomicU64,
}

impl Counters {
    /// A fresh, zeroed set of counters behind an [`Arc`].
    pub(crate) fn new_shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Hardware writes performed.
    pub(crate) fn sets(&self) -> u64 {
        self.sets.load(Ordering::Relaxed)
    }

    /// Hardware reads performed.
    pub(crate) fn gets(&self) -> u64 {
        self.gets.load(Ordering::Relaxed)
    }

    /// Operations that returned an error.
    pub(crate) fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    /// Record the outcome of one operation on the error tally.
    fn record<T>(&self, outcome: &Result<T, ControlError>) {
        if outcome.is_err() {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Wraps any [`BrightnessController`], forwarding every call to the inner
/// controller while incrementing the shared [`Counters`].
pub(crate) struct CountingController {
    inner: Box<dyn BrightnessController>,
    counters: Arc<Counters>,
}

impl CountingController {
    /// Decorate `inner`, reporting call counts into `counters`.
    pub(crate) fn new(inner: Box<dyn BrightnessController>, counters: Arc<Counters>) -> Self {
        Self { inner, counters }
    }
}

impl fmt::Debug for CountingController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CountingController")
            .field("counters", &self.counters)
            .finish_non_exhaustive()
    }
}

impl BrightnessController for CountingController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        self.counters.probes.fetch_add(1, Ordering::Relaxed);
        let out = self.inner.probe();
        self.counters.record(&out);
        out
    }

    fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError> {
        self.counters.gets.fetch_add(1, Ordering::Relaxed);
        let out = self.inner.get(feature);
        self.counters.record(&out);
        out
    }

    fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
        self.counters.sets.fetch_add(1, Ordering::Relaxed);
        let out = self.inner.set(feature, value);
        self.counters.record(&out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{Counters, CountingController};
    use duja_core::controller::{BrightnessController, ControlError};
    use duja_core::model::Feature;
    use duja_core::testing::controller::FakeController;

    #[test]
    fn counts_each_operation_kind() {
        let counters = Counters::new_shared();
        let mut c = CountingController::new(Box::new(FakeController::new()), counters.clone());

        let _ = c.probe();
        let _ = c.get(Feature::Brightness);
        let _ = c.set(Feature::Brightness, 40);
        let _ = c.set(Feature::Brightness, 60);

        assert_eq!(counters.gets(), 1);
        assert_eq!(counters.sets(), 2);
        assert_eq!(counters.errors(), 0);
    }

    #[test]
    fn tallies_errors() {
        let counters = Counters::new_shared();
        let mut fake = FakeController::new();
        fake.push_error(ControlError::Timeout);
        let mut c = CountingController::new(Box::new(fake), counters.clone());

        assert!(c.get(Feature::Brightness).is_err());
        assert!(c.get(Feature::Brightness).is_ok());
        assert_eq!(counters.errors(), 1);
        assert_eq!(counters.gets(), 2);
    }

    #[test]
    fn forwards_values_to_inner() {
        let counters = Counters::new_shared();
        let mut c = CountingController::new(Box::new(FakeController::new()), counters);
        c.set(Feature::Brightness, 73).unwrap();
        assert_eq!(c.get(Feature::Brightness).unwrap().current, 73);
    }
}

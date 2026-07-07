//! A scriptable [`crate::controller::BrightnessController`] fake: per-feature
//! values, an injectable error queue, a latency marker, and a call log.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use crate::controller::{BrightnessController, ControlError};
use crate::model::{Capabilities, Feature, FeatureRange};

/// One recorded operation against a [`FakeController`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// A [`BrightnessController::probe`] call.
    Probe,
    /// A [`BrightnessController::get`] call for a feature.
    Get(Feature),
    /// A [`BrightnessController::set`] call with the requested value.
    Set(Feature, u16),
}

/// A deterministic, scriptable [`BrightnessController`] for tests.
///
/// Errors pushed with [`push_error`](Self::push_error) are returned by the
/// next operations, one per op, then normal behaviour resumes (so a failure
/// never poisons later calls). Every call is recorded in [`calls`](Self::calls).
#[derive(Debug)]
pub struct FakeController {
    caps: Capabilities,
    values: BTreeMap<Feature, FeatureRange>,
    errors: VecDeque<ControlError>,
    log: Vec<Call>,
    connected: bool,
    latency: Duration,
}

impl FakeController {
    /// A connected controller supporting `Brightness` and `Contrast` (but not
    /// `InputSource`, so the "unsupported" contract case is exercisable), each
    /// seeded at current 50 / max 100.
    #[must_use]
    pub fn new() -> Self {
        let caps = Capabilities {
            features: [Feature::Brightness, Feature::Contrast]
                .into_iter()
                .collect(),
            hardware_range: true,
            raw_capabilities: None,
        };
        Self::with_capabilities(caps)
    }

    /// A connected controller with the given capabilities; each supported
    /// feature is seeded at 50/100.
    #[must_use]
    pub fn with_capabilities(caps: Capabilities) -> Self {
        let values = caps
            .features
            .iter()
            .map(|&f| {
                (
                    f,
                    FeatureRange {
                        current: 50,
                        max: 100,
                    },
                )
            })
            .collect();
        FakeController {
            caps,
            values,
            errors: VecDeque::new(),
            log: Vec::new(),
            connected: true,
            latency: Duration::ZERO,
        }
    }

    /// A controller that reports [`ControlError::Disconnected`] for every op.
    #[must_use]
    pub fn disconnected() -> Self {
        let mut c = Self::new();
        c.connected = false;
        c
    }

    /// Overwrite the stored range for a feature (regardless of support).
    pub fn seed(&mut self, feature: Feature, range: FeatureRange) {
        self.values.insert(feature, range);
    }

    /// Set a feature's current value, keeping its existing max (default 100).
    pub fn set_value(&mut self, feature: Feature, current: u16) {
        let max = self.values.get(&feature).map_or(100, |r| r.max);
        self.values.insert(
            feature,
            FeatureRange {
                current: current.min(max),
                max,
            },
        );
    }

    /// Queue an error to be returned by the next operation.
    pub fn push_error(&mut self, err: ControlError) {
        self.errors.push_back(err);
    }

    /// Set the latency marker (recorded metadata; no real time passes).
    pub fn set_latency(&mut self, latency: Duration) {
        self.latency = latency;
    }

    /// Mark the controller as disconnected.
    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    /// The recorded call log, in order.
    #[must_use]
    pub fn calls(&self) -> &[Call] {
        &self.log
    }

    /// The current latency marker.
    #[must_use]
    pub fn latency(&self) -> Duration {
        self.latency
    }

    /// Pop a queued error, if any (consumed one per operation).
    fn take_error(&mut self) -> Option<ControlError> {
        self.errors.pop_front()
    }
}

impl Default for FakeController {
    fn default() -> Self {
        Self::new()
    }
}

impl BrightnessController for FakeController {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        self.log.push(Call::Probe);
        if let Some(err) = self.take_error() {
            return Err(err);
        }
        if !self.connected {
            return Err(ControlError::Disconnected);
        }
        Ok(self.caps.clone())
    }

    fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError> {
        self.log.push(Call::Get(feature));
        if let Some(err) = self.take_error() {
            return Err(err);
        }
        if !self.connected {
            return Err(ControlError::Disconnected);
        }
        if !self.caps.supports(feature) {
            return Err(ControlError::Unsupported);
        }
        Ok(self.values.get(&feature).copied().unwrap_or(FeatureRange {
            current: 0,
            max: 100,
        }))
    }

    fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
        self.log.push(Call::Set(feature, value));
        if let Some(err) = self.take_error() {
            return Err(err);
        }
        if !self.connected {
            return Err(ControlError::Disconnected);
        }
        if !self.caps.supports(feature) {
            return Err(ControlError::Unsupported);
        }
        let max = self.values.get(&feature).map_or(100, |r| r.max);
        self.values.insert(
            feature,
            FeatureRange {
                current: value.min(max),
                max,
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{BrightnessController, ControlError};
    use crate::model::Feature;
    use std::time::Duration;

    #[test]
    fn logs_calls_in_order() {
        let mut c = FakeController::new();
        let _ = c.probe();
        let _ = c.set(Feature::Brightness, 30);
        let _ = c.get(Feature::Brightness);
        assert_eq!(
            c.calls(),
            &[
                Call::Probe,
                Call::Set(Feature::Brightness, 30),
                Call::Get(Feature::Brightness),
            ]
        );
    }

    #[test]
    fn set_get_roundtrips_and_clamps_to_max() {
        let mut c = FakeController::new();
        c.set(Feature::Brightness, 80).unwrap();
        assert_eq!(c.get(Feature::Brightness).unwrap().current, 80);
        c.set(Feature::Brightness, 5000).unwrap();
        assert_eq!(c.get(Feature::Brightness).unwrap().current, 100);
    }

    #[test]
    fn injected_error_is_consumed_once_then_recovers() {
        let mut c = FakeController::new();
        c.push_error(ControlError::Timeout);
        assert!(matches!(
            c.get(Feature::Brightness),
            Err(ControlError::Timeout)
        ));
        assert!(c.get(Feature::Brightness).is_ok());
    }

    #[test]
    fn disconnected_reports_disconnected() {
        let mut c = FakeController::disconnected();
        assert!(matches!(c.probe(), Err(ControlError::Disconnected)));
        assert!(matches!(
            c.get(Feature::Brightness),
            Err(ControlError::Disconnected)
        ));
    }

    #[test]
    fn unsupported_feature_reports_unsupported() {
        let mut c = FakeController::new();
        assert!(matches!(
            c.get(Feature::InputSource),
            Err(ControlError::Unsupported)
        ));
        assert!(matches!(
            c.set(Feature::InputSource, 1),
            Err(ControlError::Unsupported)
        ));
    }

    #[test]
    fn latency_marker_roundtrips() {
        let mut c = FakeController::new();
        c.set_latency(Duration::from_millis(50));
        assert_eq!(c.latency(), Duration::from_millis(50));
    }
}

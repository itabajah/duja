//! The cross-backend [`crate::controller::BrightnessController`] contract.
//!
//! Every backend (fake now; real DDC/panel backends in later phases) must
//! satisfy [`run_controller_contract`]. The factory produces a fresh
//! controller for each scenario so cases never interfere.

use crate::controller::{BrightnessController, ControlError};
use crate::model::Feature;

/// The state a factory should build a controller in for a given contract case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Connected and healthy.
    Nominal,
    /// Every operation reports [`ControlError::Disconnected`].
    Disconnected,
    /// The first operation fails, then the controller behaves normally.
    ErrorThenOk,
}

/// Run the full [`BrightnessController`] contract against controllers built by
/// `factory`, allowing `tolerance` units of set/get round-trip drift.
///
/// The seven cases (plan §4.2 + ADR-0002): every supported *continuous*
/// feature reports a sane range (`max > 0`, `current <= max`) and round-trips
/// a set/get within tolerance, probe is idempotent, out-of-range is
/// rejected-or-clamped, a disconnected controller reports `Disconnected`
/// (never panics), an unsupported feature reports `Unsupported`, and an
/// injected error does not poison later operations.
///
/// # Non-continuous features (ADR-0002)
/// [`Feature::InputSource`] is deliberately **read-only** here and exempt from
/// the range-sanity checks: real monitors lie about its `max` (the P1 spike
/// measured `current=15, max=3`), the honest value set comes from the caps
/// string ∩ quirks, and a blind write would switch a real monitor's input
/// mid-test. The contract only requires that `get` succeeds when it is
/// reported as supported.
///
/// # Panics
/// Panics (failing the calling test) if the controller violates any case.
pub fn run_controller_contract<C, F>(factory: F, tolerance: u16)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    supported_ranges_are_sane(&factory);
    round_trip_within_tolerance(&factory, tolerance);
    probe_is_idempotent(&factory);
    out_of_range_rejected_or_clamped(&factory);
    unsupported_reports_unsupported(&factory);
    disconnected_reports_disconnected(&factory);
    errors_do_not_poison(&factory);
}

/// The continuous features (safe to write in a contract run).
const CONTINUOUS: [Feature; 2] = [Feature::Brightness, Feature::Contrast];

/// Every supported feature, via a fresh probe.
fn supported<C: BrightnessController>(c: &mut C) -> Vec<Feature> {
    let caps = c.probe().ok();
    assert!(caps.is_some(), "nominal probe must succeed");
    caps.map(|caps| {
        Feature::ALL
            .into_iter()
            .filter(|f| caps.supports(*f))
            .collect()
    })
    .unwrap_or_default()
}

/// Every supported continuous feature must report `max > 0` and
/// `current <= max`; a supported [`Feature::InputSource`] must be readable
/// but its metadata is untrusted (ADR-0002).
fn supported_ranges_are_sane<C, F>(factory: &F)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::Nominal);
    for feature in supported(&mut c) {
        let range = c.get(feature).ok();
        assert!(
            range.is_some(),
            "get on supported feature {feature:?} must succeed"
        );
        let Some(range) = range else { continue };
        if CONTINUOUS.contains(&feature) {
            assert!(
                range.max > 0,
                "{feature:?} max must be positive; a zero max would peg the \
                 feature at raw 0 (ADR-0002 range sanity)"
            );
            assert!(
                range.current <= range.max,
                "{feature:?} current {} exceeds max {}",
                range.current,
                range.max
            );
        }
        // InputSource: readable is enough — max/current relations are
        // legitimately violated by real hardware (ADR-0002).
    }
}

fn round_trip_within_tolerance<C, F>(factory: &F, tolerance: u16)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::Nominal);
    let features: Vec<Feature> = supported(&mut c)
        .into_iter()
        .filter(|f| CONTINUOUS.contains(f))
        .collect();

    for feature in features {
        let range = c.get(feature).ok();
        assert!(
            range.is_some(),
            "get on supported feature {feature:?} must succeed"
        );
        let Some(range) = range else { continue };

        let target = 42u16.min(range.max);
        assert!(
            c.set(feature, target).is_ok(),
            "in-range set on {feature:?} must succeed"
        );

        let after = c.get(feature).ok();
        assert!(after.is_some(), "get after set on {feature:?} must succeed");
        let Some(after) = after else { continue };

        let drift = after.current.abs_diff(target);
        assert!(
            drift <= tolerance,
            "{feature:?} round-trip drift {drift} exceeds tolerance {tolerance}"
        );
    }
}

fn probe_is_idempotent<C, F>(factory: &F)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::Nominal);
    let first = c.probe().ok();
    let second = c.probe().ok();
    assert!(
        first.is_some() && second.is_some(),
        "probe must succeed in the nominal scenario"
    );
    assert_eq!(first, second, "probe must be idempotent");
}

fn out_of_range_rejected_or_clamped<C, F>(factory: &F)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::Nominal);
    let features: Vec<Feature> = supported(&mut c)
        .into_iter()
        .filter(|f| CONTINUOUS.contains(f))
        .collect();
    for feature in features {
        // A backend may reject an out-of-range set outright; if it accepts,
        // the stored value must be clamped within the reported max.
        if let Ok(()) = c.set(feature, u16::MAX) {
            let after = c.get(feature).ok();
            assert!(
                after.is_some(),
                "get after clamp on {feature:?} must succeed"
            );
            if let Some(range) = after {
                assert!(
                    range.current <= range.max,
                    "{feature:?} clamped value {} exceeds max {}",
                    range.current,
                    range.max
                );
            }
        }
    }
}

fn unsupported_reports_unsupported<C, F>(factory: &F)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::Nominal);
    let caps = c.probe().ok();
    assert!(caps.is_some(), "nominal probe must succeed");
    let Some(caps) = caps else { return };
    let Some(feature) = Feature::ALL.into_iter().find(|f| !caps.supports(*f)) else {
        return; // supports everything; nothing to assert
    };
    assert!(
        matches!(c.get(feature), Err(ControlError::Unsupported)),
        "get on an unsupported feature must report Unsupported"
    );
    assert!(
        matches!(c.set(feature, 1), Err(ControlError::Unsupported)),
        "set on an unsupported feature must report Unsupported"
    );
}

fn disconnected_reports_disconnected<C, F>(factory: &F)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::Disconnected);
    assert!(
        matches!(c.probe(), Err(ControlError::Disconnected)),
        "probe on a disconnected controller must report Disconnected"
    );
    assert!(
        matches!(c.get(Feature::Brightness), Err(ControlError::Disconnected)),
        "get on a disconnected controller must report Disconnected"
    );
    assert!(
        matches!(
            c.set(Feature::Brightness, 10),
            Err(ControlError::Disconnected)
        ),
        "set on a disconnected controller must report Disconnected"
    );
}

fn errors_do_not_poison<C, F>(factory: &F)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::ErrorThenOk);
    assert!(
        c.get(Feature::Brightness).is_err(),
        "the first operation should surface the injected error"
    );
    assert!(
        c.get(Feature::Brightness).is_ok(),
        "a later operation must succeed after an error"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::ControlError;
    use crate::testing::controller::FakeController;

    fn factory(scenario: Scenario) -> FakeController {
        match scenario {
            Scenario::Nominal => FakeController::new(),
            Scenario::Disconnected => FakeController::disconnected(),
            Scenario::ErrorThenOk => {
                let mut c = FakeController::new();
                c.push_error(ControlError::Timeout);
                c
            }
        }
    }

    #[test]
    fn fake_controller_satisfies_contract() {
        run_controller_contract(factory, 0);
    }

    use crate::model::{Capabilities, FeatureRange};
    use std::collections::BTreeSet;

    /// A backend that reports `max == 0` for a supported continuous feature —
    /// the exact pathology the P2 gate review proved the old suite greenlit.
    #[derive(Debug)]
    struct ZeroMax;

    impl BrightnessController for ZeroMax {
        fn probe(&mut self) -> Result<Capabilities, ControlError> {
            let features: BTreeSet<Feature> = [Feature::Brightness].into_iter().collect();
            Ok(Capabilities {
                features,
                hardware_range: true,
                raw_capabilities: None,
            })
        }
        fn get(&mut self, f: Feature) -> Result<FeatureRange, ControlError> {
            if f == Feature::Brightness {
                Ok(FeatureRange { current: 0, max: 0 })
            } else {
                Err(ControlError::Unsupported)
            }
        }
        fn set(&mut self, f: Feature, _v: u16) -> Result<(), ControlError> {
            if f == Feature::Brightness {
                Ok(())
            } else {
                Err(ControlError::Unsupported)
            }
        }
    }

    #[test]
    #[should_panic(expected = "max must be positive")]
    fn zero_max_backend_fails_the_contract() {
        run_controller_contract(|_| ZeroMax, 0);
    }

    /// A backend whose `InputSource` metadata lies (`current > max`) — the real
    /// MSI MP273QP behavior from ADR-0002. The contract must TOLERATE this:
    /// input-source metadata is untrusted and the feature is read-only here.
    #[derive(Debug)]
    struct LyingInputSource {
        scenario: Scenario,
        tripped: bool,
        brightness: u16,
    }

    impl LyingInputSource {
        fn new(scenario: Scenario) -> Self {
            Self {
                scenario,
                tripped: false,
                brightness: 70,
            }
        }

        /// Scenario gate shared by every operation.
        fn gate(&mut self) -> Result<(), ControlError> {
            match self.scenario {
                Scenario::Disconnected => Err(ControlError::Disconnected),
                Scenario::ErrorThenOk if !self.tripped => {
                    self.tripped = true;
                    Err(ControlError::Timeout)
                }
                _ => Ok(()),
            }
        }
    }

    impl BrightnessController for LyingInputSource {
        fn probe(&mut self) -> Result<Capabilities, ControlError> {
            self.gate()?;
            let features: BTreeSet<Feature> = [Feature::Brightness, Feature::InputSource]
                .into_iter()
                .collect();
            Ok(Capabilities {
                features,
                hardware_range: true,
                raw_capabilities: None,
            })
        }
        fn get(&mut self, f: Feature) -> Result<FeatureRange, ControlError> {
            self.gate()?;
            match f {
                Feature::Brightness => Ok(FeatureRange {
                    current: self.brightness,
                    max: 100,
                }),
                // The spike-measured lie: current=15 (DP) with "max"=3.
                Feature::InputSource => Ok(FeatureRange {
                    current: 15,
                    max: 3,
                }),
                Feature::Contrast => Err(ControlError::Unsupported),
            }
        }
        fn set(&mut self, f: Feature, v: u16) -> Result<(), ControlError> {
            self.gate()?;
            match f {
                Feature::Brightness => {
                    self.brightness = v.min(100);
                    Ok(())
                }
                // Writing input source in a contract run would be a bug;
                // reject to prove the suite never attempts it.
                Feature::InputSource => Err(ControlError::Timeout),
                Feature::Contrast => Err(ControlError::Unsupported),
            }
        }
    }

    #[test]
    fn max_lying_input_source_is_tolerated_read_only() {
        run_controller_contract(LyingInputSource::new, 0);
    }
}

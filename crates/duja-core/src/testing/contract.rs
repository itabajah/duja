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
/// The six cases (plan §4.2): set/get round-trip within tolerance, probe is
/// idempotent, out-of-range is rejected-or-clamped, a disconnected controller
/// reports `Disconnected` (never panics), an unsupported feature reports
/// `Unsupported`, and an injected error does not poison later operations.
///
/// # Panics
/// Panics (failing the calling test) if the controller violates any case.
pub fn run_controller_contract<C, F>(factory: F, tolerance: u16)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    round_trip_within_tolerance(&factory, tolerance);
    probe_is_idempotent(&factory);
    out_of_range_rejected_or_clamped(&factory);
    unsupported_reports_unsupported(&factory);
    disconnected_reports_disconnected(&factory);
    errors_do_not_poison(&factory);
}

/// The first feature the controller reports as supported, via a fresh probe.
fn first_supported<C: BrightnessController>(c: &mut C) -> Option<Feature> {
    let caps = c.probe().ok();
    assert!(caps.is_some(), "nominal probe must succeed");
    let caps = caps?;
    Feature::ALL.into_iter().find(|f| caps.supports(*f))
}

fn round_trip_within_tolerance<C, F>(factory: &F, tolerance: u16)
where
    C: BrightnessController,
    F: Fn(Scenario) -> C,
{
    let mut c = factory(Scenario::Nominal);
    let Some(feature) = first_supported(&mut c) else {
        return; // nothing to round-trip
    };

    let range = c.get(feature).ok();
    assert!(range.is_some(), "get on a supported feature must succeed");
    let Some(range) = range else { return };

    let target = 42u16.min(range.max);
    assert!(c.set(feature, target).is_ok(), "in-range set must succeed");

    let after = c.get(feature).ok();
    assert!(after.is_some(), "get after set must succeed");
    let Some(after) = after else { return };

    let drift = after.current.abs_diff(target);
    assert!(
        drift <= tolerance,
        "round-trip drift {drift} exceeds tolerance {tolerance}"
    );
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
    let Some(feature) = first_supported(&mut c) else {
        return;
    };
    // A backend may reject an out-of-range set outright; if it accepts, the
    // stored value must be clamped within the reported max.
    if let Ok(()) = c.set(feature, u16::MAX) {
        let after = c.get(feature).ok();
        assert!(after.is_some(), "get after clamp must succeed");
        if let Some(range) = after {
            assert!(
                range.current <= range.max,
                "clamped value {} exceeds max {}",
                range.current,
                range.max
            );
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
}

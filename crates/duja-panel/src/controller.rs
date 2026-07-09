//! [`PanelController`]: the [`BrightnessController`] adapter over any
//! [`PanelTransport`].
//!
//! Internal panels expose exactly one controllable feature — brightness — as a
//! percentage. This adapter therefore:
//! - supports only [`Feature::Brightness`]; [`Feature::Contrast`] and
//!   [`Feature::InputSource`] return [`ControlError::Unsupported`];
//! - reports a [`FeatureRange`] of `{ current, max: 100 }` (percent domain);
//! - reports [`Capabilities`] `{ features: {Brightness}, hardware_range: true,
//!   raw_capabilities: None }` — a real backlight range, but no MCCS caps
//!   string (that is a DDC/CI concept);
//! - **clamps** an out-of-range `set` to `0..=100` rather than rejecting it, so
//!   a slider that overshoots still lands at full brightness.

use duja_core::controller::{BrightnessController, ControlError};
use duja_core::model::{Capabilities, Feature, FeatureRange};

use crate::transport::PanelTransport;

/// The maximum brightness value in the panel's percent domain.
const PANEL_MAX_PCT: u16 = 100;

/// A [`BrightnessController`] backed by a [`PanelTransport`].
///
/// Generic over the transport so the Windows WMI backend and the test fake
/// share one adapter (and one contract run). See the [crate docs](crate) for
/// the supported-feature and range semantics.
#[derive(Debug)]
pub struct PanelController<T: PanelTransport> {
    transport: T,
}

impl<T: PanelTransport> PanelController<T> {
    /// Wrap a transport in the brightness-controller adapter.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Borrow the underlying transport (for backend-specific inspection).
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: PanelTransport> BrightnessController for PanelController<T> {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        // A query doubles as a reachability probe: if the panel is gone this
        // surfaces Disconnected before we advertise capabilities.
        self.transport.query()?;
        Ok(Capabilities {
            features: [Feature::Brightness].into_iter().collect(),
            hardware_range: true,
            raw_capabilities: None,
        })
    }

    fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError> {
        if feature != Feature::Brightness {
            return Err(ControlError::Unsupported);
        }
        let brightness = self.transport.query()?;
        Ok(FeatureRange {
            current: u16::from(brightness.current),
            max: PANEL_MAX_PCT,
        })
    }

    fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
        if feature != Feature::Brightness {
            return Err(ControlError::Unsupported);
        }
        // Clamp into the percent domain; the min guarantees a lossless u8 cast.
        let percent = u8::try_from(value.min(PANEL_MAX_PCT)).unwrap_or(100);
        self.transport.set_brightness(percent)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PanelError;
    use crate::transport::PanelBrightness;
    use duja_core::testing::contract::{Scenario, run_controller_contract};
    use std::collections::VecDeque;

    /// A deterministic, scriptable [`PanelTransport`] for the contract suite.
    ///
    /// Mirrors the scenario semantics of `duja_core`'s `FakeController`: a
    /// queued error is returned once (never poisoning later calls), a
    /// disconnected fake fails every op with [`PanelError::Disconnected`], and
    /// `set` clamps into `0..=100` just like the real backend.
    #[derive(Debug)]
    struct FakePanelTransport {
        current: u8,
        levels: Vec<u8>,
        connected: bool,
        errors: VecDeque<PanelError>,
    }

    impl FakePanelTransport {
        fn new() -> Self {
            Self {
                current: 50,
                levels: (0..=10u8).map(|n| n.saturating_mul(10)).collect(),
                connected: true,
                errors: VecDeque::new(),
            }
        }

        fn disconnected() -> Self {
            let mut fake = Self::new();
            fake.connected = false;
            fake
        }

        fn push_error(&mut self, err: PanelError) {
            self.errors.push_back(err);
        }

        /// The scenario gate shared by every operation.
        fn gate(&mut self) -> Result<(), PanelError> {
            if let Some(err) = self.errors.pop_front() {
                return Err(err);
            }
            if !self.connected {
                return Err(PanelError::Disconnected);
            }
            Ok(())
        }
    }

    impl PanelTransport for FakePanelTransport {
        fn query(&mut self) -> Result<PanelBrightness, PanelError> {
            self.gate()?;
            Ok(PanelBrightness {
                current: self.current,
                levels: self.levels.clone(),
            })
        }

        fn set_brightness(&mut self, percent: u8) -> Result<(), PanelError> {
            self.gate()?;
            self.current = percent.min(100);
            Ok(())
        }
    }

    fn factory(scenario: Scenario) -> PanelController<FakePanelTransport> {
        let transport = match scenario {
            Scenario::Nominal => FakePanelTransport::new(),
            Scenario::Disconnected => FakePanelTransport::disconnected(),
            Scenario::ErrorThenOk => {
                let mut fake = FakePanelTransport::new();
                fake.push_error(PanelError::Timeout);
                fake
            }
        };
        PanelController::new(transport)
    }

    #[test]
    fn panel_controller_satisfies_contract() {
        run_controller_contract(factory, 0);
    }

    #[test]
    fn probe_reports_brightness_only_hardware_backed_caps() {
        let mut controller = factory(Scenario::Nominal);
        let caps = controller.probe().unwrap();
        assert!(caps.supports(Feature::Brightness));
        assert!(!caps.supports(Feature::Contrast));
        assert!(!caps.supports(Feature::InputSource));
        assert!(caps.hardware_range);
        assert_eq!(caps.raw_capabilities, None);
    }

    #[test]
    fn contrast_and_input_source_are_unsupported() {
        let mut controller = factory(Scenario::Nominal);
        for feature in [Feature::Contrast, Feature::InputSource] {
            assert!(matches!(
                controller.get(feature),
                Err(ControlError::Unsupported)
            ));
            assert!(matches!(
                controller.set(feature, 10),
                Err(ControlError::Unsupported)
            ));
        }
    }

    #[test]
    fn brightness_range_is_percent_domain() {
        let mut controller = factory(Scenario::Nominal);
        let range = controller.get(Feature::Brightness).unwrap();
        assert_eq!(range.max, 100);
        assert_eq!(range.current, 50);
    }

    #[test]
    fn set_roundtrips_within_percent_domain() {
        let mut controller = factory(Scenario::Nominal);
        controller.set(Feature::Brightness, 37).unwrap();
        assert_eq!(controller.get(Feature::Brightness).unwrap().current, 37);
    }

    #[test]
    fn out_of_range_set_is_clamped_to_100() {
        let mut controller = factory(Scenario::Nominal);
        // The contract accepts reject-or-clamp; this backend clamps.
        controller.set(Feature::Brightness, u16::MAX).unwrap();
        let range = controller.get(Feature::Brightness).unwrap();
        assert_eq!(range.current, 100);
        assert!(range.current <= range.max);
    }

    #[test]
    fn disconnected_panel_reports_disconnected() {
        let mut controller = factory(Scenario::Disconnected);
        assert!(matches!(
            controller.get(Feature::Brightness),
            Err(ControlError::Disconnected)
        ));
        assert!(matches!(
            controller.set(Feature::Brightness, 20),
            Err(ControlError::Disconnected)
        ));
    }
}

//! A scriptable [`crate::controller::BrightnessController`] fake: per-feature
//! values, an injectable error queue, a latency marker, and a call log.

// ---- specs first (TDD); implementation follows in the next commit ----

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
        assert!(matches!(c.get(Feature::Brightness), Err(ControlError::Timeout)));
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

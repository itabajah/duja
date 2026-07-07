//! The [`BrightnessController`] trait every OS backend implements, and the
//! backend-agnostic [`ControlError`] it surfaces.

// ---- specs first (TDD); implementation follows in the next commit ----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Capabilities, Feature, FeatureRange};
    use std::error::Error;
    use std::fmt;

    #[derive(Debug)]
    struct Boom;
    impl fmt::Display for Boom {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("boom")
        }
    }
    impl Error for Boom {}

    #[test]
    fn control_error_display_messages() {
        assert_eq!(
            ControlError::Disconnected.to_string(),
            "display is disconnected"
        );
        assert_eq!(
            ControlError::Unsupported.to_string(),
            "feature is not supported by this display"
        );
        assert_eq!(ControlError::Timeout.to_string(), "control operation timed out");
    }

    #[test]
    fn backend_error_wraps_and_exposes_source() {
        let err = ControlError::backend(Boom);
        assert!(err.to_string().contains("boom"));
        assert!(err.source().is_some());
    }

    #[test]
    fn control_error_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ControlError>();
    }

    #[derive(Debug, Default)]
    struct Stub {
        level: u16,
    }
    impl BrightnessController for Stub {
        fn probe(&mut self) -> Result<Capabilities, ControlError> {
            Ok(Capabilities {
                features: [Feature::Brightness].into_iter().collect(),
                hardware_range: true,
                raw_capabilities: None,
            })
        }
        fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError> {
            match feature {
                Feature::Brightness => Ok(FeatureRange {
                    current: self.level,
                    max: 100,
                }),
                _ => Err(ControlError::Unsupported),
            }
        }
        fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
            match feature {
                Feature::Brightness => {
                    self.level = value.min(100);
                    Ok(())
                }
                _ => Err(ControlError::Unsupported),
            }
        }
    }

    #[test]
    fn trait_is_object_safe_and_roundtrips() {
        let mut c: Box<dyn BrightnessController> = Box::new(Stub::default());
        assert!(c.probe().is_ok());
        c.set(Feature::Brightness, 55).unwrap();
        assert_eq!(c.get(Feature::Brightness).unwrap().current, 55);
        assert!(matches!(
            c.set(Feature::Contrast, 10),
            Err(ControlError::Unsupported)
        ));
    }

    #[test]
    fn boxed_controller_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Box<dyn BrightnessController>>();
    }
}

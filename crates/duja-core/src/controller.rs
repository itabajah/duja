//! The [`BrightnessController`] trait every OS backend implements, and the
//! backend-agnostic [`ControlError`] it surfaces.

use std::error::Error;

use crate::model::{Capabilities, Feature, FeatureRange};

/// A backend-agnostic failure from a display-control operation.
///
/// Per-crate backend errors (`DdcError`, `PanelError`, …) cross into
/// [`ControlError::Backend`] at the trait boundary.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The display is no longer connected (unplugged, powered off, over RDP).
    #[error("display is disconnected")]
    Disconnected,
    /// The display does not support the requested feature.
    #[error("feature is not supported by this display")]
    Unsupported,
    /// The operation did not complete within the backend's deadline.
    #[error("control operation timed out")]
    Timeout,
    /// An opaque backend-specific error.
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn Error + Send + Sync>),
}

impl ControlError {
    /// Wrap any `Send + Sync` error as a [`ControlError::Backend`].
    pub fn backend<E>(err: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        ControlError::Backend(err.into())
    }
}

/// A per-display handle for reading and writing VCP features.
///
/// `&mut self` is deliberate: it makes per-monitor serialization a
/// compile-time property (each DDC worker exclusively owns its controller, so
/// no locking is required). `Send + Debug` let a controller be moved onto its
/// worker thread and logged.
pub trait BrightnessController: Send + std::fmt::Debug {
    /// Probe the display's capabilities (caps string + quirk merge).
    ///
    /// # Errors
    /// Returns [`ControlError`] if the display cannot be reached or its
    /// capabilities cannot be read.
    fn probe(&mut self) -> Result<Capabilities, ControlError>;

    /// Read the current value and maximum of `feature`.
    ///
    /// # Errors
    /// [`ControlError::Unsupported`] if the feature is unavailable,
    /// [`ControlError::Disconnected`] / [`ControlError::Timeout`] / other
    /// [`ControlError`] on failure.
    fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError>;

    /// Write `value` to `feature`.
    ///
    /// # Errors
    /// [`ControlError::Unsupported`] if the feature is unavailable, or another
    /// [`ControlError`] if the write cannot be completed.
    fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError>;
}

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
        assert_eq!(
            ControlError::Timeout.to_string(),
            "control operation timed out"
        );
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

//! [`PanelError`], the panel backend's error type, and its lowering into the
//! backend-agnostic [`duja_core::controller::ControlError`].

use duja_core::controller::ControlError;

/// A failure from the internal-panel backend.
///
/// These cross into [`ControlError`] at the [`crate::PanelController`] trait
/// boundary via the [`From`] impl below: a vanished panel becomes
/// [`ControlError::Disconnected`], a deadline becomes [`ControlError::Timeout`],
/// and everything else (a WMI/COM fault, malformed data) becomes an opaque
/// [`ControlError::Backend`]. The mere *absence* of the WMI class or of any
/// panel instance is **not** modelled here as an error — [`crate::enumerate`]
/// reports that as an empty list, the expected state on a desktop.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PanelError {
    /// The panel is no longer reachable (removed, powered off, session locked).
    #[error("the internal panel is no longer reachable")]
    Disconnected,
    /// A panel operation did not complete within the backend's deadline.
    #[error("the panel brightness operation timed out")]
    Timeout,
    /// A WMI/COM call failed. `context` names the failing step; `hresult` is the
    /// raw `HRESULT` for diagnosis.
    #[error("WMI failure in {context}: 0x{hresult:08X}")]
    Wmi {
        /// The COM/WMI step that failed (e.g. `"ConnectServer"`).
        context: &'static str,
        /// The raw `HRESULT` returned by the failing call.
        hresult: i32,
    },
    /// WMI returned data that did not match the expected shape.
    #[error("WMI returned malformed data: {0}")]
    Malformed(&'static str),
}

impl From<PanelError> for ControlError {
    fn from(err: PanelError) -> Self {
        match err {
            PanelError::Disconnected => ControlError::Disconnected,
            PanelError::Timeout => ControlError::Timeout,
            other => ControlError::backend(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_and_timeout_map_to_their_control_errors() {
        assert!(matches!(
            ControlError::from(PanelError::Disconnected),
            ControlError::Disconnected
        ));
        assert!(matches!(
            ControlError::from(PanelError::Timeout),
            ControlError::Timeout
        ));
    }

    #[test]
    fn wmi_and_malformed_map_to_backend() {
        assert!(matches!(
            ControlError::from(PanelError::Wmi {
                context: "ExecQuery",
                hresult: -2_147_217_407,
            }),
            ControlError::Backend(_)
        ));
        assert!(matches!(
            ControlError::from(PanelError::Malformed("no CurrentBrightness")),
            ControlError::Backend(_)
        ));
    }

    #[test]
    fn backend_error_preserves_display_text() {
        let err = ControlError::from(PanelError::Wmi {
            context: "ConnectServer",
            hresult: 0x8004_1002_u32.cast_signed(),
        });
        assert!(err.to_string().contains("ConnectServer"));
    }
}

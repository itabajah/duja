//! Launch-at-login registration (the in-house autostart trait, plan §8).
//!
//! On Windows, "start with the OS" is a value under the per-user *Run* key:
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, name `Duja`, data the
//! quoted path to the current executable. Enabling autostart writes that value;
//! disabling deletes it; querying tests whether it is present. Duja owns this
//! directly rather than pulling in an `auto-launch` dependency.
//!
//! # Structure
//!
//! - [`Autostart`] is the seam the app drives: `is_enabled` / `set_enabled`.
//! - The Windows implementation ([`system`]) is generic over a small
//!   `RegistryStore` value seam, so its enable/disable/query *logic* is unit
//!   tested against an in-memory fake, while a single live test proves the real
//!   Win32 registry FFI round-trips under a throwaway scratch key.
//! - The `Duja = "<quoted exe>"` command string is composed by the pure
//!   [`run_command`], tested without touching the registry.
//! - Non-Windows targets get a stub whose every operation reports
//!   [`AutostartError::Unsupported`].
//!
//! A `FakeAutostart` is exposed to downstream tests (this crate's tests and,
//! via the `test-support` feature, the app/UI) so wiring can be exercised
//! without writing to the real registry.

use std::path::Path;

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{WindowsAutostart, system};

#[cfg(not(windows))]
pub use stub::{StubAutostart, system};

/// Launch-at-login registration.
///
/// Implementors persist a single boolean — "does Duja start with the OS?" —
/// in whatever per-user store the platform uses. Both methods are fallible:
/// the underlying store (the Windows registry) can refuse access.
pub trait Autostart {
    /// Whether Duja is currently registered to launch at login.
    ///
    /// # Errors
    /// [`AutostartError`] if the backing store cannot be read.
    fn is_enabled(&self) -> Result<bool, AutostartError>;

    /// Enable (`on = true`) or disable (`on = false`) launch at login.
    ///
    /// Idempotent: enabling when already enabled rewrites the value; disabling
    /// when already disabled is a no-op success.
    ///
    /// # Errors
    /// [`AutostartError`] if the backing store cannot be written.
    fn set_enabled(&mut self, on: bool) -> Result<(), AutostartError>;
}

/// A failure registering or querying launch-at-login.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// Autostart is not supported on this platform/build.
    #[error("autostart is not supported on this platform")]
    Unsupported,
    /// The current executable path could not be resolved.
    #[error("could not resolve the current executable path: {0}")]
    ExePath(String),
    /// A registry (or equivalent store) operation failed.
    #[error("registry operation failed: {0}")]
    Registry(String),
}

/// The Run-key value name Duja registers under.
pub const VALUE_NAME: &str = "Duja";

/// Compose the Run-key command string for an executable path: the path,
/// always double-quoted so a space in the path (e.g. `C:\Program Files\…`)
/// does not split the command Windows parses from the value.
///
/// Pure and platform-independent, so the composition is unit-tested without a
/// registry. Any embedded double quotes are stripped first (a path cannot
/// legally contain them, and leaving them would let the value break out of its
/// own quoting).
#[must_use]
pub fn run_command(exe: &Path) -> String {
    let raw = exe.to_string_lossy();
    let sanitized = raw.replace('"', "");
    format!("\"{sanitized}\"")
}

/// An in-memory [`Autostart`] fake for downstream tests.
///
/// Holds the enabled flag in memory and never touches any OS store. An optional
/// forced error lets tests exercise the failure branch of the wiring that
/// drives an [`Autostart`]. Available to this crate's tests and, via the
/// `test-support` feature, to downstream crates' tests; never in a release.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct FakeAutostart {
    enabled: bool,
    fail: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeAutostart {
    /// A fake that starts disabled and always succeeds.
    #[must_use]
    pub fn new() -> Self {
        FakeAutostart {
            enabled: false,
            fail: false,
        }
    }

    /// A fake pre-seeded to `enabled`.
    #[must_use]
    pub fn with_enabled(enabled: bool) -> Self {
        FakeAutostart {
            enabled,
            fail: false,
        }
    }

    /// Make every operation fail with [`AutostartError::Registry`] (to test the
    /// caller's error handling).
    #[must_use]
    pub fn failing() -> Self {
        FakeAutostart {
            enabled: false,
            fail: true,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Autostart for FakeAutostart {
    fn is_enabled(&self) -> Result<bool, AutostartError> {
        if self.fail {
            return Err(AutostartError::Registry("fake failure".to_owned()));
        }
        Ok(self.enabled)
    }

    fn set_enabled(&mut self, on: bool) -> Result<(), AutostartError> {
        if self.fail {
            return Err(AutostartError::Registry("fake failure".to_owned()));
        }
        self.enabled = on;
        Ok(())
    }
}

#[cfg(not(windows))]
mod stub {
    use super::{Autostart, AutostartError};

    /// A no-op autostart for platforms without a launch-at-login backend yet.
    ///
    /// Every operation reports [`AutostartError::Unsupported`]; the settings UI
    /// surfaces that as a disabled toggle.
    #[derive(Debug, Default)]
    pub struct StubAutostart;

    impl Autostart for StubAutostart {
        fn is_enabled(&self) -> Result<bool, AutostartError> {
            Err(AutostartError::Unsupported)
        }

        fn set_enabled(&mut self, _on: bool) -> Result<(), AutostartError> {
            Err(AutostartError::Unsupported)
        }
    }

    /// The platform autostart (a stub that reports "unsupported" here).
    ///
    /// # Errors
    /// Never fails on this target; the `Result` mirrors the Windows signature
    /// so the caller stays cfg-free.
    // RATIONALE (clippy::unnecessary_wraps): mirrors the Windows `system()`
    // signature so the caller stays cfg-free.
    #[allow(clippy::unnecessary_wraps)]
    pub fn system() -> Result<StubAutostart, AutostartError> {
        Ok(StubAutostart)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_command_quotes_the_path() {
        let cmd = run_command(&PathBuf::from(r"C:\Program Files\Duja\duja.exe"));
        assert_eq!(cmd, "\"C:\\Program Files\\Duja\\duja.exe\"");
    }

    #[test]
    fn run_command_strips_stray_quotes() {
        // A path cannot legally contain quotes; if one appears we must not let it
        // break out of the surrounding quoting — only the two wrapping quotes
        // survive.
        let cmd = run_command(&PathBuf::from("C:\\a\"b\\duja.exe"));
        assert_eq!(cmd, "\"C:\\ab\\duja.exe\"");
        assert_eq!(cmd.matches('"').count(), 2);
    }

    #[test]
    fn fake_round_trips_enabled_state() {
        let mut fake = FakeAutostart::new();
        assert!(!fake.is_enabled().expect("query"));
        fake.set_enabled(true).expect("enable");
        assert!(fake.is_enabled().expect("query"));
        fake.set_enabled(false).expect("disable");
        assert!(!fake.is_enabled().expect("query"));
    }

    #[test]
    fn fake_with_enabled_seeds_state() {
        assert!(
            FakeAutostart::with_enabled(true)
                .is_enabled()
                .expect("query")
        );
    }

    #[test]
    fn failing_fake_errors_on_every_op() {
        let mut fake = FakeAutostart::failing();
        assert!(matches!(
            fake.is_enabled(),
            Err(AutostartError::Registry(_))
        ));
        assert!(matches!(
            fake.set_enabled(true),
            Err(AutostartError::Registry(_))
        ));
    }
}

//! A statically-named, held-for-life mutex the Windows installer can detect.
//!
//! Duja's tray window only *hides* on close, and the per-user single-instance
//! mutex ([`crate::SingleInstance`]) embeds the user's SID in its name
//! (`Local\duja-<sid>`) — a name Inno Setup's `AppMutex` directive cannot
//! express. So an in-place upgrade or uninstall over a running Duja could not
//! detect the live process: it would silently fail, or leave the old `duja.exe`
//! running against a freshly written `dujactl.exe` (a version skew).
//!
//! [`InstallerGuard`] closes that gap with the *minimal correct* primitive: the
//! app creates a mutex under a FIXED, well-known name and holds the handle open
//! for its whole lifetime. The same name is hard-coded in
//! `packaging/windows/duja.iss` (`AppMutex=Local\duja-installer-guard`), so the
//! installer detects a running instance and prompts the user to close it before
//! installing. The guard is *only ever held*, never inspected — the installer
//! does the detecting. It is an **additional**, detection-only mutex: it does
//! not replace the per-user single-instance guard, which still enforces
//! one-running-instance.
//!
//! # Follow-up
//!
//! Seamless auto-close — the installer asking a running Duja to exit via the
//! Restart Manager (`RmRegisterResources` + `WM_QUERYENDSESSION`) rather than
//! only *prompting* — is a documented follow-up, not part of this change.
//!
//! On non-Windows targets there is no such installer, so the guard is a no-op.

#[cfg(windows)]
pub use imp::InstallerGuard;

#[cfg(not(windows))]
pub use stub::InstallerGuard;

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::PCWSTR;

    /// The fixed, well-known mutex name the app holds and the Windows installer
    /// (`packaging/windows/duja.iss`) hard-codes in its `AppMutex` directive.
    ///
    /// This MUST stay byte-identical to the `.iss` value or the installer will
    /// not detect a running instance. It is deliberately a *fixed* string —
    /// unlike the per-user single-instance name, which embeds the SID and so
    /// cannot appear verbatim in an Inno directive.
    const INSTALLER_GUARD_MUTEX_NAME: &str = r"Local\duja-installer-guard";

    /// A held installer-detection mutex. Closing it (on drop) releases this
    /// process's handle to the well-known name.
    pub struct InstallerGuard {
        handle: HANDLE,
    }

    impl InstallerGuard {
        /// Create and hold the installer-detection mutex.
        ///
        /// The returned guard must be kept alive for as long as the app should
        /// be detectable by the installer (i.e. its whole lifetime). Creation
        /// failure (rare) degrades to an invalid handle — the app still runs;
        /// only the installer's running-instance detection is lost.
        #[must_use]
        pub fn acquire() -> Self {
            let wide: Vec<u16> = INSTALLER_GUARD_MUTEX_NAME
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `wide` is a NUL-terminated wide string that outlives the
            // call. `CreateMutexW` returns an owned handle we hold (and close on
            // drop). This guard exists only to *hold the name open* so the
            // installer's `AppMutex` check can see it, so we request no
            // ownership (`binitialowner = false`) and never inspect the last
            // error — mirroring the `CreateMutexW` shape in `single_instance`. A
            // rare creation failure degrades to an invalid handle
            // (`HANDLE::default()`) so the app still runs.
            let handle =
                unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }.unwrap_or_default();
            InstallerGuard { handle }
        }

        /// Whether the guard actually holds a live mutex handle (i.e. the create
        /// succeeded). Exposed for tests/diagnostics; the installer detects the
        /// name itself and never calls this.
        #[must_use]
        pub fn is_held(&self) -> bool {
            !self.handle.is_invalid()
        }
    }

    impl Drop for InstallerGuard {
        fn drop(&mut self) {
            if !self.handle.is_invalid() {
                // SAFETY: `handle` came from `CreateMutexW` above and is owned
                // solely by this guard; closing it once releases our handle to
                // the named mutex.
                unsafe {
                    let _ = CloseHandle(self.handle);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn acquire_holds_the_named_mutex() {
            let guard = InstallerGuard::acquire();
            assert!(
                guard.is_held(),
                "the installer guard must hold a live handle"
            );
            // A second acquire also succeeds: a named mutex allows many open
            // handles, so holding it never wedges a second caller (and the app
            // holding it never blocks the installer's own detection open).
            let second = InstallerGuard::acquire();
            assert!(second.is_held());
        }

        #[test]
        fn guard_name_is_the_fixed_installer_string() {
            // Pinned so a rename cannot silently drift from duja.iss's AppMutex
            // value (they must be byte-identical).
            assert_eq!(INSTALLER_GUARD_MUTEX_NAME, r"Local\duja-installer-guard");
        }
    }
}

#[cfg(not(windows))]
mod stub {
    /// A no-op installer guard: there is no Windows installer to detect it.
    pub struct InstallerGuard {
        _private: (),
    }

    impl InstallerGuard {
        /// No-op on non-Windows; returns a guard that holds nothing.
        #[must_use]
        pub fn acquire() -> Self {
            InstallerGuard { _private: () }
        }

        /// Always `false` off Windows (nothing is held).
        #[must_use]
        pub fn is_held(&self) -> bool {
            false
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stub_guard_holds_nothing() {
            assert!(!InstallerGuard::acquire().is_held());
        }
    }
}

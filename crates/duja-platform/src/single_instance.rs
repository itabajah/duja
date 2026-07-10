//! Single-instance enforcement via a per-user named mutex.
//!
//! Duja is a tray application: a second launch must not start a second engine,
//! tray icon, or set of overlays. [`SingleInstance::acquire`] creates a named
//! kernel mutex in the session-local namespace (`Local\duja-<sid>`, where
//! `<sid>` is a stable per-user disambiguator). The **first** process to create
//! the name owns it; any later process sees `ERROR_ALREADY_EXISTS` and learns it
//! is not first ([`already_running`](SingleInstance::already_running)).
//!
//! The guard *holds the mutex handle open* for the process lifetime; dropping it
//! closes the handle and releases the name, so a clean exit lets the next launch
//! become the owner. (P4 has the second instance simply exit 0; the IPC
//! "show the flyout of the running instance" handshake lands in P5.)
//!
//! On non-Windows targets the guard is a no-op that always reports "first"
//! (there is no tray app there yet; real backends land in P6/P7).

#[cfg(windows)]
pub use imp::SingleInstance;

#[cfg(not(windows))]
pub use stub::SingleInstance;

#[cfg(windows)]
mod imp {
    // RATIONALE (clippy::cast_possible_truncation): the SID length is a small
    // fixed-size value (≤ 68 bytes for any real SID) and token buffer lengths
    // are supplied by the API itself, so the usize/u32 casts cannot truncate.
    #![allow(clippy::cast_possible_truncation)]
    // RATIONALE (clippy::borrow_as_ptr): passing `&mut out_param` to a Win32
    // function that takes a raw pointer is the idiomatic FFI call shape; the
    // borrow lives exactly for the synchronous call.
    #![allow(clippy::borrow_as_ptr)]

    use std::fmt;

    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows::Win32::Security::{
        GetLengthSid, GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
    use windows::core::PCWSTR;

    /// A held single-instance mutex. Closing it (on drop) releases the name.
    pub struct SingleInstance {
        handle: HANDLE,
        already_running: bool,
    }

    impl fmt::Debug for SingleInstance {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("SingleInstance")
                .field("already_running", &self.already_running)
                .finish_non_exhaustive()
        }
    }

    impl SingleInstance {
        /// Acquire the process-wide Duja single-instance mutex.
        ///
        /// Returns a guard whose [`already_running`](Self::already_running) tells
        /// the caller whether another instance already held the name. The guard
        /// must be kept alive for as long as this instance should hold the name.
        #[must_use]
        pub fn acquire() -> Self {
            Self::acquire_named(&mutex_name())
        }

        /// Acquire a mutex under an explicit fully-qualified name (test seam).
        #[must_use]
        pub(crate) fn acquire_named(name: &str) -> Self {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: `wide` is a valid NUL-terminated wide string that outlives
            // the call. `CreateMutexW` returns an owned handle (closed on drop)
            // or an error; we read `GetLastError` immediately after so a
            // pre-existing name is observed as ERROR_ALREADY_EXISTS.
            let (handle, already_running) = unsafe {
                match CreateMutexW(None, false, PCWSTR(wide.as_ptr())) {
                    Ok(h) => (h, GetLastError() == ERROR_ALREADY_EXISTS),
                    // Creation itself failed (rare): degrade to "first" so the
                    // app still runs rather than refusing to start.
                    Err(_) => (HANDLE::default(), false),
                }
            };
            SingleInstance {
                handle,
                already_running,
            }
        }

        /// Whether another instance already held the name when this guard was
        /// acquired.
        #[must_use]
        pub fn already_running(&self) -> bool {
            self.already_running
        }
    }

    impl Drop for SingleInstance {
        fn drop(&mut self) {
            if !self.handle.is_invalid() {
                // SAFETY: `handle` came from `CreateMutexW` above and is owned
                // solely by this guard; closing it releases our reference to the
                // named mutex exactly once.
                unsafe {
                    let _ = CloseHandle(self.handle);
                }
            }
        }
    }

    /// The fully-qualified mutex name: session-local, per-user.
    fn mutex_name() -> String {
        format!(
            "Local\\duja-{}",
            current_user_sid().unwrap_or_else(|| "anon".to_owned())
        )
    }

    /// A stable per-user disambiguator: the current process token's user SID,
    /// hex-encoded. `None` if the token or SID cannot be read (the caller then
    /// falls back to a constant — the `Local\` namespace is already per-session).
    fn current_user_sid() -> Option<String> {
        // SAFETY: `GetCurrentProcess` is a pseudo-handle needing no close.
        // `OpenProcessToken` writes an owned token handle we close below.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
            let sid_hex = read_token_sid_hex(token);
            let _ = CloseHandle(token);
            sid_hex
        }
    }

    /// Read the `TokenUser` SID for an opened token and hex-encode its bytes.
    ///
    /// # Safety
    /// `token` must be a valid token handle opened with `TOKEN_QUERY`.
    unsafe fn read_token_sid_hex(token: HANDLE) -> Option<String> {
        let mut len = 0u32;
        // First call sizes the buffer; it "fails" with the required length in
        // `len`, which is the documented probing convention.
        // SAFETY: passing a null buffer with length 0 is the sizing call.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
        if len == 0 {
            return None;
        }
        // An over-aligned (`u64`) buffer so the later `TOKEN_USER` view never
        // *increases* the pointer alignment (u64 and TOKEN_USER are both 8-aligned).
        let words = (len as usize).div_ceil(8);
        let mut buf = vec![0u64; words];
        // SAFETY: `buf` is at least `len` bytes; the call fills it with a
        // TOKEN_USER whose embedded PSID points inside `buf`.
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                len,
                &mut len,
            )
            .ok()?;
        }
        // SAFETY: `buf` now holds a well-formed, 8-aligned TOKEN_USER; read its
        // SID pointer.
        let sid: PSID = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        if sid.is_invalid() {
            return None;
        }
        // SAFETY: `sid` is a valid PSID living inside `buf`; GetLengthSid returns
        // its byte length, which bounds the slice we read.
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        if sid_len == 0 {
            return None;
        }
        // SAFETY: `sid` points to `sid_len` valid bytes inside `buf`.
        let bytes = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), sid_len) };
        let mut hex = String::with_capacity(sid_len.saturating_mul(2));
        for b in bytes {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
        }
        Some(hex)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn second_acquire_of_same_name_detects_the_first() {
            // A unique name per test run so we never collide with a real running
            // instance or a parallel test.
            let name = format!("Local\\duja-test-{}", std::process::id());
            let first = SingleInstance::acquire_named(&name);
            assert!(
                !first.already_running(),
                "first holder is not 'already running'"
            );

            let second = SingleInstance::acquire_named(&name);
            assert!(
                second.already_running(),
                "second holder must detect the first"
            );

            // Dropping both releases the name; a fresh acquire is 'first' again.
            drop(second);
            drop(first);
            let third = SingleInstance::acquire_named(&name);
            assert!(
                !third.already_running(),
                "after release the name is free again"
            );
        }

        #[test]
        fn real_name_is_session_local_and_per_user() {
            let name = mutex_name();
            assert!(name.starts_with("Local\\duja-"), "name = {name}");
        }

        #[test]
        fn acquire_returns_a_usable_guard() {
            let guard = SingleInstance::acquire();
            // Whatever the outcome, the accessor is callable and Debug works.
            let _ = guard.already_running();
            assert!(format!("{guard:?}").contains("SingleInstance"));
        }
    }
}

#[cfg(not(windows))]
mod stub {
    /// A no-op single-instance guard for platforms without a tray app yet.
    #[derive(Debug)]
    pub struct SingleInstance {
        _private: (),
    }

    impl SingleInstance {
        /// Always reports "first" — there is no second-instance detection here.
        #[must_use]
        pub fn acquire() -> Self {
            SingleInstance { _private: () }
        }

        /// Always `false` on non-Windows targets.
        #[must_use]
        pub fn already_running(&self) -> bool {
            false
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stub_never_reports_already_running() {
            assert!(!SingleInstance::acquire().already_running());
        }
    }
}

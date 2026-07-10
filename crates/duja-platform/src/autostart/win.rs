//! The Windows autostart backend: the per-user *Run* key.
//!
//! [`WindowsAutostart`] is [`RegistryAutostart`] over a real [`RunKey`] store
//! (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`). The enable / disable
//! / query *logic* lives in the store-generic [`RegistryAutostart`] and is unit
//! tested against an in-memory [`FakeRegistry`]; the actual Win32 registry FFI
//! is proved by one live test that round-trips a value under a throwaway
//! `HKCU\Software\DujaTest\<pid>` scratch key (never the real Run key).

// RATIONALE (clippy::cast_possible_truncation): registry blob sizes are small,
// bounded values the API itself reports; the usize↔u32 casts here cannot
// truncate a real Run-key string (a path is far under 4 GiB).
#![allow(clippy::cast_possible_truncation)]
// RATIONALE (clippy::borrow_as_ptr): passing `&mut out_param` to a Win32 function
// that takes a raw pointer is the idiomatic FFI call shape; each borrow lives
// exactly for the synchronous call (the same allowance single_instance uses).
#![allow(clippy::borrow_as_ptr)]

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};
use windows::core::PCWSTR;

use super::{Autostart, AutostartError, VALUE_NAME, run_command};

/// The per-user Run key path. Values here are launched at each interactive
/// logon.
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// A named-string-value store: read, write, or delete a single value.
///
/// This is the seam that lets the autostart *logic* be tested without the
/// registry. The real implementation ([`RunKey`]) opens a registry key; the
/// test double ([`FakeRegistry`]) is an in-memory map.
trait RegistryStore {
    /// Read the string value `name`, or `None` if it (or the key) is absent.
    fn read(&self, name: &str) -> Result<Option<String>, AutostartError>;
    /// Write `data` to the string value `name`, creating the key if needed.
    fn write(&mut self, name: &str, data: &str) -> Result<(), AutostartError>;
    /// Delete the value `name`; succeeds even if it is already absent.
    fn delete(&mut self, name: &str) -> Result<(), AutostartError>;
}

/// [`Autostart`] over any [`RegistryStore`]: enabled ⇔ the `Duja` value exists.
struct RegistryAutostart<S: RegistryStore> {
    store: S,
    /// The value name and the command it should carry when enabled.
    value_name: String,
    command: String,
}

impl<S: RegistryStore> RegistryAutostart<S> {
    fn new(store: S, value_name: String, command: String) -> Self {
        RegistryAutostart {
            store,
            value_name,
            command,
        }
    }
}

impl<S: RegistryStore> Autostart for RegistryAutostart<S> {
    fn is_enabled(&self) -> Result<bool, AutostartError> {
        Ok(self.store.read(&self.value_name)?.is_some())
    }

    fn set_enabled(&mut self, on: bool) -> Result<(), AutostartError> {
        if on {
            self.store.write(&self.value_name, &self.command)
        } else {
            self.store.delete(&self.value_name)
        }
    }
}

/// The Windows launch-at-login backend (the real Run key).
pub struct WindowsAutostart(RegistryAutostart<RunKey>);

impl Autostart for WindowsAutostart {
    fn is_enabled(&self) -> Result<bool, AutostartError> {
        self.0.is_enabled()
    }

    fn set_enabled(&mut self, on: bool) -> Result<(), AutostartError> {
        self.0.set_enabled(on)
    }
}

/// Build the system autostart backend, resolving the current executable path
/// for the command it would register.
///
/// # Errors
/// [`AutostartError::ExePath`] if the current executable path cannot be
/// resolved (it is captured now so `set_enabled(true)` cannot silently register
/// a wrong path later).
pub fn system() -> Result<WindowsAutostart, AutostartError> {
    let exe = std::env::current_exe().map_err(|e| AutostartError::ExePath(e.to_string()))?;
    Ok(WindowsAutostart(RegistryAutostart::new(
        RunKey::new(RUN_SUBKEY),
        VALUE_NAME.to_owned(),
        run_command(&exe),
    )))
}

/// A registry store rooted at `HKCU\<subkey>`.
///
/// Each operation opens (or creates) the key, acts, and closes it — the key
/// handle is never held across calls, so there is nothing to leak and no
/// lifetime to thread through the seam.
struct RunKey {
    subkey: String,
}

impl RunKey {
    fn new(subkey: &str) -> Self {
        RunKey {
            subkey: subkey.to_owned(),
        }
    }
}

impl RegistryStore for RunKey {
    fn read(&self, name: &str) -> Result<Option<String>, AutostartError> {
        let Some(key) = OpenedKey::open(&self.subkey, false)? else {
            // The key itself is absent ⇒ the value cannot be present.
            return Ok(None);
        };
        key.query_string(name)
    }

    fn write(&mut self, name: &str, data: &str) -> Result<(), AutostartError> {
        let key = OpenedKey::create(&self.subkey)?;
        key.set_string(name, data)
    }

    fn delete(&mut self, name: &str) -> Result<(), AutostartError> {
        let Some(key) = OpenedKey::open(&self.subkey, true)? else {
            // No key ⇒ nothing to delete; disabling is already the state.
            return Ok(());
        };
        key.delete_value(name)
    }
}

/// An owned, RAII-closed `HKEY`.
struct OpenedKey(HKEY);

impl OpenedKey {
    /// Open `HKCU\<subkey>`, returning `None` if the key does not exist.
    ///
    /// `for_write` selects the access rights (`KEY_SET_VALUE` for a mutating
    /// caller, else read access — [`RegQueryValueExW`] needs no explicit
    /// `KEY_QUERY_VALUE` beyond the default read rights `KEY_SET_VALUE` implies,
    /// so a write-access open can also query).
    fn open(subkey: &str, for_write: bool) -> Result<Option<Self>, AutostartError> {
        let wide = wide(subkey);
        let mut handle = HKEY::default();
        let access = if for_write {
            KEY_SET_VALUE
        } else {
            KEY_QUERY_VALUE
        };
        // SAFETY: `wide` is a NUL-terminated wide string that outlives the call;
        // `handle` receives an owned key handle we close in `Drop`. A missing key
        // returns ERROR_FILE_NOT_FOUND, handled below.
        let rc = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide.as_ptr()),
                None,
                access,
                &mut handle,
            )
        };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(rc, "RegOpenKeyExW")?;
        Ok(Some(OpenedKey(handle)))
    }

    /// Open (or create) `HKCU\<subkey>` with write access.
    fn create(subkey: &str) -> Result<Self, AutostartError> {
        let wide = wide(subkey);
        let mut handle = HKEY::default();
        // SAFETY: `wide` is a NUL-terminated wide string that outlives the call.
        // `handle` receives an owned key handle closed in `Drop`; the class,
        // security-attributes and disposition out-params are all optional/None.
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut handle,
                None,
            )
        };
        check(rc, "RegCreateKeyExW")?;
        Ok(OpenedKey(handle))
    }

    /// Query the string value `name`, or `None` when it is absent.
    fn query_string(&self, name: &str) -> Result<Option<String>, AutostartError> {
        let name_w = wide(name);
        let mut ty = REG_VALUE_TYPE::default();
        let mut len: u32 = 0;
        // First call sizes the buffer (data pointer null).
        // SAFETY: `name_w` is a NUL-terminated wide string; passing a null data
        // buffer with `len` receiving the required byte count is the documented
        // sizing convention.
        let rc = unsafe {
            RegQueryValueExW(
                self.0,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                None,
                Some(&mut len),
            )
        };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(rc, "RegQueryValueExW(size)")?;
        if len == 0 {
            return Ok(Some(String::new()));
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is `len` bytes; the call fills it and updates `len` with
        // the bytes written. `name_w` outlives the call.
        let rc = unsafe {
            RegQueryValueExW(
                self.0,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
        };
        check(rc, "RegQueryValueExW(read)")?;
        buf.truncate(len as usize);
        Ok(Some(decode_wide(&buf)))
    }

    /// Write `data` as a `REG_SZ` value under `name`.
    fn set_string(&self, name: &str, data: &str) -> Result<(), AutostartError> {
        let name_w = wide(name);
        let data_w = wide(data);
        // The registry blob is the UTF-16 units (including the NUL) as bytes.
        let byte_len = data_w.len().saturating_mul(2);
        // SAFETY: `data_w` is a live `[u16]`; reading it as `byte_len` (= len * 2)
        // bytes stays within the same allocation and every byte pattern is valid.
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(data_w.as_ptr().cast::<u8>(), byte_len) };
        // SAFETY: `name_w`/`bytes` outlive the synchronous call; REG_SZ matches
        // the wide-string blob we pass.
        let rc =
            unsafe { RegSetValueExW(self.0, PCWSTR(name_w.as_ptr()), None, REG_SZ, Some(bytes)) };
        check(rc, "RegSetValueExW")
    }

    /// Delete the value `name`, tolerating an already-absent value.
    fn delete_value(&self, name: &str) -> Result<(), AutostartError> {
        let name_w = wide(name);
        // SAFETY: `name_w` is a NUL-terminated wide string outliving the call.
        let rc = unsafe { RegDeleteValueW(self.0, PCWSTR(name_w.as_ptr())) };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(rc, "RegDeleteValueW")
    }
}

impl Drop for OpenedKey {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a `RegOpenKeyExW`/`RegCreateKeyExW` success
        // and is owned solely by this guard; closing it releases it exactly once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Encode `s` as a NUL-terminated UTF-16 vector for the `*W` registry APIs.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Decode a `REG_SZ` byte blob (UTF-16 LE, possibly NUL-terminated) to a string.
fn decode_wide(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        // `chunks_exact(2)` yields exactly-2-byte slices, so the array conversion
        // never fails; `unwrap_or` keeps the map panic-free for the lint wall.
        .map(|pair| <[u8; 2]>::try_from(pair).map_or(0, u16::from_le_bytes))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Map a Win32 registry return code to a `Result`.
fn check(rc: WIN32_ERROR, op: &str) -> Result<(), AutostartError> {
    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(AutostartError::Registry(format!(
            "{op} failed (code {})",
            rc.0
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// An in-memory registry value store: the seam under test for the logic.
    #[derive(Default)]
    struct FakeRegistry {
        values: BTreeMap<String, String>,
    }

    impl RegistryStore for FakeRegistry {
        fn read(&self, name: &str) -> Result<Option<String>, AutostartError> {
            Ok(self.values.get(name).cloned())
        }
        fn write(&mut self, name: &str, data: &str) -> Result<(), AutostartError> {
            self.values.insert(name.to_owned(), data.to_owned());
            Ok(())
        }
        fn delete(&mut self, name: &str) -> Result<(), AutostartError> {
            self.values.remove(name);
            Ok(())
        }
    }

    fn fake_autostart() -> RegistryAutostart<FakeRegistry> {
        RegistryAutostart::new(
            FakeRegistry::default(),
            VALUE_NAME.to_owned(),
            "\"C:\\Duja\\duja.exe\"".to_owned(),
        )
    }

    #[test]
    fn enable_then_query_reports_enabled() {
        let mut a = fake_autostart();
        assert!(!a.is_enabled().expect("query"));
        a.set_enabled(true).expect("enable");
        assert!(a.is_enabled().expect("query"));
        // The stored value is the quoted command.
        assert_eq!(
            a.store.values.get(VALUE_NAME).map(String::as_str),
            Some("\"C:\\Duja\\duja.exe\"")
        );
    }

    #[test]
    fn disable_removes_the_value() {
        let mut a = fake_autostart();
        a.set_enabled(true).expect("enable");
        a.set_enabled(false).expect("disable");
        assert!(!a.is_enabled().expect("query"));
        assert!(a.store.values.is_empty());
    }

    #[test]
    fn disable_when_absent_is_a_noop_success() {
        let mut a = fake_autostart();
        a.set_enabled(false).expect("disable-absent");
        assert!(!a.is_enabled().expect("query"));
    }

    #[test]
    fn enable_is_idempotent() {
        let mut a = fake_autostart();
        a.set_enabled(true).expect("enable");
        a.set_enabled(true).expect("enable-again");
        assert!(a.is_enabled().expect("query"));
    }

    #[test]
    fn decode_wide_trims_nul_terminator() {
        // "Hi" + NUL as UTF-16 LE bytes.
        let bytes = [0x48, 0x00, 0x69, 0x00, 0x00, 0x00];
        assert_eq!(decode_wide(&bytes), "Hi");
    }

    // --- one live FFI proof against a throwaway scratch key (NOT the Run key) ---

    #[test]
    fn live_registry_round_trip_under_scratch_key() {
        // A unique per-process subkey so parallel test runs never collide, and
        // so we never touch the real Run key.
        let subkey = format!(r"Software\DujaTest\autostart-{}", std::process::id());
        let mut store = RunKey::new(&subkey);

        // Absent → read is None.
        assert_eq!(store.read(VALUE_NAME).expect("read-absent"), None);

        // Write → read returns the exact string.
        let command = "\"C:\\Program Files\\Duja\\duja.exe\"";
        store.write(VALUE_NAME, command).expect("write");
        assert_eq!(
            store.read(VALUE_NAME).expect("read-present").as_deref(),
            Some(command)
        );

        // Delete → read is None again; deleting twice is fine.
        store.delete(VALUE_NAME).expect("delete");
        assert_eq!(store.read(VALUE_NAME).expect("read-after-delete"), None);
        store.delete(VALUE_NAME).expect("delete-again");

        // Clean up the scratch key we created.
        delete_scratch_key(&subkey);
    }

    /// Remove the throwaway `HKCU\<subkey>` created by the live test.
    fn delete_scratch_key(subkey: &str) {
        use windows::Win32::System::Registry::RegDeleteKeyW;
        let wide = wide(subkey);
        // SAFETY: `wide` is a NUL-terminated wide string outliving the call; we
        // delete only the process-unique scratch key we created above.
        unsafe {
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(wide.as_ptr()));
        }
    }
}

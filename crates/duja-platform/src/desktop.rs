//! Three small questions the tray asks the desktop environment, and one thing it
//! asks it to do.
//!
//! - [`open_url`] — hand a URL to the user's browser. Duja only ever opens the
//!   releases *page*; it never downloads anything.
//! - [`os_dark_theme`] — whether the OS is in dark mode, for the `System` theme
//!   setting.
//! - [`animations_enabled`] — whether the user wants motion, for the flyout's
//!   external-change slider glide.
//!
//! They live here rather than in the app binary for the reason `#87` hoisted the
//! tray geometry here: each one is a platform call, and this crate is where the
//! confined, audited FFI lives. Each is answered from the OS directly rather than
//! through winit/Slint. (winit 0.30 does expose `Window::theme()`, but it needs a
//! live window and Slint does not hand out the winit handle, so it is not reachable
//! from where Duja resolves its palette — before either window exists.)
//!
//! # Failure is never fatal
//!
//! Every entry point degrades to a documented default rather than an error: an
//! unopenable URL comes back as an `Err` carrying whatever code the platform gave,
//! for the caller to log; an unanswerable theme query is `None` (the caller
//! decides); and an unanswerable motion query is motion-**on**, matching both
//! platforms' own defaults. None of these is worth failing a launch over.
//!
//! # Purity
//!
//! Wherever the OS's raw answer needs *interpreting*, that step is a **pure
//! decoder** rather than an inline expression, so it is compiled and tested on
//! every CI lane and not only on the one that can run the query. That covers the
//! three real decisions: `AppsUseLightTheme == 0` meaning dark, an **absent**
//! `AppsUseLightTheme` meaning light, a missing `AppleInterfaceStyle` meaning
//! light, plus `ShellExecuteW`'s legacy "greater than 32" success rule.
//!
//! Not every arm has one, and that is the point of the rule rather than an
//! exception to it: macOS answers all three questions with a value that needs no
//! interpretation (a `bool` from `openURL`, a `bool` to negate for reduced motion),
//! and the placeholder arms return constants. A decoder exists where there is a
//! decision to get wrong.

/// A URL that could not be opened, carrying whatever the platform reported.
///
/// The code is kept rather than flattened to a bool because the log line is this
/// path's *only* observability: `SE_ERR_NOASSOC` (31, no browser registered),
/// `ERROR_FILE_NOT_FOUND` (2) and `SE_ERR_ACCESSDENIED` (5) are three different
/// user-visible situations that a bare "could not open" cannot tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenUrlFailure {
    /// The platform's own failure code where it has one — Windows'
    /// `ShellExecuteW` legacy return. `None` on platforms that report only
    /// success or failure, which is macOS.
    pub code: Option<u32>,
}

/// Open `url` in the user's default browser.
///
/// Best-effort and **non-blocking on the outcome**: `Ok` means the OS accepted the
/// request, not that a browser window is on screen. Callers log an `Err` rather
/// than treating it as fatal.
///
/// # Errors
/// [`OpenUrlFailure`] when the platform reported that it could not open the URL.
pub fn open_url(url: &str) -> Result<(), OpenUrlFailure> {
    platform::open_url(url)
}

/// Whether the OS is currently in dark mode.
///
/// `Some(true)` dark, `Some(false)` light, `None` when the platform genuinely has
/// no answer — the caller decides what unknown means, because the right default is
/// a UI question, not a platform one.
///
/// `None` means *the query failed*, and is deliberately narrow. In particular a
/// **missing** setting is not unknown on either platform: both express "light" by
/// the absence of a value, so both answer `Some(false)` there. Widening `None` to
/// cover absence is how the flyout ends up dark on a stock light-themed desktop.
#[must_use]
pub fn os_dark_theme() -> Option<bool> {
    platform::os_dark_theme()
}

/// Whether the OS wants UI animations.
///
/// `false` only when the user has explicitly asked the system for less motion
/// (Windows: Settings → Accessibility → Visual effects → "Animation effects";
/// macOS: System Settings → Accessibility → Display → "Reduce motion"). A failed
/// or unavailable query answers `true`, which is both platforms' own default.
#[must_use]
pub fn animations_enabled() -> bool {
    platform::animations_enabled()
}

// --- pure decoders ---------------------------------------------------------

/// What the registry had to say about `AppsUseLightTheme`.
///
/// Three states, not two, because **absent** and **unreadable** mean opposite
/// things and `RegGetValueW` reports them with different codes
/// (`ERROR_FILE_NOT_FOUND` vs e.g. `ERROR_UNSUPPORTED_TYPE`).
#[cfg(any(test, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppsUseLightTheme {
    /// The value was read. `0` = dark, non-zero = light (note the inversion).
    Value(u32),
    /// The key or the value does not exist.
    Absent,
    /// The read failed for some other reason — wrong type, access denied.
    Unreadable,
}

/// Decode Windows' `AppsUseLightTheme` registry value into [`os_dark_theme`]'s
/// answer.
///
/// The value is a `REG_DWORD` that is `0` for dark and non-zero for light — note
/// the **inversion**: the name asks about *light*, so `0` means dark.
///
/// # Absent is an answer, and it is the common case
///
/// Windows writes this value only once the user first changes the app-mode
/// setting, so on a stock profile it does **not exist** — verified on Windows 11
/// 26200, where `HKEY_USERS\.DEFAULT\…\Themes\Personalize` carries zero values.
/// Windows' own default app mode is Light, so absence deterministically means
/// light; it is not a coin flip and it is not a legacy-build quirk.
///
/// Reporting absence as `None` would therefore have left the majority cohort —
/// stock Windows, stock Duja config, where `ConfigTheme::System` is the serde
/// default — resolving to the caller's dark fallback while every other app on the
/// machine is light. That is the exact defect this query exists to fix, so absence
/// maps to `Some(false)` and only a genuine read failure is `None`.
#[cfg(any(test, windows))]
const fn dark_from_apps_use_light_theme(value: AppsUseLightTheme) -> Option<bool> {
    match value {
        AppsUseLightTheme::Value(v) => Some(v == 0),
        // Windows' default app mode is Light, and this value's absence *is* that
        // default rather than a missing opinion.
        AppsUseLightTheme::Absent => Some(false),
        AppsUseLightTheme::Unreadable => None,
    }
}

/// Decode `ShellExecuteW`'s return into "did the shell accept this?".
///
/// A legacy convention rather than a boolean: the function returns an
/// `HINSTANCE`-shaped value that is **greater than 32** on success, and one of the
/// `SE_ERR_*` / `ERROR_*` codes at or below 32 on failure. `>` and not `>=` — 32
/// itself is a failure — which is the whole reason this is a named, tested
/// function instead of an inline comparison.
#[cfg(any(test, windows))]
const fn shell_accepted(result: usize) -> bool {
    result > 32
}

/// Decode macOS' `AppleInterfaceStyle` user default into [`os_dark_theme`]'s
/// answer.
///
/// Apple publishes no "light" value: the key is **absent** in light mode and set
/// to `"Dark"` in dark mode, so absence is a real answer (`Some(false)`), not an
/// unknown. Any other value is treated as light as well — the key has only ever
/// carried `"Dark"`, and a future third value would more likely be a new dark
/// variant than a new light one, so this errs toward the theme the user can still
/// override explicitly.
///
/// Deliberately case-insensitive: the documented spelling is `"Dark"`, and
/// matching it exactly would turn a hypothetical `"dark"` into a silently
/// light-themed flyout.
///
/// Returns a plain `bool`, not `os_dark_theme`'s `Option<bool>`: on macOS there
/// is no "unknown" to represent, because absence of the key *is* the light
/// answer. The caller wraps.
#[cfg(any(test, target_os = "macos"))]
fn dark_from_interface_style(style: Option<&str>) -> bool {
    style.is_some_and(|s| s.eq_ignore_ascii_case("Dark"))
}

/// Decode Windows' `SPI_GETCLIENTAREAANIMATION` answer into
/// [`animations_enabled`].
///
/// `queried` is the OS-written `BOOL` from a **successful** query, or `None` when
/// the call itself failed (⇒ the motion-on default). Motion is on unless the OS
/// explicitly reported client-area animations disabled.
///
/// The `None` arm is not hypothetical bookkeeping: the original app-side version
/// of this AND-ed the call's success flag into the result and so returned
/// motion-**off** on a failed query — the opposite of the documented default, and
/// exactly what this separation makes testable.
#[cfg(any(test, windows))]
const fn animations_enabled_from(queried: Option<i32>) -> bool {
    match queried {
        Some(enabled) => enabled != 0,
        None => true,
    }
}

// --- Windows ---------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };
    use windows::core::{PCWSTR, w};

    use super::{AppsUseLightTheme, OpenUrlFailure};

    /// The subkey the Settings app writes the theme preference into.
    pub(super) const PERSONALIZE: PCWSTR =
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

    /// A `REG_DWORD` is four bytes; `RegGetValueW` is told so and writes the
    /// actual size back. A compile-time assert rather than a runtime conversion,
    /// so there is no fallible path that could silently answer "unknown".
    const DWORD_BYTES: u32 = 4;
    const _: () = assert!(DWORD_BYTES as usize == size_of::<u32>());

    /// Open a URL with the shell's `open` verb, i.e. the user's default browser.
    pub(super) fn open_url(url: &str) -> Result<(), OpenUrlFailure> {
        let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated wide string that outlives the call;
        // the "open" verb (`w!`) is a static NUL-terminated literal. Passing a
        // null HWND/dir/params is valid for opening a URL. The returned HINSTANCE
        // is a legacy success/error code we compare but never dereference.
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        let code = result.0 as usize;
        if super::shell_accepted(code) {
            Ok(())
        } else {
            Err(OpenUrlFailure {
                code: u32::try_from(code).ok(),
            })
        }
    }

    /// Read `HKCU\…\Themes\Personalize\AppsUseLightTheme`, the value the Settings
    /// app writes when the user picks an app theme.
    pub(super) fn os_dark_theme() -> Option<bool> {
        // SAFETY: both arguments are `w!` string literals, so each is a valid
        // NUL-terminated wide string with `'static` storage.
        let read = unsafe { read_dword(PERSONALIZE, w!("AppsUseLightTheme")) };
        super::dark_from_apps_use_light_theme(read)
    }

    /// Read one `REG_DWORD` under `HKCU`, distinguishing **absent** from
    /// **unreadable** — the whole point, since the two mean opposite things (see
    /// [`super::dark_from_apps_use_light_theme`]).
    ///
    /// `RRF_RT_REG_DWORD` restricts the type *before* any copy, so a `REG_SZ` under
    /// this name comes back `ERROR_UNSUPPORTED_TYPE` with the buffer untouched
    /// rather than as garbage, and `ERROR_MORE_DATA` is unreachable for a value
    /// that must be exactly four bytes.
    ///
    /// # Safety
    /// `subkey` and `name` must each be a valid, NUL-terminated wide string that
    /// stays live for the duration of the call.
    ///
    /// `unsafe` rather than safe-with-a-comment: the precondition is a property of
    /// the raw pointers, which the signature cannot enforce, so a safe signature
    /// would let a caller reach UB without writing `unsafe` anywhere.
    pub(super) unsafe fn read_dword(subkey: PCWSTR, name: PCWSTR) -> AppsUseLightTheme {
        let mut value: u32 = 0;
        let mut size = DWORD_BYTES;
        // SAFETY: `subkey`/`name` are valid NUL-terminated wide strings, live for
        // the call, by this function's own contract. `pdwtype` is null (we do not
        // need the type back); `pvdata`/`pcbdata` point at a live, aligned
        // `u32`/`u32` pair whose sizes match what RRF_RT_REG_DWORD requires.
        // `value` is read only after an `ERROR_SUCCESS` return.
        //
        // `pvdata` must be `Some`: passing `None` there is *documented* to return
        // `ERROR_SUCCESS` and report only the size, so the buffer would keep its
        // seed and every read would "succeed" with a fabricated value. Pinned by
        // `tests::live::a_written_dword_round_trips_through_the_production_read`,
        // which is the only shape that can observe it.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                subkey,
                name,
                RRF_RT_REG_DWORD,
                None,
                Some(std::ptr::addr_of_mut!(value).cast()),
                Some(&raw mut size),
            )
        };
        match rc {
            ERROR_SUCCESS => AppsUseLightTheme::Value(value),
            // Not written. On a stock profile that is the normal state, not an
            // error. `ERROR_FILE_NOT_FOUND` is the code Microsoft documents, and
            // measurement says it covers a missing *subkey* too, at any depth —
            // `ERROR_PATH_NOT_FOUND` is listed for breadth, not because any probed
            // shape produces it.
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => AppsUseLightTheme::Absent,
            _ => AppsUseLightTheme::Unreadable,
        }
    }

    /// Query `SPI_GETCLIENTAREAANIMATION` (the "Animation effects" toggle).
    pub(super) fn animations_enabled() -> bool {
        // A Win32 `BOOL` is a 4-byte int; seed motion-on so a failed call that
        // leaves the buffer untouched still decodes to the documented default.
        let mut enabled: i32 = 1;
        // SAFETY: `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)` writes a
        // `BOOL` (4-byte int) into `pvparam`; we pass a pointer to a live,
        // correctly-sized, aligned `i32` and read it only after the call returns.
        // `uiparam`/`fwinini` are 0, as documented for a read (no broadcast, no
        // profile write).
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some(std::ptr::addr_of_mut!(enabled).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        // Decide on the value alone: on success the OS overwrote `enabled`, on
        // failure it left our seed, and only a *successful* query is evidence.
        super::animations_enabled_from(ok.is_ok().then_some(enabled))
    }
}

// --- macOS -----------------------------------------------------------------

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL, NSUserDefaults};

    use super::OpenUrlFailure;

    /// Hand the URL to `NSWorkspace`, which routes it to the default handler for
    /// its scheme — the user's browser for `https`.
    ///
    /// `AppKit` reports only success or failure, so the failure carries no code.
    pub(super) fn open_url(url: &str) -> Result<(), OpenUrlFailure> {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
            // `URLWithString` returns nil for a string it cannot parse as a URL.
            return Err(OpenUrlFailure { code: None });
        };
        if NSWorkspace::sharedWorkspace().openURL(&url) {
            Ok(())
        } else {
            Err(OpenUrlFailure { code: None })
        }
    }

    /// Read the `AppleInterfaceStyle` user default.
    ///
    /// Deliberately **not** `NSApp.effectiveAppearance`: that reads the
    /// application's resolved appearance, which requires a live `NSApplication`,
    /// and this is called once during startup *before* Slint's winit backend has
    /// built one. Touching `sharedApplication` there would create the app object
    /// out from under winit. `NSUserDefaults` has no such requirement and answers
    /// the same system-level question.
    ///
    /// Always `Some`: macOS has no unknown state here (see
    /// [`dark_from_interface_style`](super::dark_from_interface_style)).
    // RATIONALE: the `Option` is the cross-platform `os_dark_theme` contract, which
    // the other two arms genuinely answer `None` to — Windows on an unreadable
    // registry value, and the placeholder arm always. Unwrapping it here would mean
    // two different return types for one public fn.
    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn os_dark_theme() -> Option<bool> {
        let style = NSUserDefaults::standardUserDefaults()
            .stringForKey(&NSString::from_str("AppleInterfaceStyle"))
            .map(|s| s.to_string());
        Some(super::dark_from_interface_style(style.as_deref()))
    }

    /// Ask `NSWorkspace` for the Accessibility "Reduce motion" setting. Reported
    /// in the negative by `AppKit`, so it is inverted into Duja's motion-on
    /// vocabulary here.
    pub(super) fn animations_enabled() -> bool {
        !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
}

// --- other targets ---------------------------------------------------------

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::OpenUrlFailure;

    /// No URL opener wired on this platform yet.
    ///
    /// **A placeholder, not a supported configuration.** Linux (P7) has the
    /// answer — `xdg-open`, or the portal's `OpenURI` — but wiring it means
    /// choosing between spawning a process and taking a D-Bus dependency, which
    /// is a P7 decision. Reporting a failure makes the caller log "could not open
    /// the releases page", which is true, rather than silently claiming success.
    pub(super) const fn open_url(_url: &str) -> Result<(), OpenUrlFailure> {
        Err(OpenUrlFailure { code: None })
    }

    /// No theme query wired on this platform yet (Linux, P7: the XDG
    /// `color-scheme` portal setting, or the desktop's own key). `None` leaves
    /// the choice to the caller's documented default.
    pub(super) const fn os_dark_theme() -> Option<bool> {
        None
    }

    /// No reduced-motion query wired on this platform yet (Linux, P7:
    /// `gtk-enable-animations` / the portal). Takes the motion-on default, which
    /// is what every desktop ships with.
    pub(super) const fn animations_enabled() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    /// `AppsUseLightTheme` asks about *light*, so `0` is dark. Inverting this is
    /// the obvious slip and it would flip every `System`-theme user's flyout.
    #[test]
    fn windows_apps_use_light_theme_is_inverted() {
        use super::AppsUseLightTheme::Value;
        use super::dark_from_apps_use_light_theme as decode;
        assert_eq!(decode(Value(0)), Some(true), "0 = dark");
        assert_eq!(decode(Value(1)), Some(false), "1 = light");
    }

    /// An **absent** value means light, because Windows only writes it once the
    /// user first changes the app-mode setting and Windows' own default is light.
    ///
    /// The case that matters most in practice: a stock profile has no value at
    /// all, and `ConfigTheme::System` is the serde default, so answering `None`
    /// here (which is what "absent or unreadable ⇒ unknown" did) left the whole
    /// default cohort on the caller's dark fallback with every other app light —
    /// i.e. the query would have shipped without fixing what it exists to fix.
    #[test]
    fn an_absent_windows_theme_value_means_light_not_unknown() {
        use super::AppsUseLightTheme::Absent;
        assert_eq!(super::dark_from_apps_use_light_theme(Absent), Some(false));
    }

    /// A *failed* read is genuinely unknown, and must stay distinguishable from
    /// absence — collapsing the two is exactly the bug above, in the other
    /// direction: it would assert "light" on a host that never answered.
    #[test]
    fn an_unreadable_windows_theme_value_is_unknown() {
        use super::AppsUseLightTheme::Unreadable;
        assert_eq!(super::dark_from_apps_use_light_theme(Unreadable), None);
    }

    /// `ShellExecuteW`'s legacy success rule, at the boundary: **32 itself is a
    /// failure**. An off-by-one here reports "opened" for `SE_ERR_*` code 32.
    #[test]
    fn the_shell_success_floor_is_exclusive() {
        use super::shell_accepted;
        assert!(!shell_accepted(0));
        assert!(!shell_accepted(31), "SE_ERR_NOASSOC");
        assert!(!shell_accepted(32), "32 is still a failure");
        assert!(shell_accepted(33), "the first success value");
    }

    /// macOS is the opposite shape: the key is absent in light mode, so absence
    /// is a real answer. Returning `None` here would strand every light-mode Mac
    /// on the caller's dark default.
    #[test]
    fn a_missing_apple_interface_style_means_light_not_unknown() {
        use super::dark_from_interface_style as decode;
        assert!(!decode(None));
        assert!(decode(Some("Dark")));
    }

    /// Matched case-insensitively on purpose; an exact-match decoder would render
    /// a lowercase spelling as light.
    #[test]
    fn apple_interface_style_matches_dark_regardless_of_case() {
        use super::dark_from_interface_style as decode;
        assert!(decode(Some("dark")));
        assert!(decode(Some("DARK")));
        // Anything that is not "Dark" is light — see the decoder's docs.
        assert!(!decode(Some("Light")));
        assert!(!decode(Some("")));
    }

    /// A failed query must fall back to motion-ON. The historical bug AND-ed the
    /// call's success flag in and returned motion-OFF here, disabling the glide
    /// on any host where the query failed.
    #[test]
    fn motion_defaults_on_when_the_os_query_fails() {
        use super::animations_enabled_from as decode;
        assert!(decode(None));
        assert!(decode(Some(1)));
        // An explicit accessibility opt-out is honoured.
        assert!(!decode(Some(0)));
    }

    /// Live tests against the real OS — the decoders above pin the *decisions*,
    /// these pin that the platform call under them actually works.
    ///
    /// The crate has precedent for this shape (`autostart/win.rs`'s scratch-key
    /// round trip, `geometry.rs`'s real-backend anchor), and it earns its keep
    /// here because the registry read is a hand-built flags/buffer/size triple.
    ///
    /// # Why an error-path test is not enough
    ///
    /// The tempting shortcut — "read a value that cannot exist and check we say
    /// `Absent`" — proves less than it looks, and the reasoning that makes it look
    /// sufficient is **false**. Getting the data pointer wrong does *not* make the
    /// read fail: [`RegGetValueW`] is documented to return `ERROR_SUCCESS` and
    /// merely report the required size when `pvData` is `NULL`. So a read with a
    /// null buffer *succeeds*, leaves the buffer at whatever it was seeded with,
    /// and hands back a fabricated value — every user getting one fixed theme
    /// forever, with no error anywhere for a decoder or an error-path test to see.
    ///
    /// Only a **round trip** can observe that the OS wrote our bytes, so that is
    /// what [`a_written_dword_round_trips_through_the_production_read`] does,
    /// under a throwaway per-process key and never against the real theme value.
    ///
    /// # What these still cannot pin
    ///
    /// The call *shape* is covered; the call's **identity** is not. Swapping the
    /// subkey and value arguments, truncating the subkey, or reading
    /// `SystemUsesLightTheme` (Windows' separate taskbar/Start setting) instead of
    /// `AppsUseLightTheme` each leave every test green while making `os_dark_theme`
    /// wrong on every machine — the first two answer `Absent` ⇒ light everywhere,
    /// the third silently follows a setting the user can set independently.
    ///
    /// That is structural rather than an omission: `Absent` deliberately merges
    /// "value missing" and "key missing" — the right product decision, since a stock
    /// profile has neither — so nothing reading through `read_dword` can distinguish
    /// a correct path from a wrong one. The only real defence would be distinct
    /// newtypes for the two `PCWSTR` arguments, which is more machinery than the
    /// risk earns for two `w!` literals at a single call site. Recorded so the next
    /// reader knows which half is guarded.
    ///
    /// [`RegGetValueW`]: https://learn.microsoft.com/en-us/windows/win32/api/winreg/nf-winreg-reggetvaluew
    /// [`a_written_dword_round_trips_through_the_production_read`]: live::a_written_dword_round_trips_through_the_production_read
    #[cfg(windows)]
    mod live {
        use windows::Win32::Foundation::ERROR_SUCCESS;
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
            REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteKeyW,
            RegSetValueExW,
        };
        use windows::core::{PCWSTR, w};

        use super::super::platform::read_dword;
        use super::super::{AppsUseLightTheme, os_dark_theme};

        /// A value neither `0` nor `1`, so it cannot be confused with a real theme
        /// setting and so a stale/zeroed buffer cannot pass by coincidence.
        const SENTINEL: u32 = 0x00BA_DA55;

        /// NUL-terminate a Rust string for the Win32 wide-string APIs.
        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        /// The OS's bytes actually land in our buffer.
        ///
        /// Writes `SENTINEL` under a per-process scratch key, reads it back through
        /// the **production** `read_dword`, and asserts the value round-trips. This
        /// is the only construction that can catch a mis-wired `pvData` — see the
        /// module docs for why a null buffer *succeeds* rather than failing.
        #[test]
        fn a_written_dword_round_trips_through_the_production_read() {
            let scratch = ScratchKey::new("desktop");

            // Absent before anything is written — the same error path the other
            // test covers, asserted here against a key we know the state of.
            // SAFETY: the scratch path outlives the call (owned by `scratch`); the
            // value name is a `w!` literal.
            let before = unsafe { read_dword(scratch.path(), w!("Probe")) };
            assert_eq!(before, AppsUseLightTheme::Absent, "nothing written yet");

            scratch.write_dword(SENTINEL);

            // SAFETY: as above.
            let after = unsafe { read_dword(scratch.path(), w!("Probe")) };
            assert_eq!(
                after,
                AppsUseLightTheme::Value(SENTINEL),
                "the value we wrote must come back byte-for-byte; a `Value` with \
                 any other payload means the OS never wrote our buffer"
            );
        }

        /// A value of the wrong **type** must be refused, not decoded.
        ///
        /// `RRF_RT_REG_DWORD` is what makes that true, and it is the last property
        /// of the call shape the round trip above cannot see: relax it to
        /// `RRF_RT_ANY` and a `REG_SZ` is copied into the `u32` buffer verbatim, so
        /// a one-letter string reads back as `Value(0x41)` and decodes to a
        /// perfectly confident *light*. The probe is written four bytes wide on
        /// purpose — it would **fit** the buffer, so only the type restriction can
        /// reject it.
        #[test]
        fn a_value_of_the_wrong_type_is_unreadable_not_decoded() {
            let scratch = ScratchKey::new("wrongtype");
            scratch.write_string_probe();

            // SAFETY: the scratch path outlives the call (owned by `scratch`); the
            // value name is a `w!` literal.
            let read = unsafe { read_dword(scratch.path(), w!("Probe")) };
            assert_eq!(
                read,
                AppsUseLightTheme::Unreadable,
                "a REG_SZ under a DWORD name must be refused on type; decoding its \
                 bytes would fabricate a theme out of text"
            );
        }

        /// A throwaway `HKCU\Software\DujaTest\<tag>-<pid>` key that deletes
        /// itself on drop.
        ///
        /// A guard rather than a cleanup call at the end of the test, because this
        /// test's whole job is to **fail** when the read is mis-wired: a trailing
        /// `delete_scratch_key(...)` is skipped by the unwind, so every red run
        /// would leave a key behind in the developer's registry. (Observed exactly
        /// that while verifying the sabotage.) `Drop` runs on the unwind too.
        struct ScratchKey {
            /// The subkey path, NUL-terminated. Owned here so the pointer handed to
            /// Win32 stays valid for this guard's whole life.
            wide: Vec<u16>,
        }

        impl ScratchKey {
            /// Create the key. Process-unique, so parallel runs never collide, and
            /// never anywhere near the real Personalize key.
            fn new(tag: &str) -> Self {
                let key = ScratchKey {
                    wide: wide(&format!(r"Software\DujaTest\{tag}-{}", std::process::id())),
                };
                let mut handle = HKEY::default();
                // SAFETY: `key.wide` is a NUL-terminated wide string owned by `key`
                // and live for the call; `phkresult` points at a live `HKEY`,
                // written only on success. Every other out-param is `None`.
                let rc = unsafe {
                    RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        key.path(),
                        None,
                        PCWSTR::null(),
                        REG_OPTION_NON_VOLATILE,
                        REG_SAM_FLAGS(KEY_READ.0 | KEY_WRITE.0),
                        None,
                        &raw mut handle,
                        None,
                    )
                };
                assert_eq!(rc, ERROR_SUCCESS, "could not create the scratch key");
                // SAFETY: `handle` came from the successful create above and is
                // closed exactly once; the key itself persists until `Drop`.
                unsafe {
                    let _ = RegCloseKey(handle);
                }
                key
            }

            /// This key's path, for the production reader.
            fn path(&self) -> PCWSTR {
                PCWSTR(self.wide.as_ptr())
            }

            /// Set `Probe` to `value` as a `REG_DWORD`.
            fn write_dword(&self, value: u32) {
                self.write_probe(REG_DWORD, &value.to_ne_bytes());
            }

            /// Set `Probe` to a one-character `REG_SZ`, for the type-restriction
            /// test. Deliberately four bytes wide (the UTF-16 encoding of a single letter plus its terminator), so it would
            /// *fit* a DWORD buffer — the restriction flag has to reject it on type,
            /// not on size.
            fn write_string_probe(&self) {
                self.write_probe(REG_SZ, &[0x41, 0x00, 0x00, 0x00]);
            }

            /// Write `Probe` with an explicit type and payload.
            fn write_probe(&self, kind: REG_VALUE_TYPE, bytes: &[u8]) {
                let mut handle = HKEY::default();
                // SAFETY: as in `new` — the path outlives the call and `phkresult`
                // points at a live `HKEY` written only on success.
                let rc = unsafe {
                    RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        self.path(),
                        None,
                        PCWSTR::null(),
                        REG_OPTION_NON_VOLATILE,
                        REG_SAM_FLAGS(KEY_READ.0 | KEY_WRITE.0),
                        None,
                        &raw mut handle,
                        None,
                    )
                };
                assert_eq!(rc, ERROR_SUCCESS, "could not open the scratch key");
                // SAFETY: `handle` is an open key from the successful create above;
                // the name is a `w!` literal; `bytes` is a live buffer matching the
                // declared type and length.
                let rc = unsafe { RegSetValueExW(handle, w!("Probe"), None, kind, Some(bytes)) };
                // SAFETY: `handle` came from `RegCreateKeyExW`, closed exactly once.
                unsafe {
                    let _ = RegCloseKey(handle);
                }
                assert_eq!(rc, ERROR_SUCCESS, "could not write the scratch value");
            }
        }

        impl Drop for ScratchKey {
            fn drop(&mut self) {
                // SAFETY: the path is a NUL-terminated wide string owned by `self`
                // and live for the call; this deletes only the process-unique key
                // this guard created.
                unsafe {
                    let _ = RegDeleteKeyW(HKEY_CURRENT_USER, self.path());
                }
                // `RegCreateKeyExW` also created the shared `Software\DujaTest`
                // parent, so removing only our own key still leaves an empty key in
                // the user's registry. Sweep it best-effort: `RegDeleteKeyW` refuses
                // a key that still has subkeys, so this can never delete a key a
                // sibling test is using.
                //
                // It does NOT guarantee the parent is gone — `autostart/win.rs`
                // creates leaves under the same parent and now has the same guard,
                // but which process drops last is a race, and any future scratch key
                // added WITHOUT a guard reintroduces the residue. Best-effort is the
                // honest description; measured to clear it in the common case.
                let parent = wide(r"Software\DujaTest");
                // SAFETY: `parent` is a NUL-terminated wide string live for the
                // call; the delete is refused rather than recursive if non-empty.
                unsafe {
                    let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(parent.as_ptr()));
                }
            }
        }

        /// A value that cannot exist must read as **`Absent`**, not `Unreadable`.
        ///
        /// The return-code half of the contract, against the *real* Personalize
        /// subkey: the key is real, the value name is not. Distinguishing these two
        /// codes is what makes a stock profile resolve to light rather than dark.
        #[test]
        fn a_value_that_does_not_exist_reads_as_absent() {
            use super::super::platform::PERSONALIZE;
            // SAFETY: both arguments are `w!`/`const` string literals with 'static
            // storage.
            let read = unsafe { read_dword(PERSONALIZE, w!("DujaNoSuchValue")) };
            assert_eq!(
                read,
                AppsUseLightTheme::Absent,
                "a missing value must be Absent (the light default), not Unreadable"
            );
        }

        /// The real query answers *something* rather than erroring out. It cannot
        /// assert a particular theme — CI runners and dev boxes differ — but a
        /// `None` here would mean the read failed outright on a live Windows host,
        /// which is never expected: the value is either present or absent, and
        /// both are `Some`.
        #[test]
        fn the_live_theme_query_is_never_unreadable_on_a_real_host() {
            assert!(
                os_dark_theme().is_some(),
                "reading the theme on a live Windows host must not fail"
            );
        }
    }
}

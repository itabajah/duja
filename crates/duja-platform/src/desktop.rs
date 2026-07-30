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
//! Every platform arm is a thin call plus a *pure* decoder that turns the OS's raw
//! answer into Duja's vocabulary — including `open_url`'s, whose success rule is
//! Windows' legacy "greater than 32" convention rather than a boolean. The decoders
//! are compiled (and tested) on every OS, so the interpretation of
//! `AppsUseLightTheme == 0`, of an **absent** `AppsUseLightTheme`, and of a missing
//! `AppleInterfaceStyle` are pinned on all three CI lanes, not only on the lane that
//! can run the query.

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
    const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

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
        super::dark_from_apps_use_light_theme(read_personalize_dword(w!("AppsUseLightTheme")))
    }

    /// Read one `REG_DWORD` from the Personalize subkey, distinguishing **absent**
    /// from **unreadable** — the whole point, since the two mean opposite things
    /// (see [`super::dark_from_apps_use_light_theme`]).
    ///
    /// `RRF_RT_REG_DWORD` restricts the type *before* any copy, so a `REG_SZ` under
    /// this name comes back `ERROR_UNSUPPORTED_TYPE` with the buffer untouched
    /// rather than as garbage, and `ERROR_MORE_DATA` is unreachable for a value
    /// that must be exactly four bytes.
    pub(super) fn read_personalize_dword(name: PCWSTR) -> AppsUseLightTheme {
        let mut value: u32 = 0;
        let mut size = DWORD_BYTES;
        // SAFETY: the subkey path is a static NUL-terminated literal and `name` is
        // one by the caller's contract. `pdwtype` is null (we do not need the type
        // back); `pvdata`/`pcbdata` point at a live, aligned `u32`/`u32` pair whose
        // sizes match what RRF_RT_REG_DWORD requires. `value` is read only after an
        // `ERROR_SUCCESS` return.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                name,
                RRF_RT_REG_DWORD,
                None,
                Some(std::ptr::addr_of_mut!(value).cast()),
                Some(&raw mut size),
            )
        };
        match rc {
            ERROR_SUCCESS => AppsUseLightTheme::Value(value),
            // The value, or the whole subkey, has never been written. On a stock
            // profile that is the normal state, not an error.
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
    /// these pin that the platform calls under them actually work.
    ///
    /// The crate has precedent for this (`autostart/win.rs`'s scratch-key round
    /// trip, `geometry.rs`'s real-backend anchor), and it earns its keep here: the
    /// registry read is a hand-built flags/buffer/size triple, and getting the
    /// restriction flag or a pointer wrong would make every read fail — which the
    /// pure decoder would faithfully translate into "unknown ⇒ dark" with nothing
    /// to see.
    #[cfg(windows)]
    mod live {
        use super::super::{AppsUseLightTheme, os_dark_theme};

        /// A value that cannot exist must read as **`Absent`**, not `Unreadable`.
        ///
        /// This exercises the whole `RegGetValueW` call — flags, buffer pointer,
        /// size in/out — plus the return-code mapping, and needs no writes: the
        /// subkey is real, the value name is not. If the triple were malformed,
        /// the call would fail with some *other* code and land in `Unreadable`,
        /// which is the failure this asserts against.
        #[test]
        fn a_value_that_does_not_exist_reads_as_absent() {
            use windows::core::w;
            let read = super::super::platform::read_personalize_dword(w!("DujaNoSuchValue"));
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

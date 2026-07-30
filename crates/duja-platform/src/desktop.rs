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
//! through winit/Slint, which expose none of the three.
//!
//! # Failure is never fatal
//!
//! Every entry point degrades to a documented default rather than an error: an
//! unopenable URL is reported as `false` for the caller to log, an unknown theme
//! is `None` (the caller decides), and an unanswerable motion query is
//! motion-**on**, matching both platforms' own defaults. None of these is worth
//! failing a launch over.
//!
//! # Purity
//!
//! Each platform arm is a thin call plus a *pure* decoder that turns the OS's raw
//! answer into Duja's vocabulary. The decoders are compiled (and tested) on every
//! OS, so the interpretation of `AppsUseLightTheme == 0` or of a missing
//! `AppleInterfaceStyle` is pinned on all three CI lanes, not only on the lane
//! that can run the query.

/// Open `url` in the user's default browser.
///
/// Best-effort and **non-blocking on the outcome**: returns `false` if the
/// platform reported that it could not open the URL, which callers log rather
/// than treat as fatal. A `true` means the OS accepted the request, not that a
/// browser window is on screen.
#[must_use]
pub fn open_url(url: &str) -> bool {
    platform::open_url(url)
}

/// Whether the OS is currently in dark mode.
///
/// `Some(true)` dark, `Some(false)` light, `None` when the platform has no answer
/// (or the query failed) — the caller decides what unknown means, because the
/// right default is a UI question, not a platform one.
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

/// Decode Windows' `AppsUseLightTheme` registry value into [`os_dark_theme`]'s
/// answer.
///
/// The value is a `REG_DWORD` that is `0` for dark and `1` for light — note the
/// **inversion**: the name asks about *light*, so `0` means dark. `None` in means
/// the value was absent or unreadable, which is `None` out: on a Windows build
/// old enough to lack the key there is no preference to honour, and guessing
/// would silently override the user's `System` choice with a coin flip.
#[cfg(any(test, windows))]
const fn dark_from_apps_use_light_theme(value: Option<u32>) -> Option<bool> {
    match value {
        Some(v) => Some(v == 0),
        None => None,
    }
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
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };
    use windows::core::{PCWSTR, w};

    /// `ShellExecuteW` returns a value **greater than 32** on success — a legacy
    /// convention where the return is an `HINSTANCE`-shaped error code otherwise.
    const SHELL_EXECUTE_SUCCESS_FLOOR: usize = 32;

    /// Open a URL with the shell's `open` verb, i.e. the user's default browser.
    pub(super) fn open_url(url: &str) -> bool {
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
        result.0 as usize > SHELL_EXECUTE_SUCCESS_FLOOR
    }

    /// Read `HKCU\…\Themes\Personalize\AppsUseLightTheme`, the value the Settings
    /// app writes when the user picks an app theme.
    pub(super) fn os_dark_theme() -> Option<bool> {
        super::dark_from_apps_use_light_theme(apps_use_light_theme())
    }

    /// The raw `AppsUseLightTheme` DWORD, or `None` if it is absent/unreadable.
    fn apps_use_light_theme() -> Option<u32> {
        let mut value: u32 = 0;
        let mut size = u32::try_from(size_of::<u32>()).ok()?;
        // SAFETY: both wide strings are static NUL-terminated literals. `pdwtype`
        // is null (we do not need the type back) and `pvdata`/`pcbdata` point at a
        // live, aligned `u32`/`u32` pair whose sizes match what RRF_RT_REG_DWORD
        // requires; the value is read only after a success return.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(std::ptr::addr_of_mut!(value).cast()),
                Some(&raw mut size),
            )
        };
        (rc == ERROR_SUCCESS).then_some(value)
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

    /// Hand the URL to `NSWorkspace`, which routes it to the default handler for
    /// its scheme — the user's browser for `https`.
    pub(super) fn open_url(url: &str) -> bool {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
            // `URLWithString` returns nil for a string it cannot parse as a URL.
            return false;
        };
        NSWorkspace::sharedWorkspace().openURL(&url)
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
    // RATIONALE: the `Option` is the cross-platform `os_dark_theme` contract,
    // which Windows genuinely can answer `None` to (an absent registry value).
    // Unwrapping it here would mean two different return types for one public fn.
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
    /// No URL opener wired on this platform yet.
    ///
    /// **A placeholder, not a supported configuration.** Linux (P7) has the
    /// answer — `xdg-open`, or the portal's `OpenURI` — but wiring it means
    /// choosing between spawning a process and taking a D-Bus dependency, which
    /// is a P7 decision. Reporting `false` makes the caller log "could not open
    /// the releases page", which is true, rather than silently claiming success.
    pub(super) const fn open_url(_url: &str) -> bool {
        false
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
        use super::dark_from_apps_use_light_theme as decode;
        assert_eq!(decode(Some(0)), Some(true), "0 = dark");
        assert_eq!(decode(Some(1)), Some(false), "1 = light");
    }

    /// A missing value is unknown, not a guess. Answering `Some(false)` here
    /// would silently force light on any host without the key.
    #[test]
    fn a_missing_windows_theme_value_is_unknown() {
        assert_eq!(super::dark_from_apps_use_light_theme(None), None);
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
}

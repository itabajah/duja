//! Reduced-motion policy for the flyout's premium slider glide.
//!
//! The only animation Duja drives from Rust is the slider thumb gliding to a new
//! position when brightness changes **externally** (the monitor's own buttons —
//! see the reflection path). It honours the Windows "Show animations in Windows"
//! accessibility setting, and never animates a hidden window. The DDC-never-
//! animates rule is unaffected: only the rendered thumb glides; the engine
//! already has the final value.

// RATIONALE: consumed only by the Windows tray assembly; the pure policy stays
// cross-platform so its tests run on every CI OS.
#![cfg_attr(not(windows), allow(dead_code))]

/// The thumb's glide duration (ms) when motion is enabled and the window is
/// visible. Short enough to feel responsive, long enough to read as a glide.
pub(crate) const GLIDE_MS: i32 = 160;

/// The glide duration (ms) to push into the flyout for the current state.
///
/// Zero (instant, no animation) whenever the window is hidden **or** the OS has
/// animations disabled — so a hidden window can never animate and an
/// accessibility opt-out is honoured. A user drag never animates regardless
/// (the `.slint` slider forces the drag duration to 0); this only governs the
/// external-change glide.
pub(crate) fn glide_for(visible: bool, os_animations: bool) -> i32 {
    if visible && os_animations {
        GLIDE_MS
    } else {
        0
    }
}

/// Whether the OS wants client-area animations (Settings → Accessibility →
/// Visual effects → "Animation effects"). Defaults to `true` (motion) if the
/// query fails, matching Windows' own default.
#[cfg(windows)]
pub(crate) fn os_animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };

    // A Win32 `BOOL` is a 4-byte int; default to animations-on if the call fails.
    let mut enabled: i32 = 1;
    // SAFETY: `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)` writes a `BOOL`
    // (4-byte int) into `pvparam`; we pass a pointer to a live, correctly-sized,
    // aligned `i32` and read it only after the call returns. `uiparam`/`fwinini`
    // are 0, as documented for a read (no broadcast, no profile write).
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(std::ptr::addr_of_mut!(enabled).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_ok() && enabled != 0
}

/// Non-Windows: the flyout is Windows-only today; assume motion is fine.
#[cfg(not(windows))]
pub(crate) fn os_animations_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{GLIDE_MS, glide_for};

    #[test]
    fn glide_is_on_only_when_visible_and_motion_allowed() {
        assert_eq!(glide_for(true, true), GLIDE_MS);
        // Hidden: never animate (a hidden window must not schedule frames).
        assert_eq!(glide_for(false, true), 0);
        // Reduced motion: honour the accessibility opt-out.
        assert_eq!(glide_for(true, false), 0);
        assert_eq!(glide_for(false, false), 0);
    }
}

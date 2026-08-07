//! Reduced-motion policy for the flyout's premium slider glide.
//!
//! The only animation Duja drives from Rust is the slider thumb gliding to a new
//! position when brightness changes **externally** (the monitor's own buttons —
//! see the reflection path). It honours the OS accessibility setting on both
//! platforms ("Animation effects" on Windows, "Reduce motion" on macOS), and
//! never animates a hidden window. The DDC-never-animates rule is unaffected:
//! only the rendered thumb glides; the engine already has the final value.
//!
//! The *query* lives in `duja_platform::desktop`, with the rest of the confined
//! platform FFI. What stays here is the **policy** — how a glide duration follows
//! from the answer plus whether the window is even on screen — which is pure and
//! is tested on every CI OS.

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

/// Whether the OS wants UI animations, from `duja_platform::desktop`.
///
/// Defaults to `true` (motion) when the platform cannot answer, matching both
/// Windows' and macOS' own defaults. The decision that turns a failed query into
/// that default is pinned in `duja-platform`, next to the query it guards.
pub(crate) fn os_animations_enabled() -> bool {
    duja_platform::animations_enabled()
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

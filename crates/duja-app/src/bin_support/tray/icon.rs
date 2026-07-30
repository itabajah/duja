//! The notification-area tray icon.
//!
//! The art is **not** drawn here: it comes from [`duja_ui::icon`], the same display
//! silhouette the taskbar/alt-tab window icon uses, so the two icons are one glyph
//! in one colour rather than the white sun and ruby monitor they used to be.
//!
//! The raw RGBA buffer is all that can cross the crate boundary — `tray-icon` is a
//! dependency of this crate only, and Slint's winit backend of `duja-ui` only, so
//! neither can name the other's `Icon` type. Wrapping the buffer into a
//! [`tray_icon::Icon`] is the only step that lives here, and the only fallible one.

/// The tray icon side length in pixels. The shared art is designed on a 64px canvas
/// and scales down to this cleanly (guarded by `duja-ui`'s
/// `glyph_survives_the_32px_tray_scale`).
///
/// **32 on Windows**, whose notification area asks for a 32×32 buffer at 200 % and
/// scales it down otherwise.
#[cfg(not(target_os = "macos"))]
const SIZE: u32 = 32;

/// The status-item icon side length in pixels on macOS: **36**, not 32.
///
/// The menu bar gives a status item an 18×18 **point** slot, and every Mac that
/// ships today is Retina, so the buffer that lands 1:1 is 18 × 2 = 36 physical
/// pixels. Handing `tray-icon` a 32px buffer would make `AppKit` scale 32 → 36 —
/// a non-integer 1.125× upsample of a glyph whose whole design constraint is
/// legibility at tiny sizes, which is exactly where resampling shows.
///
/// Deliberately a constant rather than a `backingScaleFactor` query: the menu bar
/// lives on one screen at a time and the status item follows it, so there is no
/// per-display answer to give, and a 1× Mac simply gets a cleanly halved 36 → 18.
#[cfg(target_os = "macos")]
const SIZE: u32 = 36;

/// Build the tray icon in `rgb` (the accent's [`duja_ui::accent::icon_rgb`]).
///
/// # Errors
/// [`tray_icon::BadIcon`] if the RGBA buffer does not match the declared size (it
/// always does — `monitor_rgba` is `size × size × 4` by construction, and asserted
/// as such by its own tests; this is defensive).
pub(super) fn tray_icon(rgb: [u8; 3]) -> anyhow::Result<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(duja_ui::icon::monitor_rgba(SIZE, rgb), SIZE, SIZE)
        .map_err(|e| anyhow::anyhow!("failed to build the tray icon: {e}"))
}

#[cfg(test)]
mod tests {
    use super::tray_icon;
    use duja_ui::accent::{ACCENT_ORDER, icon_rgb};

    #[test]
    fn tray_icon_builds_for_every_accent() {
        // Covers the exact path `apply_accent` takes when the user switches accent.
        for accent in ACCENT_ORDER {
            assert!(tray_icon(icon_rgb(accent)).is_ok(), "{accent:?}");
        }
    }
}

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
const SIZE: u32 = 32;

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

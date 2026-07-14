//! The application icon — a **monochrome display silhouette**, drawn in code.
//!
//! This is the single source of the icon *art*, shared by both places Duja shows
//! an icon:
//!
//! * the **taskbar / alt-tab window icon** (here, wrapped into a `winit::Icon` by
//!   `app_icon` and set by each window shell), and
//! * the **notification-area tray icon** (over in `duja-app`, which wraps the same
//!   buffer into a `tray_icon::Icon`).
//!
//! They used to be two unrelated hand-written generators — a ruby monitor here and
//! a white sun in the tray. Only the raw buffer can cross the crate boundary:
//! `tray-icon` is a `duja-app` dependency and the winit backend a `duja-ui` one, so
//! neither crate can name the other's icon type. Hence [`monitor_rgba`] is `pub`
//! and returns plain bytes.
//!
//! No asset is shipped, so the binary stays self-contained. The colour comes from
//! [`crate::accent::icon_rgb`], so both icons follow the user's chosen accent.

use i_slint_backend_winit::winit::window::Icon;

/// The window-icon side length (px). 64 keeps it crisp when Windows downsamples
/// it to the taskbar / alt-tab sizes. (The tray asks for 32.)
const WINDOW_SIZE: u32 = 64;

/// The canvas the silhouette geometry is designed on; every other size is a linear
/// scale of it.
const DESIGN_SIZE: f32 = 64.0;

/// Build the window icon in `rgb`, or `None` if winit rejects the buffer.
#[must_use]
pub(crate) fn app_icon(rgb: [u8; 3]) -> Option<Icon> {
    Icon::from_rgba(monitor_rgba(WINDOW_SIZE, rgb), WINDOW_SIZE, WINDOW_SIZE).ok()
}

/// The display silhouette as an RGBA buffer: `size × size × 4` bytes, row-major, on
/// a transparent field, anti-aliased by 4×4 supersampling.
///
/// Both icons render from this one function — the tray at 32 px, the window at 64.
/// Pixels are appended in order (never indexed into), so there is no index
/// arithmetic to reason about.
#[must_use]
pub fn monitor_rgba(size: u32, rgb: [u8; 3]) -> Vec<u8> {
    let scale = as_f32(size) / DESIGN_SIZE;
    let stride = size as usize;
    let mut buf = Vec::with_capacity(stride.saturating_mul(stride).saturating_mul(4));
    let [r, g, b] = rgb;
    for y in 0..size {
        for x in 0..size {
            // 4×4 supersample for crisp, smooth (anti-aliased) edges.
            let mut coverage = 0.0_f32;
            for sy in 0..4u32 {
                for sx in 0..4u32 {
                    let px = (as_f32(x) + (as_f32(sx) + 0.5) / 4.0 - 0.5) / scale;
                    let py = (as_f32(y) + (as_f32(sy) + 0.5) / 4.0 - 0.5) / scale;
                    if in_monitor(px, py) {
                        coverage += 1.0 / 16.0;
                    }
                }
            }
            let pixel: [u8; 4] = if coverage > 0.0 {
                [r, g, b, to_byte(coverage)]
            } else {
                [0, 0, 0, 0]
            };
            buf.extend_from_slice(&pixel);
        }
    }
    buf
}

/// Whether a point (in the 64px design space) is inside the monitor silhouette: the
/// rounded screen, the stand neck, or the stand base.
///
/// The neck is deliberately chunky (10 design px wide): the tray renders this art
/// at 32 px, where a hairline stand would dissolve into the anti-aliasing.
fn in_monitor(px: f32, py: f32) -> bool {
    // Screen (rounded rectangle).
    rounded_rect(px, py, 9.0, 12.0, 55.0, 44.0, 5.0)
        // Stand neck.
        || ((27.0..=37.0).contains(&px) && (43.0..=49.0).contains(&py))
        // Stand base (rounded).
        || rounded_rect(px, py, 18.0, 49.0, 46.0, 53.0, 2.0)
}

/// Whether `(px, py)` is inside the filled rounded rectangle `[x0,x1] × [y0,y1]`
/// with corner radius `r` (distance from the point to the inset core ≤ `r`).
fn rounded_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    let cx = px.clamp(x0 + r, x1 - r);
    let cy = py.clamp(y0 + r, y1 - r);
    (px - cx).hypot(py - cy) <= r
}

/// Convert a small non-negative integer (pixel coordinate ≤ 64) to `f32` losslessly.
fn as_f32(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

/// Scale a `0.0..=1.0` intensity to a `0..=255` byte.
fn to_byte(v: f32) -> u8 {
    // RATIONALE (cast_possible_truncation, cast_sign_loss): the clamp pins the
    // value to 0.0..=255.0 and `round` makes it integral, so the cast neither
    // truncates a meaningful fraction, loses a sign, nor overflows a `u8`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    byte
}

#[cfg(test)]
mod tests {
    use super::{WINDOW_SIZE, app_icon, monitor_rgba};
    use crate::accent::{ACCENT_ORDER, icon_rgb};

    /// The size the tray asks for.
    const TRAY_SIZE: u32 = 32;
    /// An arbitrary colour no constant in this crate uses.
    const SENTINEL: [u8; 3] = [1, 2, 3];

    /// The RGBA quad at `(x, y)` in a `size`-wide buffer.
    fn pixel(buf: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = (y as usize)
            .saturating_mul(size as usize)
            .saturating_add(x as usize)
            .saturating_mul(4);
        buf.get(offset..offset.saturating_add(4))
            .expect("pixel in range")
            .try_into()
            .expect("four bytes")
    }

    #[test]
    fn buffer_has_the_declared_size() {
        // winit and tray-icon both reject a buffer whose length disagrees with its
        // dimensions, so this guards both icons at once.
        assert_eq!(monitor_rgba(WINDOW_SIZE, SENTINEL).len(), 64 * 64 * 4);
        assert_eq!(monitor_rgba(TRAY_SIZE, SENTINEL).len(), 32 * 32 * 4);
    }

    #[test]
    fn centre_pixel_carries_the_requested_colour() {
        // Proves the colour is threaded through rather than baked in: the centre
        // sits inside the screen rectangle, so it is fully opaque and fully tinted.
        let buf = monitor_rgba(WINDOW_SIZE, SENTINEL);
        let [r, g, b, a] = pixel(&buf, WINDOW_SIZE, 32, 32);
        assert_eq!([r, g, b], SENTINEL);
        assert_eq!(a, 255);
    }

    #[test]
    fn corner_pixel_is_transparent() {
        let buf = monitor_rgba(WINDOW_SIZE, SENTINEL);
        assert_eq!(pixel(&buf, WINDOW_SIZE, 0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn glyph_survives_the_32px_tray_scale() {
        // The art is designed on a 64px canvas and the tray renders it at half that,
        // so the stand is the piece most at risk of dissolving. Assert the screen,
        // the neck and the base each still put down ink at 32px.
        let buf = monitor_rgba(TRAY_SIZE, SENTINEL);
        let opaque = |x, y| {
            let [_, _, _, a] = pixel(&buf, TRAY_SIZE, x, y);
            a > 128
        };

        assert!(opaque(16, 14), "screen centre (design 32,28) lost at 32px");
        assert!(opaque(16, 23), "stand neck (design 32,46) lost at 32px");
        assert!(opaque(16, 25), "stand base (design 32,51) lost at 32px");

        let inked = buf
            .chunks_exact(4)
            .filter(|p| p.last().is_some_and(|&a| a > 0))
            .count();
        assert!(
            inked > 250,
            "only {inked} inked pixels at 32px — the glyph collapsed"
        );
    }

    #[test]
    fn every_accent_icon_builds() {
        for accent in ACCENT_ORDER {
            let rgb = icon_rgb(accent);
            assert!(app_icon(rgb).is_some(), "{accent:?} window icon");
            assert_eq!(
                monitor_rgba(TRAY_SIZE, rgb).len(),
                32 * 32 * 4,
                "{accent:?} tray icon"
            );
        }
    }
}

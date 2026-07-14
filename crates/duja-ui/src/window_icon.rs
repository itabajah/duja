//! A code-generated, **monochrome** application window icon — a display/monitor
//! silhouette in a single colour, clean and symbolic like the in-app gear.
//!
//! Drawn in Rust with no shipped asset (the same self-contained rationale as the
//! tray icon in `duja-app`). Set on each window via winit
//! ([`slint::Window::with_winit_window`]) so the taskbar button (and alt-tab)
//! show a real icon instead of a blank placeholder when a window opens.

use std::sync::OnceLock;

use i_slint_backend_winit::winit::window::Icon;

/// The icon side length (px). 64 keeps it crisp when Windows downsamples it to
/// the taskbar / alt-tab sizes.
const SIZE: u32 = 64;

/// The single icon colour — the app's ruby accent. Mono, no gradient.
const ICON_RGB: [u8; 3] = [0xf2, 0x55, 0x5a];

/// Build the window icon, or `None` if winit rejects the buffer.
///
/// The RGBA buffer is generated once and cached; `winit::Icon::from_rgba` only
/// re-validates the (always correct) length on each call.
#[must_use]
pub(crate) fn app_icon() -> Option<Icon> {
    static RGBA: OnceLock<Vec<u8>> = OnceLock::new();
    let rgba = RGBA.get_or_init(|| monitor_rgba(SIZE));
    Icon::from_rgba(rgba.clone(), SIZE, SIZE).ok()
}

/// A mono monitor silhouette (rounded screen + stand), anti-aliased by 4×4
/// supersampling, on a transparent field. Pixels are appended in row-major order
/// (like the tray icon) so there is no index arithmetic to reason about.
fn monitor_rgba(size: u32) -> Vec<u8> {
    let scale = as_f32(size) / 64.0; // geometry is designed on a 64px canvas
    let stride = size as usize;
    let mut buf = Vec::with_capacity(stride.saturating_mul(stride).saturating_mul(4));
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
            let [r, g, b] = ICON_RGB;
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

/// Whether a point (in the 64px design space) is inside the monitor silhouette:
/// the rounded screen, the stand neck, or the stand base.
fn in_monitor(px: f32, py: f32) -> bool {
    // Screen (rounded rectangle).
    rounded_rect(px, py, 9.0, 12.0, 55.0, 44.0, 5.0)
        // Stand neck.
        || ((28.0..=36.0).contains(&px) && (43.0..=49.0).contains(&py))
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
    use super::*;

    #[test]
    fn buffer_has_the_declared_size() {
        // 64 × 64 × 4 (RGBA).
        assert_eq!(monitor_rgba(SIZE).len(), 16_384);
    }

    #[test]
    fn centre_pixel_is_opaque_and_the_icon_colour() {
        let buf = monitor_rgba(SIZE);
        // Centre of a 64×64 image is pixel (32, 32): byte offset (32*64+32)*4. It
        // sits inside the screen rectangle, so it is fully opaque and mono-tinted.
        let px: [u8; 4] = buf
            .get(8320..8324)
            .expect("centre pixel in range")
            .try_into()
            .expect("four bytes");
        let [r, g, b, a] = px;
        assert_eq!(a, 255, "centre must be opaque");
        assert_eq!([r, g, b], ICON_RGB, "centre must be the mono icon colour");
    }

    #[test]
    fn corner_pixel_is_transparent() {
        let buf = monitor_rgba(SIZE);
        assert_eq!(buf.get(0..4), Some(&[0, 0, 0, 0][..]));
    }

    #[test]
    fn icon_builds() {
        assert!(app_icon().is_some());
    }
}

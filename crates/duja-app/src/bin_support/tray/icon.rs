//! A programmatically-generated monochrome sun-glyph tray icon.
//!
//! No asset file is shipped: the 32×32 RGBA glyph is drawn in code (a white
//! core disc with eight rays on a transparent field) so the binary stays
//! self-contained. The pixel buffer is a pure function, unit-tested for shape;
//! wrapping it into a [`tray_icon::Icon`] is the only fallible step.

use std::f32::consts::PI;

/// The icon side length in pixels.
const SIZE: u32 = 32;
/// Radius of the solid sun core.
const CORE_R: f32 = 6.0;
/// Inner/outer radius of the rays.
const RAY_INNER: f32 = 8.5;
const RAY_OUTER: f32 = 13.5;
/// Half-angular-width of each ray (radians).
const RAY_HALF_WIDTH: f32 = 0.28;

/// Build the tray icon, or an error if the pixel buffer is somehow rejected.
///
/// # Errors
/// [`tray_icon::BadIcon`] if the RGBA buffer does not match the declared size
/// (it always does here — this is defensive).
pub(super) fn sun_icon() -> anyhow::Result<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(sun_rgba(), SIZE, SIZE)
        .map_err(|e| anyhow::anyhow!("failed to build the tray icon: {e}"))
}

/// The 32×32 RGBA pixels of the sun glyph (white on transparent).
fn sun_rgba() -> Vec<u8> {
    let side = SIZE as usize;
    let center = (f32::from(u16::try_from(SIZE).unwrap_or(32)) - 1.0) / 2.0;
    let mut buf = Vec::with_capacity(side.saturating_mul(side).saturating_mul(4));

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = f32::from(u16::try_from(x).unwrap_or(0)) - center;
            let dy = f32::from(u16::try_from(y).unwrap_or(0)) - center;
            let pixel: [u8; 4] = if is_glyph(dx, dy) {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            buf.extend_from_slice(&pixel);
        }
    }
    buf
}

/// Whether the pixel offset `(dx, dy)` from the centre is part of the glyph:
/// inside the core disc, or on one of the eight rays.
fn is_glyph(dx: f32, dy: f32) -> bool {
    let dist = dx.hypot(dy);
    if dist <= CORE_R {
        return true;
    }
    if !(RAY_INNER..=RAY_OUTER).contains(&dist) {
        return false;
    }
    // Ray if the angle is within RAY_HALF_WIDTH of a 45° multiple.
    let angle = dy.atan2(dx); // -PI..=PI
    let step = PI / 4.0;
    let nearest = (angle / step).round() * step;
    (angle - nearest).abs() <= RAY_HALF_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_has_the_declared_size() {
        assert_eq!(sun_rgba().len(), 4096); // 32 × 32 × 4 (RGBA)
    }

    #[test]
    fn centre_pixel_is_opaque_white() {
        let buf = sun_rgba();
        // Centre of a 32×32 image is pixel (16, 16): byte offset (16*32+16)*4.
        let pixel = buf.get(2112..2116).expect("centre pixel in range");
        assert_eq!(pixel, &[255, 255, 255, 255]);
    }

    #[test]
    fn corner_pixel_is_transparent() {
        let buf = sun_rgba();
        assert_eq!(buf.get(0..4), Some(&[0, 0, 0, 0][..]));
    }

    #[test]
    fn glyph_covers_a_reasonable_area() {
        let opaque = sun_rgba()
            .chunks_exact(4)
            .filter(|p| p.last() == Some(&255))
            .count();
        // The core disc alone is ~110px; with rays, comfortably more.
        assert!(opaque > 100, "only {opaque} opaque pixels");
    }

    #[test]
    fn icon_builds() {
        assert!(sun_icon().is_ok());
    }
}

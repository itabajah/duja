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

/// The same silhouette as [`monitor_rgba`], but filled with a **colour whirlpool**
/// instead of one flat colour: `size × size × 4` RGBA, row-major, transparent
/// field, 4×4 supersampled.
///
/// This is used only for the **static executable icon** (the file/shortcut glyph),
/// which — being a compiled-in PE resource — cannot follow the in-app accent the
/// way the runtime tray/window icons do. The shape is identical to `monitor_rgba`
/// (same `in_monitor` mask); only the fill differs, so the exe icon is
/// unmistakably the same monitor.
///
/// The fill swirls through `colors` cyclically. Colours are a parameter (the caller
/// passes [`crate::accent::icon_rgb`] values), so no colour literal lives here. An
/// empty `colors` yields a fully transparent buffer.
#[must_use]
pub fn whirlpool_rgba(size: u32, colors: &[[u8; 3]]) -> Vec<u8> {
    let scale = as_f32(size) / DESIGN_SIZE;
    let stride = size as usize;
    let len = stride.saturating_mul(stride).saturating_mul(4);
    // No palette ⇒ nothing to swirl; return a fully transparent buffer of the right
    // size rather than a black silhouette.
    if colors.is_empty() {
        return vec![0; len];
    }
    let mut buf = Vec::with_capacity(len);
    for y in 0..size {
        for x in 0..size {
            // 4×4 supersample, averaging both coverage *and* colour so the edges
            // anti-alias in colour too (the swirl varies within a single pixel).
            let mut inside = 0u32;
            let (mut sr, mut sg, mut sb) = (0.0_f32, 0.0_f32, 0.0_f32);
            for sy in 0..4u32 {
                for sx in 0..4u32 {
                    let px = (as_f32(x) + (as_f32(sx) + 0.5) / 4.0 - 0.5) / scale;
                    let py = (as_f32(y) + (as_f32(sy) + 0.5) / 4.0 - 0.5) / scale;
                    if in_monitor(px, py) {
                        let [r, g, b] = swirl(px, py, colors);
                        sr += f32::from(r);
                        sg += f32::from(g);
                        sb += f32::from(b);
                        inside = inside.saturating_add(1);
                    }
                }
            }
            let pixel: [u8; 4] = if inside > 0 {
                let n = as_f32(inside);
                [
                    to_channel(sr / n),
                    to_channel(sg / n),
                    to_channel(sb / n),
                    to_byte(inside_coverage(inside)),
                ]
            } else {
                [0, 0, 0, 0]
            };
            buf.extend_from_slice(&pixel);
        }
    }
    buf
}

/// Coverage fraction (0.0..=1.0) from the count of inside subsamples (0..=16).
fn inside_coverage(inside: u32) -> f32 {
    as_f32(inside) / 16.0
}

/// The four jewel-tone accents the static exe whirlpool blends, in swirl order
/// (Ruby, Gold, Emerald, Sapphire — Onyx is monochrome and dropped).
///
/// Deliberately **richer than [`crate::accent::icon_rgb`]**: those are tuned for
/// legibility as a tiny flat glyph on a taskbar, whereas the exe icon is a large,
/// standalone artifact with no such constraint, so it uses deeper, more premium
/// gemstone colours. This is the single source for the generator
/// (`examples/gen_exe_icon.rs`) and its drift test.
pub const EXE_ICON_PALETTE: [[u8; 3]; 4] = [
    [0xc4, 0x11, 0x3a], // Ruby     — deep blood-red
    [0xe2, 0xa0, 0x15], // Gold     — amber citrine
    [0x08, 0x84, 0x59], // Emerald  — deep blue-green
    [0x14, 0x42, 0xb0], // Sapphire — royal blue
];

/// How tightly the colour bands spiral (radians of rotation per design pixel of
/// radius). Small enough to read as a gentle whirlpool rather than a busy pinwheel.
const SWIRL_TWIST: f32 = 0.11;

/// The bright "leading" drawn between adjacent colour bands (a cloisonné seam), so
/// the four jewel colours read as distinct segments rather than one smooth blend.
const SEAM_RGB: [u8; 3] = [0xff, 0xff, 0xff];
/// The seam's half-width in design pixels (its full stroke is ~2× this).
const SEAM_HALF_PX: f32 = 1.1;

/// The whirlpool colour at a design-space point: a **solid** colour band chosen by
/// the angle around the centre (twisted by the radius so the bands curve into a
/// spiral), with a dark [`SEAM_RGB`] outline along each boundary between adjacent
/// colours. Returns black for an empty palette (unreachable — the caller masks by
/// `in_monitor` and always passes a palette).
fn swirl(px: f32, py: f32, colors: &[[u8; 3]]) -> [u8; 3] {
    let n = colors.len();
    let Some(first) = colors.first() else {
        return [0, 0, 0];
    };
    let dx = px - DESIGN_SIZE / 2.0;
    let dy = py - DESIGN_SIZE / 2.0;
    let radius = dx.hypot(dy);
    let angle = dy.atan2(dx);
    // Wrap (angle + twist·radius)/2π into 0.0..1.0, then pick the solid band.
    let n_f = as_f32(u32::try_from(n).unwrap_or(1));
    let turns = (angle + SWIRL_TWIST * radius) / core::f32::consts::TAU;
    let t = turns - turns.floor();
    let f = t * n_f;
    let flat = *colors.get(bucket_index(f, n)).unwrap_or(first);
    // Give the flat band a faceted, cut-gem finish before the seam goes on top.
    let band = gem_shade(flat, dx, dy, radius, angle);

    // Distance to the nearest band boundary (an integer of `f`), converted to a
    // roughly constant-pixel-width seam: one unit of `f` spans an arc of
    // `radius · 2π/n` design pixels, so the seam's half-width in `f`-units is
    // `SEAM_HALF_PX / arc` — capped so the bands don't drown near the centre where
    // they all meet.
    let ff = f - f.floor();
    let boundary_dist = ff.min(1.0 - ff);
    let arc = radius * core::f32::consts::TAU / n_f;
    let half = (SEAM_HALF_PX / arc.max(0.001)).min(0.34);
    let seam = 1.0 - smoothstep(half * 0.45, half, boundary_dist);
    [
        lerp_channel(band[0], SEAM_RGB[0], seam),
        lerp_channel(band[1], SEAM_RGB[1], seam),
        lerp_channel(band[2], SEAM_RGB[2], seam),
    ]
}

/// The radius (design px) that normalises the radial shading — the silhouette's
/// screen reaches roughly this far from the centre.
const GEM_RADIUS: f32 = 34.0;

/// Turn a flat band colour into a **faceted cut gem**: a radial brilliant-cut grid
/// of flat facets, each caught by the light at a slightly different brightness
/// (bright facet beside dark facet = sparkle), plus a top-left key light, a glassy
/// rim, and a few near-white glints. This is what reads as real, cut jewellery
/// rather than a plastic gradient.
fn gem_shade(colour: [u8; 3], dx: f32, dy: f32, radius: f32, angle: f32) -> [u8; 3] {
    let rn = (radius / GEM_RADIUS).clamp(0.0, 1.0);

    // Facet grid: concentric rings (table / crown / pavilion), each split into flat
    // angular facets. Each facet's brightness is *coherent* with the key light — a
    // facet whose centre faces the light is bright, one facing away is deep — plus a
    // little per-facet variety, so it reads as a real brilliant cut, not noise.
    let ring: u32 = if rn < 0.30 {
        0
    } else if rn < 0.62 {
        1
    } else {
        2
    };
    let sectors = 13.0 + as_f32(ring) * 6.0;
    let ang01 = angle / core::f32::consts::TAU + 0.5;
    let sector_i = (ang01 * sectors).floor();
    // The facet's own facing direction vs the key light (both in angle space).
    let facet_angle = ((sector_i + 0.5) / sectors - 0.5) * core::f32::consts::TAU;
    let facing = (facet_angle - LIGHT_ANGLE).cos(); // -1 (away) .. +1 (toward light)
    let variety = facet_lum(facet_id(sector_i, ring)) - 0.5; // -0.5..0.5
    let facet_light = facing * 0.30 + variety * 0.16;

    // Glassy rim light and a gentle bright table (centre) — smooth terms that add
    // depth across the facets.
    let rim = smoothstep(0.82, 1.0, rn) * 0.30;
    let table = (0.28 - rn) * 0.20;

    let amt = (facet_light + rim + table).clamp(-0.62, 0.58);
    let base = [
        shade_channel(colour[0], amt),
        shade_channel(colour[1], amt),
        shade_channel(colour[2], amt),
    ];

    // Sparkle: a tight near-white glint where the key light hits, strongest on the
    // facets that face it — so it twinkles rather than washing out.
    let glint = (1.0 - (dx + 12.0).hypot(dy + 10.0) / 7.0).clamp(0.0, 1.0);
    let spark = (glint * (facing * 0.5 + 0.5)).powi(2) * 0.95;
    [
        lerp_channel(base[0], 0xff, spark),
        lerp_channel(base[1], 0xff, spark),
        lerp_channel(base[2], 0xff, spark),
    ]
}

/// The key-light direction in `atan2(dy, dx)` angle space — up-left (≈ −125°).
const LIGHT_ANGLE: f32 = -2.18;

/// A facet's identity from its angular index and ring, hashed for a stable
/// per-facet brightness.
fn facet_id(facet_pos: f32, ring: u32) -> u32 {
    // RATIONALE (cast_possible_truncation, cast_sign_loss): `facet_pos` is a small
    // non-negative angular index (< ~27), so the floor cast is exact and in range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let idx = facet_pos.max(0.0) as u32;
    idx.wrapping_mul(7).wrapping_add(ring.wrapping_mul(131))
}

/// A stable pseudo-random brightness in `0.0..=1.0` for a facet id (integer hash).
fn facet_lum(id: u32) -> f32 {
    let mut x = id.wrapping_mul(0x9E37_79B1);
    x ^= x.wrapping_shr(16);
    x = x.wrapping_mul(0x85EB_CA77);
    x ^= x.wrapping_shr(13);
    // RATIONALE (cast_precision_loss): masked to 16 bits (<= 65535), exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let v = f32::from(u16::try_from(x & 0xFFFF).unwrap_or(0)) / 65535.0;
    v
}

/// Lighten (`amt > 0`, toward white) or darken (`amt < 0`, toward black) one 8-bit
/// channel by `amt` in `-1.0..=1.0`.
fn shade_channel(channel: u8, amt: f32) -> u8 {
    let v = f32::from(channel);
    let out = if amt >= 0.0 {
        v + (255.0 - v) * amt
    } else {
        v * (1.0 + amt)
    };
    to_channel(out)
}

/// Smooth Hermite step: 0 below `edge0`, 1 above `edge1`, eased between.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let span = edge1 - edge0;
    if span <= 0.0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / span).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The colour-ring bucket for `f` in `0.0..n` (a non-negative, integer-valued-ish
/// f32), clamped into `0..n`.
fn bucket_index(f: f32, n: usize) -> usize {
    // RATIONALE (cast_possible_truncation, cast_sign_loss): `f` is `t·n` with `t`
    // in 0.0..1.0 and `n` small, so it is a non-negative value below `n`; the floor
    // cast is exact and in range, and the `min` pins the `t == …` boundary case.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let i = f.floor().max(0.0) as usize;
    i.min(n.saturating_sub(1))
}

/// Linear interpolation between two 8-bit channels at `t` in 0.0..=1.0.
fn lerp_channel(lo: u8, hi: u8, t: f32) -> u8 {
    to_channel(f32::from(lo) + (f32::from(hi) - f32::from(lo)) * t.clamp(0.0, 1.0))
}

/// Round a 0.0..=255.0 channel value to a byte (saturating).
fn to_channel(v: f32) -> u8 {
    // RATIONALE (cast_possible_truncation, cast_sign_loss): the clamp pins the
    // value to 0.0..=255.0 and `round` makes it integral, so the cast neither
    // truncates a meaningful fraction, loses a sign, nor overflows a `u8`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = v.clamp(0.0, 255.0).round() as u8;
    byte
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
    use super::{EXE_ICON_PALETTE, WINDOW_SIZE, app_icon, monitor_rgba, whirlpool_rgba};
    use crate::accent::{ACCENT_ORDER, icon_rgb};

    /// The size the tray asks for.
    const TRAY_SIZE: u32 = 32;
    /// An arbitrary colour no constant in this crate uses.
    const SENTINEL: [u8; 3] = [1, 2, 3];

    /// The premium jewel palette the exe whirlpool blends.
    fn whirlpool_colours() -> [[u8; 3]; 4] {
        EXE_ICON_PALETTE
    }

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

    #[test]
    fn whirlpool_has_the_declared_size() {
        let colours = whirlpool_colours();
        assert_eq!(whirlpool_rgba(TRAY_SIZE, &colours).len(), 32 * 32 * 4);
        assert_eq!(whirlpool_rgba(256, &colours).len(), 256 * 256 * 4);
    }

    #[test]
    fn whirlpool_shares_the_monitor_silhouette() {
        // The exe icon must be the *same shape* as the runtime icon — only the fill
        // differs. Both use `in_monitor` with identical 4×4 supersampling, so the
        // alpha (coverage) channel must match bit-for-bit at every pixel; only RGB
        // differs. This fences the shape against ever drifting from `monitor_rgba`.
        let flat = monitor_rgba(WINDOW_SIZE, SENTINEL);
        let swirl = whirlpool_rgba(WINDOW_SIZE, &whirlpool_colours());
        assert_eq!(flat.len(), swirl.len());
        for y in 0..WINDOW_SIZE {
            for x in 0..WINDOW_SIZE {
                let [_, _, _, fa] = pixel(&flat, WINDOW_SIZE, x, y);
                let [_, _, _, sa] = pixel(&swirl, WINDOW_SIZE, x, y);
                assert_eq!(fa, sa, "coverage differs at ({x},{y})");
            }
        }
    }

    #[test]
    fn whirlpool_centre_is_opaque_and_corner_is_transparent() {
        let buf = whirlpool_rgba(WINDOW_SIZE, &whirlpool_colours());
        let [_, _, _, centre_a] = pixel(&buf, WINDOW_SIZE, 32, 32);
        assert_eq!(centre_a, 255, "screen centre must be fully opaque");
        assert_eq!(pixel(&buf, WINDOW_SIZE, 0, 0), [0, 0, 0, 0], "corner");
    }

    #[test]
    fn whirlpool_contains_all_four_hues() {
        // Prove the fill really carries four distinct colour *families* — not a
        // monochrome or a two-colour gradient. Checks hue families (red/gold/green/
        // blue) rather than exact hexes so it is robust to the gem shading, which
        // lightens and deepens each band. Uses 256px for wide bands.
        let buf = whirlpool_rgba(256, &whirlpool_colours());
        let (mut red, mut gold, mut green, mut blue) = (false, false, false, false);
        for p in buf.chunks_exact(4) {
            let [r, g, b, a] = *p else { continue };
            if a != 255 {
                continue;
            }
            let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
            if r > g + 60 && r > b + 40 {
                red = true; // ruby: red dominant
            } else if g > r + 40 && g > b + 30 {
                green = true; // emerald: green dominant
            } else if b > r + 40 && b > g + 40 {
                blue = true; // sapphire: blue dominant
            } else if r > b + 60 && g > b + 40 {
                gold = true; // gold: warm, low blue
            }
        }
        assert!(red && gold && green && blue, "missing a hue family");

        let mut distinct: Vec<[u8; 3]> = buf
            .chunks_exact(4)
            .filter_map(|p| match p {
                [r, g, b, 255] => Some([*r, *g, *b]),
                _ => None,
            })
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() > 200,
            "only {} distinct colours — not a shaded swirl",
            distinct.len()
        );
    }

    #[test]
    fn whirlpool_is_transparent_without_colours() {
        // Defensive: an empty palette yields a fully transparent buffer, never a
        // panic (the swirl helper guards the empty case).
        let buf = whirlpool_rgba(TRAY_SIZE, &[]);
        assert!(buf.chunks_exact(4).all(|p| p == [0, 0, 0, 0]));
    }
}

//! Drift guard for the committed brand artifacts: the executable icon
//! (`assets/duja.ico`) and the README brand mark (`docs/images/duja-mark.png`).
//!
//! Both are generated caches of [`duja_ui::icon::dark_whirlpool_rgba`],
//! committed so `build.rs` only has to embed a file and the README only has to
//! reference one. These tests decode the committed artifacts and assert every
//! frame matches the generator, so the shape/colours cannot silently drift from
//! their single source. If one fails, regenerate:
//!
//! ```text
//! cargo run -p duja-app --example gen_exe_icon
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use duja_ui::icon::{EXE_ICON_PALETTE, dark_whirlpool_rgba};

/// Must match `examples/gen_exe_icon.rs`.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Must match `examples/gen_exe_icon.rs`.
const MARK_SIZE: u32 = 512;

const REGEN: &str = "regenerate with: cargo run -p duja-app --example gen_exe_icon";

/// Assert a freshly generated buffer matches a committed brand artifact.
///
/// The dark re-light uses `exp`/`powf`/`atan2`, whose last bit is not identical
/// across platform libm implementations, so regenerating on a *different* OS
/// than the one that produced the committed asset rounds a handful of the
/// quarter-million pixels to a neighbouring byte (the committed PNG is
/// byte-exact on Windows/macOS yet a few pixels differ by ±1 under glibc). This
/// is pure floating-point noise, not drift: a real edit to the art — regenerated
/// per the failure hint — is byte-exact again on its own platform, and a
/// *forgotten* regeneration compares the stale asset against the new formula,
/// which diverges on far more than a sliver of pixels and by far more than a
/// byte or two.
///
/// So we pin the *shape* — the alpha (integer supersample coverage) — bit for
/// bit, and allow only sub-perceptual colour noise: no channel off by more than
/// a few levels, and under 1 % of pixels touched at all.
fn assert_matches_generator(actual: &[u8], expected: &[u8], what: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{what}: byte length differs; {REGEN}"
    );
    let total = actual.len() / 4;
    let mut noisy = 0usize;
    for (idx, (got, want)) in actual
        .chunks_exact(4)
        .zip(expected.chunks_exact(4))
        .enumerate()
    {
        assert_eq!(
            got.last(),
            want.last(),
            "{what}: coverage (alpha) differs at pixel {idx}; {REGEN}"
        );
        let channel_diff = got
            .iter()
            .zip(want.iter())
            .take(3)
            .map(|(a, b)| u32::from(*a).abs_diff(u32::from(*b)))
            .max()
            .unwrap_or(0);
        assert!(
            channel_diff <= 4,
            "{what}: colour off by {channel_diff} (> 4) at pixel {idx} — a real change, not libm noise; {REGEN}"
        );
        if channel_diff != 0 {
            noisy = noisy.saturating_add(1);
        }
    }
    // libm noise touches only a sliver of pixels; real drift touches a lot.
    assert!(
        noisy.saturating_mul(100) < total,
        "{what}: {noisy}/{total} pixels differ (≥ 1 %) — a real change, not libm noise; {REGEN}"
    );
}

#[test]
fn exe_icon_matches_the_dark_whirlpool_generator() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("duja.ico");
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("committed {} is missing ({e}); {REGEN}", path.display()));
    let dir = ico::IconDir::read(file).expect("assets/duja.ico is not a valid .ico");
    let colours = EXE_ICON_PALETTE;

    let present: Vec<u32> = dir.entries().iter().map(ico::IconDirEntry::width).collect();
    for size in SIZES {
        assert!(present.contains(&size), "size {size}px missing; {REGEN}");
    }

    for entry in dir.entries() {
        let size = entry.width();
        let image = entry.decode().expect("icon frame decodes");
        let expected = dark_whirlpool_rgba(size, &colours);
        assert_matches_generator(
            image.rgba_data(),
            &expected,
            &format!("icon frame {size}px"),
        );
    }
}

#[test]
fn brand_mark_matches_the_dark_whirlpool_generator() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("images")
        .join("duja-mark.png");
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("committed {} is missing ({e}); {REGEN}", path.display()));
    let image = ico::IconImage::read_png(file).expect("docs/images/duja-mark.png decodes");
    assert_eq!(
        (image.width(), image.height()),
        (MARK_SIZE, MARK_SIZE),
        "mark is not {MARK_SIZE}px; {REGEN}"
    );
    let expected = dark_whirlpool_rgba(MARK_SIZE, &EXE_ICON_PALETTE);
    assert_matches_generator(image.rgba_data(), &expected, "brand mark");
}

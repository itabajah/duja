//! Drift guard for the committed brand artifacts: the executable icon
//! (`assets/duja.ico`) and the README brand mark (`docs/images/duja-mark.png`).
//!
//! Both are generated caches of [`duja_ui::icon::dark_whirlpool_rgba`],
//! committed so `build.rs` only has to embed a file and the README only has to
//! reference one. These tests decode the committed artifacts and assert every
//! frame is pixel-identical to the generator, so the shape/colours cannot
//! silently drift from their single source. If one fails, regenerate:
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
        assert_eq!(
            image.rgba_data(),
            expected.as_slice(),
            "committed icon frame {size}px differs from dark_whirlpool_rgba; {REGEN}"
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
    assert_eq!(
        image.rgba_data(),
        expected.as_slice(),
        "committed brand mark differs from dark_whirlpool_rgba; {REGEN}"
    );
}

//! Drift guard for the committed executable icon, `assets/duja.ico`.
//!
//! The `.ico` is a generated cache of [`duja_ui::icon::whirlpool_rgba`], committed
//! so `build.rs` only has to embed a file. This test decodes the committed icon and
//! asserts every frame is pixel-identical to the generator, so the shape/colours
//! cannot silently drift from their single source. If it fails, regenerate:
//!
//! ```text
//! cargo run -p duja-app --example gen_exe_icon
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use duja_ui::icon::{EXE_ICON_PALETTE, whirlpool_rgba};

/// Must match `examples/gen_exe_icon.rs`.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

const REGEN: &str = "regenerate with: cargo run -p duja-app --example gen_exe_icon";

#[test]
fn exe_icon_matches_the_whirlpool_generator() {
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
        let expected = whirlpool_rgba(size, &colours);
        assert_eq!(
            image.rgba_data(),
            expected.as_slice(),
            "committed icon frame {size}px differs from whirlpool_rgba; {REGEN}"
        );
    }
}

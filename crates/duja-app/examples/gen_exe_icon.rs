//! Regenerates the static executable icon, `crates/duja-app/assets/duja.ico`,
//! and the brand mark, `docs/images/duja-mark.png`.
//!
//! The exe's file/shortcut icon is a compiled-in PE resource, so — unlike the
//! runtime tray/window icons — it cannot follow the in-app accent. It is
//! therefore the fixed **dark whirlpool** brand mark (*duja* is Arabic for
//! darkness): near-black gems whose spiral seams glow in the four chromatic
//! accents (Ruby, Gold, Emerald, Sapphire; Onyx is monochrome and would smear
//! grey), drawn on the *same* monitor silhouette as the runtime icon via
//! [`duja_ui::icon::dark_whirlpool_rgba`]. The README/social banners embed the
//! same art from `docs/images/duja-mark.png`, emitted here at 512 px.
//!
//! Both artifacts are committed (build.rs only embeds the `.ico`); this example
//! is how you refresh them. A drift test (`tests/exe_icon.rs`) fails if either
//! committed file stops matching the generator, pointing back here. Run:
//!
//! ```text
//! cargo run -p duja-app --example gen_exe_icon
//! ```

use std::path::Path;

use duja_ui::icon::{EXE_ICON_PALETTE, dark_whirlpool_rgba};

/// The sizes packed into the `.ico`. Windows picks the nearest for each context
/// (16/32 in Explorer lists, 256 for large thumbnails). Must match the drift test.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// The brand-mark render size: 2× the largest display size in the banners, so
/// the README hero stays crisp on high-DPI screens. Must match the drift test.
const MARK_SIZE: u32 = 512;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let colours = EXE_ICON_PALETTE;
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in SIZES {
        let rgba = dark_whirlpool_rgba(size, &colours);
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        icon_dir.add_entry(ico::IconDirEntry::encode(&image)?);
    }

    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("duja.ico");
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::File::create(&out)?;
    icon_dir.write(file)?;
    println!("wrote {} ({} sizes)", out.display(), SIZES.len());

    let rgba = dark_whirlpool_rgba(MARK_SIZE, &colours);
    let mark = ico::IconImage::from_rgba_data(MARK_SIZE, MARK_SIZE, rgba);
    let mark_out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("images")
        .join("duja-mark.png");
    mark.write_png(std::fs::File::create(&mark_out)?)?;
    println!("wrote {} ({MARK_SIZE}px)", mark_out.display());
    Ok(())
}

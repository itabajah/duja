//! Regenerates the static executable icon, `crates/duja-app/assets/duja.ico`.
//!
//! The exe's file/shortcut icon is a compiled-in PE resource, so — unlike the
//! runtime tray/window icons — it cannot follow the in-app accent. It is therefore
//! a fixed **colour whirlpool** of the four chromatic accents (Ruby, Gold, Emerald,
//! Sapphire; Onyx is monochrome and would smear grey), drawn on the *same* monitor
//! silhouette as the runtime icon via [`duja_ui::icon::whirlpool_rgba`].
//!
//! The `.ico` is committed (build.rs only embeds it); this example is how you
//! refresh it. A drift test (`tests/exe_icon.rs`) fails if the committed file stops
//! matching the generator, pointing back here. Run:
//!
//! ```text
//! cargo run -p duja-app --example gen_exe_icon
//! ```

use std::path::Path;

use duja_ui::icon::{EXE_ICON_PALETTE, whirlpool_rgba};

/// The sizes packed into the `.ico`. Windows picks the nearest for each context
/// (16/32 in Explorer lists, 256 for large thumbnails). Must match the drift test.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let colours = EXE_ICON_PALETTE;
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in SIZES {
        let rgba = whirlpool_rgba(size, &colours);
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
    Ok(())
}

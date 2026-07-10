//! Build script: embed the Windows application manifest.
//!
//! Duja must be `PerMonitorV2` DPI-aware so overlays and the flyout land on
//! physical pixels across mixed-DPI monitors. The manifest is embedded into the
//! executable here (no external `.manifest` file, no `mt.exe` step); a
//! successful release build that carries it is the verification.

fn main() {
    // Only meaningful on Windows; on other targets this is a no-op.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::DpiAwareness;
        use embed_manifest::{embed_manifest, new_manifest};

        let manifest =
            new_manifest("Io.Github.Itabajah.Duja").dpi_awareness(DpiAwareness::PerMonitorV2);
        if let Err(err) = embed_manifest(manifest) {
            // A failed embed must fail the build loudly (panic/expect are denied
            // by the lint wall, so exit non-zero after emitting the error).
            println!("cargo:warning=failed to embed application manifest: {err}");
            std::process::exit(1);
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}

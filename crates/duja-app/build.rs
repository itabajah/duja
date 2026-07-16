//! Build script: embed the Windows application manifest and the executable icon.
//!
//! Two PE resources are embedded here on Windows (both no-ops on other targets):
//!
//! * the `PerMonitorV2` DPI-awareness **manifest** (via `embed-manifest`), so
//!   overlays and the flyout land on physical pixels across mixed-DPI monitors; and
//! * the executable's file/shortcut **icon** (via `embed-resource` compiling the
//!   one-line `duja.rc`), a static "whirlpool" of the accent colours. Unlike the
//!   runtime tray/window icons it cannot follow the in-app accent, so it is a fixed
//!   blend — see `assets/duja.ico` and `examples/gen_exe_icon.rs`.
//!
//! A successful release build that carries both is the verification.

fn main() {
    // Only meaningful on Windows; on other targets these are no-ops.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_dpi_manifest();
        embed_exe_icon();
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// Embed the `PerMonitorV2` DPI-awareness application manifest.
fn embed_dpi_manifest() {
    use embed_manifest::manifest::DpiAwareness;
    use embed_manifest::{embed_manifest, new_manifest};

    let manifest =
        new_manifest("Io.Github.Itabajah.Duja").dpi_awareness(DpiAwareness::PerMonitorV2);
    if let Err(err) = embed_manifest(manifest) {
        // A failed embed must fail the build loudly (panic/expect are denied by the
        // lint wall, so exit non-zero after emitting the error).
        println!("cargo:warning=failed to embed application manifest: {err}");
        std::process::exit(1);
    }
}

/// Compile `duja.rc` so the whirlpool icon (`assets/duja.ico`) is embedded as the
/// executable's icon resource.
fn embed_exe_icon() {
    println!("cargo:rerun-if-changed=duja.rc");
    println!("cargo:rerun-if-changed=assets/duja.ico");
    if let Err(err) = embed_resource::compile("duja.rc", embed_resource::NONE).manifest_required() {
        println!("cargo:warning=failed to embed the executable icon: {err}");
        std::process::exit(1);
    }
}

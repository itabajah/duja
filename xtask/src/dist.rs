//! `cargo xtask dist --version X.Y.Z` — stage the portable Windows artifact.
//!
//! Assembles `target/dist/duja-<ver>-windows-x64/` from the already-built
//! release binaries plus the licences and README (the "license bundling" this
//! crate's description promises), then zips it to
//! `target/dist/duja-<ver>-windows-x64.zip` with PowerShell `Compress-Archive`
//! (so no archiving crate — this crate stays dependency-free).
//!
//! Checksums, minisign signatures, the Inno Setup installer, and the GitHub
//! Release are the release workflow's job (they need CI secrets and external
//! tools); this task only produces the portable staging dir + zip, and is
//! runnable locally for parity.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The files copied alongside the binaries into the portable archive.
const EXTRA_FILES: [&str; 3] = ["LICENSE-MIT", "LICENSE-APACHE", "README.md"];

/// Run the `dist` task with the arguments following `dist` on the command line.
///
/// # Errors
/// Returns a human-readable message if `--version` is missing, a source file is
/// absent (usually: the release build has not run), or an I/O / zip step fails.
pub(crate) fn run(mut args: std::env::Args) -> Result<(), String> {
    let mut version: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                version = Some(args.next().ok_or("`--version` needs a value")?);
            }
            other => return Err(format!("unknown `dist` argument `{other}`")),
        }
    }
    let version = version.ok_or("usage: cargo xtask dist --version X.Y.Z")?;

    let root = repo_root()?;
    let release = root.join("target").join("release");
    let dist = root.join("target").join("dist");
    let stage_name = format!("duja-{version}-windows-x64");
    let stage = dist.join(&stage_name);

    // Start from a clean staging dir so a rerun never ships stale files.
    if stage.exists() {
        std::fs::remove_dir_all(&stage)
            .map_err(|e| format!("clearing {}: {e}", stage.display()))?;
    }
    std::fs::create_dir_all(&stage).map_err(|e| format!("creating {}: {e}", stage.display()))?;

    // The two release binaries (`.exe` on Windows via EXE_SUFFIX).
    for bin in ["duja", "dujactl"] {
        let file = format!("{bin}{}", std::env::consts::EXE_SUFFIX);
        let src = release.join(&file);
        if !src.exists() {
            return Err(format!(
                "missing {} — run `cargo build --release -p duja-app -p dujactl` first",
                src.display()
            ));
        }
        copy_into(&src, &stage)?;
    }
    // Licences + README.
    for name in EXTRA_FILES {
        copy_into(&root.join(name), &stage)?;
    }

    let zip = dist.join(format!("{stage_name}.zip"));
    if zip.exists() {
        std::fs::remove_file(&zip).map_err(|e| format!("clearing {}: {e}", zip.display()))?;
    }
    compress(&stage, &zip)?;

    println!("staged  {}", stage.display());
    println!("archive {}", zip.display());
    Ok(())
}

/// The repository root — the parent of this crate's manifest directory.
fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot resolve the repository root".to_owned())
}

/// Copy `src` into directory `dir`, keeping its file name.
fn copy_into(src: &Path, dir: &Path) -> Result<(), String> {
    let name = src
        .file_name()
        .ok_or_else(|| format!("{} has no file name", src.display()))?;
    if !src.exists() {
        return Err(format!("missing {}", src.display()));
    }
    std::fs::copy(src, dir.join(name)).map_err(|e| format!("copying {}: {e}", src.display()))?;
    Ok(())
}

/// Zip `stage` (the folder, so it appears at the archive root) into `zip` via
/// PowerShell `Compress-Archive`. Keeps this crate free of an archiving
/// dependency; the release workflow already runs on Windows.
fn compress(stage: &Path, zip: &Path) -> Result<(), String> {
    let script = format!(
        "Compress-Archive -Path '{}' -DestinationPath '{}' -Force",
        stage.display(),
        zip.display()
    );
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NoLogo",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .status()
        .map_err(|e| format!("launching PowerShell to zip: {e}"))?;
    if !status.success() {
        return Err(format!("Compress-Archive failed ({status})"));
    }
    Ok(())
}

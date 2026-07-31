//! `cargo xtask dist --version X.Y.Z` — stage the shippable artifact for a host.
//!
//! Two targets today, picked from the host unless `--target` overrides it:
//!
//! - **Windows** — assembles `target/dist/duja-<ver>-windows-x64/` from the
//!   already-built release binaries plus the licences and README (the "license
//!   bundling" this crate's description promises), then zips it with PowerShell
//!   `Compress-Archive` (so no archiving crate — this crate stays
//!   dependency-free).
//! - **macOS** — fuses the two already-built thin binaries into universal ones
//!   with `lipo`, assembles `Duja.app` around them ([`crate::bundle`]), seals it
//!   with `codesign`, and wraps it in a drag-to-install disk image with
//!   `hdiutil`.
//!
//! Neither target *builds* anything: like the Windows path always has, `dist`
//! stages what `cargo build --release` already produced and says exactly which
//! command is missing if it did not.
//!
//! # What is verified where
//!
//! The decisions — the plist, the bundle layout, the artifact names, the version
//! alphabet, the host→target mapping — live in [`crate::bundle`] and
//! [`crate::version`] and are unit-tested on **every** lane. What is left in
//! this module is filesystem plumbing plus four external tools (`powershell`,
//! `lipo`, `codesign`, `hdiutil`) that only exist on their own host; those are
//! exercised by the `release` workflow's `workflow_dispatch` dry run, which
//! builds and packages without publishing.
//!
//! Checksums, minisign signatures, the Inno Setup installer, notarization, and
//! the GitHub Release are the release workflow's job (they need CI secrets and
//! external tools); this task produces the staging trees and the two container
//! artifacts, and is runnable locally for parity.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bundle::{self, BundleInputs};
use crate::repo_root;
use crate::version::Version;

/// The files copied alongside the binaries into every artifact — at the archive
/// root on Windows, in `Contents/Resources` inside the macOS bundle.
const EXTRA_FILES: [&str; 3] = ["LICENSE-MIT", "LICENSE-APACHE", "README.md"];

/// The binaries Duja ships.
const BINARIES: [&str; 2] = [bundle::MAIN_EXECUTABLE, bundle::HELPER_EXECUTABLE];

/// The two Rust targets fused into the universal macOS binary, in `lipo` order.
const MAC_ARCHES: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// `codesign`'s ad-hoc identity: a valid signature with no certificate behind
/// it. Enough to *run* (Apple Silicon refuses to execute an unsigned binary at
/// all); not enough for Gatekeeper on a downloaded copy, which is what the
/// release workflow's notarization path is for.
const AD_HOC: &str = "-";

/// Which platform's artifact to stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The portable zip plus the tree the Inno Setup script installs.
    Windows,
    /// The universal `.app` bundle and its disk image.
    Macos,
}

impl Target {
    /// The target this host packages for by default.
    ///
    /// Deliberately a total function over the hosts that *have* packaging rather
    /// than a `windows`-else fallback: on Linux there is no answer yet, and
    /// silently staging a Windows tree there would produce an artifact nobody
    /// asked for. Linux packaging (`AppImage` plus a native format) is P7.
    ///
    /// # Errors
    /// Returns a message naming the `--target` escape hatch on a host with no
    /// packaging of its own.
    fn host() -> Result<Target, String> {
        if cfg!(target_os = "windows") {
            Ok(Target::Windows)
        } else if cfg!(target_os = "macos") {
            Ok(Target::Macos)
        } else {
            Err(
                "`dist` has no packaging for this host yet (Linux packaging is P7); \
                 pass `--target windows|macos` to stage one explicitly"
                    .to_owned(),
            )
        }
    }

    /// Parse an explicit `--target` value.
    ///
    /// # Errors
    /// Returns a message listing the accepted values.
    fn parse(raw: &str) -> Result<Target, String> {
        match raw {
            "windows" => Ok(Target::Windows),
            "macos" => Ok(Target::Macos),
            other => Err(format!(
                "unknown `--target` `{other}` (expected `windows` or `macos`)"
            )),
        }
    }
}

/// Run the `dist` task with the arguments following `dist` on the command line.
///
/// # Errors
/// Returns a human-readable message if `--version` is missing or malformed, a
/// source file is absent (usually: the release build has not run), or an I/O,
/// archiving, or packaging-tool step fails.
pub(crate) fn run(mut args: std::env::Args) -> Result<(), String> {
    let mut version: Option<Version> = None;
    let mut target: Option<Target> = None;
    let mut identity: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                let raw = args.next().ok_or("`--version` needs a value")?;
                version = Some(Version::parse(&raw)?);
            }
            "--target" => {
                let raw = args.next().ok_or("`--target` needs a value")?;
                target = Some(Target::parse(&raw)?);
            }
            "--sign" => identity = Some(args.next().ok_or("`--sign` needs a value")?),
            other => return Err(format!("unknown `dist` argument `{other}`")),
        }
    }
    let version = version.ok_or("usage: cargo xtask dist --version X.Y.Z")?;
    let target = match target {
        Some(explicit) => explicit,
        None => Target::host()?,
    };

    let root = repo_root()?;
    let dist = root.join("target").join("dist");
    match target {
        Target::Windows => windows(&root, &dist, &version),
        Target::Macos => macos(
            &root,
            &dist,
            &version,
            identity.as_deref().unwrap_or(AD_HOC),
        ),
    }
}

/// Stage `duja-<ver>-windows-x64/` and zip it.
fn windows(root: &Path, dist: &Path, version: &Version) -> Result<(), String> {
    let release = root.join("target").join("release");
    let stage_name = format!("duja-{version}-windows-x64");
    let stage = dist.join(&stage_name);
    fresh_dir(&stage)?;

    // The two release binaries (`.exe` on Windows via EXE_SUFFIX).
    for bin in BINARIES {
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

/// Fuse, assemble, sign, and image the macOS artifact.
///
/// The order is not arrangeable: `lipo` rewrites the Mach-O, the bundle seal
/// covers the `Info.plist` and every file under `Contents`, and the disk image
/// is a snapshot of the signed tree — so **signing is the last mutation**, after
/// the universal binaries exist and after the bundle is complete. Nested code
/// (`dujactl`) is signed before the bundle that encloses it, because sealing the
/// bundle records the signatures it finds inside.
fn macos(root: &Path, dist: &Path, version: &Version, identity: &str) -> Result<(), String> {
    // 1. Resolve every input *before* creating anything, so the common failure —
    //    "you have not built both slices yet" — leaves no half-staged tree.
    let main_slices = slices(root, bundle::MAIN_EXECUTABLE)?;
    let helper_slices = slices(root, bundle::HELPER_EXECUTABLE)?;

    // 2. Two thin binaries per program → one universal binary each, in a scratch
    //    dir so a rerun never lipos yesterday's output into today's.
    let stage = dist.join(bundle::stage_dir_name(version));
    fresh_dir(&stage)?;
    let fused_dir = dist.join("universal");
    fresh_dir(&fused_dir)?;
    let main = fuse(&fused_dir, bundle::MAIN_EXECUTABLE, &main_slices)?;
    let helper = fuse(&fused_dir, bundle::HELPER_EXECUTABLE, &helper_slices)?;

    // 3. Duja.app around them.
    let resources: Vec<PathBuf> = EXTRA_FILES.iter().map(|name| root.join(name)).collect();
    let app = bundle::assemble(
        &stage,
        version,
        &BundleInputs {
            main: &main,
            helper: &helper,
            resources: &resources,
        },
    )?;

    // 4. Seal it: nested code first, then the bundle, then verify the seal.
    codesign(
        identity,
        &app.executable(bundle::HELPER_EXECUTABLE),
        Some(&format!(
            "{}.{}",
            bundle::BUNDLE_ID,
            bundle::HELPER_EXECUTABLE
        )),
    )?;
    codesign(identity, app.root(), None)?;
    verify_signature(app.root())?;

    // 5. The drag-to-install target, then the image itself.
    link_applications(&stage)?;
    let dmg = dist.join(bundle::dmg_file_name(version));
    if dmg.exists() {
        std::fs::remove_file(&dmg).map_err(|e| format!("clearing {}: {e}", dmg.display()))?;
    }
    image(&stage, &dmg, version)?;

    println!("staged  {}", app.root().display());
    println!("image   {}", dmg.display());
    Ok(())
}

/// The per-arch release builds of `bin`, one per [`MAC_ARCHES`] entry.
///
/// # Errors
/// Names the first missing slice **and the exact `cargo build` that produces
/// it**: forgetting the second `--target` is the way this path fails in
/// practice, and a bare "not found" would not say which of the two is absent.
fn slices(root: &Path, bin: &str) -> Result<Vec<PathBuf>, String> {
    let target_dir = root.join("target");
    let mut thin = Vec::new();
    for arch in MAC_ARCHES {
        let path = target_dir.join(arch).join("release").join(bin);
        if !path.exists() {
            return Err(format!(
                "missing {} — run `cargo build --release --target {arch} -p duja-app -p dujactl` first",
                path.display()
            ));
        }
        thin.push(path);
    }
    Ok(thin)
}

/// `lipo` the thin builds of `bin` into one universal binary under `into`,
/// returning its path.
fn fuse(into: &Path, bin: &str, thin: &[PathBuf]) -> Result<PathBuf, String> {
    let out = into.join(bin);
    let mut cmd = Command::new("lipo");
    cmd.arg("-create").arg("-output").arg(&out).args(thin);
    tool(cmd, "lipo")?;
    Ok(out)
}

/// Sign `path` with `identity`, optionally forcing the signing identifier.
///
/// `--options runtime` enables the hardened runtime: Duja has no JIT, loads no
/// unsigned plug-ins, and needs no entitlement exceptions, so it costs nothing —
/// and notarization *requires* it, so applying it on the ad-hoc path too keeps
/// the locally staged artifact on the same code path the released one takes.
/// A timestamp needs Apple's timestamp server and a real certificate, so the
/// ad-hoc path opts out explicitly rather than failing offline.
fn codesign(identity: &str, path: &Path, identifier: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new("codesign");
    cmd.arg("--force")
        .arg("--options")
        .arg("runtime")
        .arg("--sign")
        .arg(identity);
    if identity == AD_HOC {
        cmd.arg("--timestamp=none");
    } else {
        cmd.arg("--timestamp");
    }
    if let Some(id) = identifier {
        cmd.arg("--identifier").arg(id);
    }
    cmd.arg(path);
    tool(cmd, "codesign")
}

/// Re-read the seal we just wrote, so a bundle that cannot validate fails here
/// rather than on a user's machine.
fn verify_signature(app: &Path) -> Result<(), String> {
    let mut cmd = Command::new("codesign");
    cmd.arg("--verify")
        .arg("--strict")
        .arg("--verbose=2")
        .arg(app);
    tool(cmd, "codesign --verify")
}

/// Add the `/Applications` symlink beside the bundle, so mounting the image
/// gives the familiar drag-the-app-onto-the-folder window.
///
/// The `cfg` hides no decision: `std::os::unix::fs::symlink` does not exist off
/// unix, and neither does the rest of this function's neighbourhood (`hdiutil`).
fn link_applications(stage: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let link = stage.join("Applications");
        std::os::unix::fs::symlink("/Applications", &link)
            .map_err(|e| format!("linking /Applications at {}: {e}", link.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = stage;
        Err("staging the macOS disk image needs a unix host (the /Applications symlink)".to_owned())
    }
}

/// Wrap `stage` in a compressed read-only disk image.
///
/// `HFS+` rather than the newer APFS: an APFS image is unreadable before macOS
/// 10.13, and a compressed HFS+ image costs nothing on the systems Duja does
/// support. `UDZO` is the zlib-compressed read-only format every macOS reads.
fn image(stage: &Path, dmg: &Path, version: &Version) -> Result<(), String> {
    let mut cmd = Command::new("hdiutil");
    cmd.arg("create")
        .arg("-volname")
        .arg(bundle::volume_name(version))
        .arg("-srcfolder")
        .arg(stage)
        .arg("-fs")
        .arg("HFS+")
        .arg("-format")
        .arg("UDZO")
        .arg("-ov")
        .arg(dmg);
    tool(cmd, "hdiutil")
}

/// Run an external packaging tool, reporting a non-zero exit as a message that
/// names the tool. Stdio is inherited, so the tool's own diagnostics land in the
/// build log next to this error.
fn tool(mut cmd: Command, name: &str) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("launching `{name}`: {e} (is it installed and on PATH?)"))?;
    if !status.success() {
        return Err(format!("`{name}` failed ({status})"));
    }
    Ok(())
}

/// Create `dir` empty, removing anything a previous run left there so a rerun
/// never ships a stale file.
fn fresh_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("clearing {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))
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
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NoLogo",
        "-NonInteractive",
        "-Command",
        &script,
    ]);
    tool(cmd, "Compress-Archive")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The *choice* of target, not a literal. Packaging for the wrong platform
    /// is silent — a Windows zip staged on a Mac looks like a successful run —
    /// so the mapping is pinned per lane rather than left at the call site.
    /// Reds on the macOS lane if the arms are swapped, on Windows if the
    /// fallback is widened, and on Linux if the "no packaging here" answer is
    /// quietly turned into one of the two.
    #[test]
    fn the_default_target_is_this_host_rather_than_a_literal() {
        let host = Target::host();
        if cfg!(target_os = "windows") {
            assert_eq!(host, Ok(Target::Windows));
        } else if cfg!(target_os = "macos") {
            assert_eq!(host, Ok(Target::Macos));
        } else {
            let err = host.expect_err("no packaging on this host");
            assert!(err.contains("--target"), "{err}");
        }
    }

    #[test]
    fn an_explicit_target_overrides_the_host() {
        assert_eq!(Target::parse("windows"), Ok(Target::Windows));
        assert_eq!(Target::parse("macos"), Ok(Target::Macos));
        let err = Target::parse("linux").expect_err("linux");
        assert!(err.contains("expected"), "{err}");
    }

    /// Both artifacts carry the same two programs, and the macOS bundle's names
    /// are the ones the `Info.plist` and the signing step reference.
    #[test]
    fn the_shipped_binaries_are_the_ones_the_bundle_names() {
        assert_eq!(
            BINARIES,
            [bundle::MAIN_EXECUTABLE, bundle::HELPER_EXECUTABLE]
        );
    }

    /// `lipo` order is arch order; a universal binary missing a slice runs on
    /// half the Macs and is invisible until one of them tries.
    #[test]
    fn the_universal_binary_covers_both_apple_architectures() {
        assert!(MAC_ARCHES.contains(&"aarch64-apple-darwin"));
        assert!(MAC_ARCHES.contains(&"x86_64-apple-darwin"));
    }
}

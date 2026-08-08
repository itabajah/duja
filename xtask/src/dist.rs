//! `cargo xtask dist --version X.Y.Z` — stage the shippable artifact for a host.
//!
//! Three targets, picked from the host unless `--target` overrides it:
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
//! - **Linux** — the Windows path's twin: the same binaries, licences and README
//!   plus a `.desktop` entry and an icon, tarred with `tar`. No installer and no
//!   bundle, and `packaging/linux/README.md` carries why — a `.deb` or an
//!   `AppImage` makes a dependency claim that cannot be checked from a machine
//!   which has never run this binary.
//!
//! No target *builds* anything: like the Windows path always has, `dist` stages
//! what `cargo build --release` already produced and says exactly which command is
//! missing if it did not.
//!
//! Two of the three are confined to their own host, by different means. macOS
//! needs `lipo`, `codesign` and `hdiutil`, so it fails on anything else by not
//! finding them. Linux needs only `tar`, which Windows now ships — so it refuses
//! explicitly instead, because the thing it cannot do there is set a permission
//! bit, and that failure would not surface until a user tried to run the binary.
//!
//! # What is verified where
//!
//! The decisions — the plist, the bundle layout, the artifact names, the version
//! alphabet, the host→target mapping — live in [`crate::bundle`] and
//! [`crate::version`] and are unit-tested on **every** lane. What is left in
//! this module is filesystem plumbing plus five external tools (`powershell`,
//! `lipo`, `codesign`, `hdiutil`, `tar`) that only exist on their own host; those are
//! exercised by the `release` workflow's `workflow_dispatch` dry run, which
//! builds and packages without publishing.
//!
//! Checksums, minisign signatures, the Inno Setup installer, notarization, and
//! the GitHub Release are the release workflow's job (they need CI secrets and
//! external tools); this task produces the staging trees and the two container
//! artifacts, and is runnable locally for parity.

use std::path::{Path, PathBuf};
use std::process::Command;

use self::verified::Verified;
use crate::args::value;
use crate::bundle::{self, BundleInputs};
use crate::macho;
use crate::repo_root;
use crate::version::Version;

/// The files copied alongside the binaries into every artifact — at the archive
/// root on Windows, in `Contents/Resources` inside the macOS bundle.
const EXTRA_FILES: [&str; 3] = ["LICENSE-MIT", "LICENSE-APACHE", "README.md"];

/// The binaries Duja ships.
const BINARIES: [&str; 2] = [bundle::MAIN_EXECUTABLE, bundle::HELPER_EXECUTABLE];

/// The XDG menu entry shipped in the Linux tarball, relative to the repo root.
const DESKTOP_ENTRY: &str = "packaging/linux/duja.desktop";

/// The icon that entry's `Icon=duja` resolves to, once the user has installed it
/// into an icon theme.
///
/// Taken from `docs/` rather than copied into `packaging/`, so the brand mark is
/// one file. A second copy would be a second thing to update, and the failure
/// mode of missing it — a stale icon in the tarball while the README and the
/// social preview moved on — is silent.
const ICON_SOURCE: &str = "docs/images/duja-mark.png";

/// What [`ICON_SOURCE`] is called inside the archive: the `.desktop` file's
/// `Icon=` name plus an extension, which is what an icon theme expects.
const ICON_NAME: &str = "duja.png";

/// The two architectures fused into the universal macOS binary, as (Rust target
/// triple, the name `lipo` and the Mach-O header use).
///
/// Both halves are load-bearing: the triple locates the build output, and the
/// arch name is what [`Verified::checked`] reads back out of the result. The order
/// here is arm64-first because that is the majority of shipping Macs, not
/// because it survives: `lipo` sorts slices by CPU type, so `lipo -archs` on the
/// result prints `x86_64 arm64` whatever order it was given.
const MAC_ARCHES: [(&str, &str); 2] = [
    ("aarch64-apple-darwin", "arm64"),
    ("x86_64-apple-darwin", "x86_64"),
];

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
    /// The portable tarball. No installer and no bundle - see
    /// `packaging/linux/README.md` for why there is no `AppImage` or `.deb` yet.
    Linux,
}

impl Target {
    /// The target this host packages for by default.
    ///
    /// Still a total function over the hosts that *have* packaging rather than a
    /// `windows`-else fallback — and the arm that used to prove the point is gone.
    /// This returned an error on Linux until P7 wave 6, because silently staging a
    /// Windows tree there would have produced an artifact nobody asked for. All
    /// three hosts answer now; the fallback stays for the fourth.
    ///
    /// # Errors
    /// Returns a message naming the `--target` escape hatch on a host with no
    /// packaging of its own.
    fn host() -> Result<Target, String> {
        if cfg!(target_os = "windows") {
            Ok(Target::Windows)
        } else if cfg!(target_os = "macos") {
            Ok(Target::Macos)
        } else if cfg!(target_os = "linux") {
            Ok(Target::Linux)
        } else {
            Err("`dist` has no packaging for this host; \
                 pass `--target windows|macos|linux` to stage one explicitly"
                .to_owned())
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
            "linux" => Ok(Target::Linux),
            other => Err(format!(
                "unknown `--target` `{other}` (expected `windows`, `macos` or `linux`)"
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
pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let parsed = Invocation::parse(args)?;
    let root = repo_root()?;
    let dist = root.join("target").join("dist");
    match parsed.target {
        Target::Windows => windows(&root, &dist, &parsed.version),
        Target::Macos => macos(&root, &dist, &parsed.version, &parsed.identity),
        Target::Linux => linux(&root, &dist, &parsed.version),
    }
}

/// One resolved `dist` command line.
///
/// Parsing is separated from doing so the argument rules — the host default,
/// the flag-shaped-value rejection, and refusing an option the chosen target
/// cannot honour — are testable without staging anything.
#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    /// The validated release version.
    version: Version,
    /// The platform to package for.
    target: Target,
    /// The `codesign` identity; [`AD_HOC`] unless `--sign` said otherwise.
    identity: String,
}

impl Invocation {
    /// Parse the arguments following `dist`.
    ///
    /// # Errors
    /// A human-readable message for a missing or malformed `--version`, an
    /// unknown flag, a flag whose value is missing or is itself a flag, a host
    /// with no packaging and no explicit `--target`, or `--sign` on a target
    /// that has nothing to sign.
    fn parse<I: Iterator<Item = String>>(mut args: I) -> Result<Invocation, String> {
        let mut version: Option<Version> = None;
        let mut target: Option<Target> = None;
        let mut identity: Option<String> = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--version" => version = Some(Version::parse(&value(&mut args, "--version")?)?),
                "--target" => target = Some(Target::parse(&value(&mut args, "--target")?)?),
                "--sign" => identity = Some(value(&mut args, "--sign")?),
                other => return Err(format!("unknown `dist` argument `{other}`")),
            }
        }
        let version = version.ok_or("usage: cargo xtask dist --version X.Y.Z")?;
        let target = match target {
            Some(explicit) => explicit,
            None => Target::host()?,
        };
        // Accepting an argument and then ignoring it is a lie about what ran:
        // there is nothing for `--sign` to sign in a Windows zip.
        if identity.is_some() && target != Target::Macos {
            return Err("`--sign` applies to the macOS target only".to_owned());
        }
        Ok(Invocation {
            version,
            target,
            identity: identity.unwrap_or_else(|| AD_HOC.to_owned()),
        })
    }
}

/// Stage `duja-<ver>-windows-x64/` and zip it.
fn windows(root: &Path, dist: &Path, version: &Version) -> Result<(), String> {
    let release = root.join("target").join("release");
    let stage_name = bundle::windows_stage_dir_name(version);
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

    let zip = dist.join(bundle::windows_zip_name(version));
    if zip.exists() {
        std::fs::remove_file(&zip).map_err(|e| format!("clearing {}: {e}", zip.display()))?;
    }
    compress(&stage, &zip)?;

    println!("staged  {}", stage.display());
    println!("archive {}", zip.display());
    Ok(())
}

/// Stage `duja-<ver>-linux-x64/` and tar it.
///
/// The Windows portable zip's twin, and deliberately not more than that. There is
/// no installer, no bundle and no dependency metadata — `packaging/linux/README.md`
/// carries the argument, and the short version is that a `.deb` or an `AppImage`
/// makes a **claim** a tarball does not, and neither claim can be checked from a
/// machine that has never run this binary.
///
/// # Why this refuses to run off a unix host
///
/// A tarball carries a permission bit and a zip does not. `bundle::assemble`'s
/// `make_executable` is `cfg(unix)`, and every other way to set the bit —
/// `PermissionsExt`, `tar --mode` — is either unix-only or a GNU extension, so a
/// tree staged from Windows would produce an archive whose `duja` extracts as
/// `rw-r--r--`. That is the worst shape a packaging bug can take: it stages
/// cleanly, tars cleanly, checksums cleanly, uploads cleanly, and fails on the
/// user's machine with `Permission denied`. `--target macos` is confined to a Mac
/// by needing `lipo`; this one has to say so itself.
fn linux(root: &Path, dist: &Path, version: &Version) -> Result<(), String> {
    if !cfg!(unix) {
        return Err(
            "`--target linux` needs a unix host: the tarball has to carry the              executable bit, and this host cannot set one"
                .to_owned(),
        );
    }

    let release = root.join("target").join("release");
    let stage_name = bundle::linux_stage_dir_name(version);
    let stage = dist.join(&stage_name);
    fresh_dir(&stage)?;

    // No `EXE_SUFFIX`: that constant is the *host's*, and the answer wanted here
    // is the target's, which is the empty string. They agree on a Linux host and
    // the distinction is what keeps this readable next to the Windows arm.
    for bin in BINARIES {
        let src = release.join(bin);
        if !src.exists() {
            return Err(format!(
                "missing {} — run `cargo build --release -p duja-app -p dujactl` first",
                src.display()
            ));
        }
        copy_into(&src, &stage)?;
        bundle::make_executable(&stage.join(bin))?;
    }
    // Licences + README, as in every other artifact.
    for name in EXTRA_FILES {
        copy_into(&root.join(name), &stage)?;
    }
    // The menu entry, and the icon its `Icon=duja` resolves to once installed.
    copy_into(&root.join(DESKTOP_ENTRY), &stage)?;
    bundle::copy(&root.join(ICON_SOURCE), &stage.join(ICON_NAME))?;

    let tarball = dist.join(bundle::linux_tarball_name(version));
    if tarball.exists() {
        std::fs::remove_file(&tarball)
            .map_err(|e| format!("clearing {}: {e}", tarball.display()))?;
    }
    archive(dist, &stage_name, &tarball)?;

    println!("staged  {}", stage.display());
    println!("archive {}", tarball.display());
    Ok(())
}

/// Fuse, assemble, sign, and image the macOS artifact.
///
/// The order is not arrangeable: `lipo` rewrites the Mach-O and the bundle seal
/// covers the `Info.plist` and every file under `Contents`, so **signing is the
/// last mutation of the bundle** — after the universal binaries exist and after
/// the bundle is complete. Nested code (`dujactl`) is signed before the bundle
/// that encloses it, because sealing the bundle records the signatures it finds
/// inside. The steps that follow deliberately touch only the *staging directory*
/// around the sealed `.app`: the `/Applications` symlink is its sibling, and
/// `hdiutil` only reads.
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

    // 4. Read the fused binaries back: both architectures present, and both
    //    built for the release the plist is about to advertise. Before signing,
    //    so a wrong artifact is never sealed and never reaches a disk image.
    let verified = Verified::checked(&app)?;

    // 5. Seal it: nested code first, then the bundle, then verify the seal.
    seal(&verified, identity)?;

    // 6. The drag-to-install target, then the image itself.
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
    for (triple, _) in MAC_ARCHES {
        let path = target_dir.join(triple).join("release").join(bin);
        if !path.exists() {
            return Err(format!(
                "missing {} — run `MACOSX_DEPLOYMENT_TARGET={} cargo build --release \
                 --target {triple} -p duja-app -p dujactl` first",
                path.display(),
                bundle::MIN_MACOS,
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

/// The check that stands between `lipo` and `codesign`, behind a token the rest
/// of this module cannot forge.
mod verified {
    use super::{MAC_ARCHES, bundle, macho};

    /// A bundle whose binaries have been read back and agree with its
    /// `Info.plist`.
    ///
    /// The field is private to this module and [`Verified::checked`] is the only
    /// constructor, so [`super::seal`] — which takes nothing else — **cannot be
    /// reached without the check having run**; forging one from `dist` is
    /// `error[E0451]`. That is deliberate rather than decorative: `dist::macos`
    /// cannot execute on the lanes this code is written on, so "a test proves
    /// the call site is still there" is not available.
    ///
    /// Precisely what it does and does not guarantee: deleting the
    /// `Verified::checked` call *and* the `seal` call together still compiles,
    /// and would produce an unverified, unsigned image. What stops that is not
    /// this type but `dead_code` under CI's `clippy -D warnings`. The type
    /// closes the case that matters — a signed artifact that was never
    /// checked — not every case.
    pub(super) struct Verified<'a> {
        /// The bundle this token vouches for.
        app: &'a bundle::Bundle,
    }

    impl<'a> Verified<'a> {
        /// Read `app`'s binaries and refuse anything that is not what its
        /// `Info.plist` claims.
        ///
        /// Two failures this catches, both silent and both shipping-grade:
        ///
        /// * **A missing slice.** `lipo` fuses however many inputs it is given;
        ///   a binary without its `x86_64` half runs on exactly the Macs the
        ///   maintainer tested on and fails on the other half of the world.
        /// * **The wrong deployment target.** `LSMinimumSystemVersion` is a
        ///   claim about the *binary*, and only `MACOSX_DEPLOYMENT_TARGET` at
        ///   build time honours it. Forgetting it yields slices built for
        ///   whatever rustc defaults to, inside a bundle advertising
        ///   [`bundle::MIN_MACOS`] — an app that launches fine on the machine
        ///   that built it. No amount of checking the plist can see this, which
        ///   is why this reads the Mach-O instead of comparing another pair of
        ///   strings.
        ///
        /// Runs identically locally and in CI, because [`macho`] parses the
        /// header rather than shelling out to `otool` — which exists on neither
        /// of the lanes this code is developed on.
        ///
        /// # Errors
        /// A message naming the binary, the architecture, and the `cargo build`
        /// that would fix it.
        pub(super) fn checked(app: &'a bundle::Bundle) -> Result<Self, String> {
            for name in [bundle::MAIN_EXECUTABLE, bundle::HELPER_EXECUTABLE] {
                let path = app.executable(name);
                let bytes =
                    std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
                let found =
                    macho::slices(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                let present: Vec<&str> = found.iter().map(|s| s.arch.as_str()).collect();
                for (triple, arch) in MAC_ARCHES {
                    let slice = found.iter().find(|s| s.arch == arch).ok_or_else(|| {
                        format!(
                            "{} has no {arch} slice (it has: {}) — build {triple} too",
                            path.display(),
                            present.join(", "),
                        )
                    })?;
                    if slice.min_os.as_deref() != Some(bundle::MIN_MACOS) {
                        return Err(format!(
                            "{} {arch} slice targets macOS {}, but the bundle advertises {} — \
                             rebuild it with MACOSX_DEPLOYMENT_TARGET={}",
                            path.display(),
                            slice.min_os.as_deref().unwrap_or("<no recorded minimum>"),
                            bundle::MIN_MACOS,
                            bundle::MIN_MACOS,
                        ));
                    }
                }
            }
            Ok(Verified { app })
        }

        /// The bundle this token vouches for.
        pub(super) fn bundle(&self) -> &bundle::Bundle {
            self.app
        }
    }
}

/// Seal the bundle: nested code first, then the bundle itself, then read the
/// seal back.
///
/// Inside-out order is required, not stylistic: sealing a bundle records the
/// signatures of the code it finds nested inside, so a `dujactl` signed
/// afterwards would invalidate the enclosing seal.
fn seal(verified: &Verified<'_>, identity: &str) -> Result<(), String> {
    let app = verified.bundle();
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
    verify_signature(app.root())
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
/// Nothing ever writes to a read-only compressed image, so the filesystem inside
/// it is an implementation detail; `HFS+` is asked for **explicitly**, for
/// breadth rather than for anything APFS lacks. Without `-fs`, `-srcfolder`
/// inherits the source volume's filesystem (`man hdiutil` COMPATIBILITY, macOS
/// 11.0: the new APFS default "does not apply to images created with
/// `-srcfolder`" — the `-fs` paragraph's flat "The default file system is APFS"
/// is the one that misleads here), which would make the
/// output depend on whatever the build machine happens to use — an APFS image
/// from one runner and an HFS+ image from another, for the same release. Naming
/// it removes that. (The reason usually given for HFS+ — that APFS images are
/// unreadable before macOS 10.13 — does *not* apply here: Duja's own floor is
/// already higher than that.) `UDZO` is the zlib-compressed read-only format
/// every macOS reads.
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

/// Tar+gzip `stage_name` (relative to `dir`, so the folder appears at the archive
/// root) into `tarball`.
///
/// `-C dir` rather than an absolute path, for the same reason [`compress`] zips
/// the folder rather than its contents: a user extracting this must get one
/// directory, not two binaries and four loose files in whatever they were sitting
/// in. An absolute path would additionally bake the build machine's directory
/// layout into every member name.
///
/// Deliberately **not** reproducible. GNU tar can be made so — `--sort=name`,
/// `--mtime`, `--owner=0 --group=0 --numeric-owner` — and those flags are a GNU
/// extension that a busybox or bsdtar host rejects outright. The release publishes
/// one checksum for one build and never rebuilds it, so byte-identical rebuilds
/// buy nothing here that would justify a tar this can fail on. The Windows zip is
/// not reproducible either, for want of even the option.
fn archive(dir: &Path, stage_name: &str, tarball: &Path) -> Result<(), String> {
    let mut cmd = Command::new("tar");
    cmd.arg("-czf")
        .arg(tarball)
        .arg("-C")
        .arg(dir)
        .arg(stage_name);
    tool(cmd, "tar")
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
    tool(cmd, "powershell")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a command line written the way it would be typed.
    fn args(argv: &[&str]) -> Result<Invocation, String> {
        Invocation::parse(argv.iter().map(|s| (*s).to_owned()))
    }

    /// The *choice* of target, not a literal. Packaging for the wrong platform
    /// is silent — a Windows zip staged on a Mac looks like a successful run —
    /// so the mapping is pinned per lane rather than left at the call site.
    /// Reds on any lane whose arm is swapped for another, and on all three if the
    /// fallback is widened to cover a host that has packaging of its own.
    ///
    /// The final `else` is now unreachable on every lane CI runs, which is worth
    /// stating rather than deleting: it is the answer for a fourth host, and the
    /// assertion under it is what stops that host being quietly given one of the
    /// three existing artifacts. Until P7 wave 6 this branch was Linux's.
    #[test]
    fn the_default_target_is_this_host_rather_than_a_literal() {
        let host = Target::host();
        if cfg!(target_os = "windows") {
            assert_eq!(host, Ok(Target::Windows));
        } else if cfg!(target_os = "macos") {
            assert_eq!(host, Ok(Target::Macos));
        } else if cfg!(target_os = "linux") {
            assert_eq!(host, Ok(Target::Linux));
        } else {
            let err = host.expect_err("no packaging on this host");
            assert!(err.contains("--target"), "{err}");
        }
    }

    #[test]
    fn an_explicit_target_overrides_the_host() {
        assert_eq!(Target::parse("windows"), Ok(Target::Windows));
        assert_eq!(Target::parse("macos"), Ok(Target::Macos));
        assert_eq!(Target::parse("linux"), Ok(Target::Linux));
    }

    /// The rejection, kept as its own test now that all three accepted values are
    /// above.
    ///
    /// The message has to *list* what is accepted rather than say the value is
    /// wrong: `--target` is typed by a maintainer cutting a release, from memory,
    /// and "unknown target `Linux`" with no list is a second guess rather than an
    /// answer. It named two values for as long as there were two, which is exactly
    /// the kind of string that goes stale silently — so it is asserted, not just
    /// written.
    #[test]
    fn an_unknown_target_is_refused_with_the_list_of_real_ones() {
        let err = Target::parse("freebsd").expect_err("not a target");
        for accepted in ["windows", "macos", "linux"] {
            assert!(
                err.contains(accepted),
                "the refusal must name `{accepted}`: {err}"
            );
        }
        // Case matters: these are matched literally, so a capitalised value is a
        // real mistake to make and must not silently mean something else.
        assert!(Target::parse("Linux").is_err());
    }

    /// The two ways `--target linux` refuses before it stages anything, which are
    /// the only two a maintainer meets in practice.
    ///
    /// Lane-dependent by construction, and that is the point rather than a
    /// weakness: the host check *is* the behaviour, so a single-lane test could
    /// only ever pin half of it. On Windows this asserts the explicit refusal; on
    /// unix it asserts the missing-input message, which is the same path with the
    /// guard passed.
    #[test]
    fn staging_a_linux_tarball_refuses_early_and_says_which_way() {
        let dir = std::env::temp_dir().join(format!("duja-linux-dist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let version = Version::parse("9.9.9").expect("version");

        let err = linux(&dir, &dir.join("dist"), &version).expect_err("nothing is built here");

        if cfg!(unix) {
            // The guard passed, so the first real step ran and found no binary.
            // The message has to name the command rather than the file alone:
            // "missing target/release/duja" is a fact, and `cargo build --release`
            // is the answer.
            assert!(
                err.contains("cargo build --release"),
                "a missing binary must name the command that produces it: {err}"
            );
        } else {
            // The permission bit is unsettable here, so this must not stage at
            // all -- a tarball whose `duja` extracts as rw-r--r-- passes every
            // check in the release pipeline and fails on the user's machine.
            assert!(
                err.contains("unix host"),
                "a non-unix host must refuse rather than stage: {err}"
            );
            assert!(
                !dir.join("dist").exists(),
                "the refusal must come before anything is created"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Assemble a bundle whose two binaries are the given synthetic universal
    /// files, and run the packaging check over it.
    fn checked(tag: &str, binary: &[(&str, (u32, u32, u32))]) -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("duja-verify-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let bytes = macho::fixtures::universal(binary);
        let main = dir.join("duja-fat");
        let helper = dir.join("dujactl-fat");
        std::fs::write(&main, &bytes).expect("main");
        std::fs::write(&helper, &bytes).expect("helper");
        let app = bundle::assemble(
            &dir,
            &Version::parse("9.9.9").expect("version"),
            &BundleInputs {
                main: &main,
                helper: &helper,
                resources: &[],
            },
        )
        .expect("assemble");
        let out = Verified::checked(&app).map(|_| ());
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// What `lipo` produces when both builds ran with the right deployment
    /// target — the only artifact that may be signed.
    #[test]
    fn a_universal_binary_at_the_advertised_floor_is_accepted() {
        assert_eq!(
            checked("good", &[("arm64", (11, 0, 0)), ("x86_64", (11, 0, 0))]),
            Ok(())
        );
    }

    /// Forgetting `MACOSX_DEPLOYMENT_TARGET` on one build. The bundle would
    /// still assemble, still sign, still mount, and still claim 11.0 — the whole
    /// reason the check reads the Mach-O rather than comparing two strings.
    #[test]
    fn a_slice_below_the_advertised_floor_is_refused() {
        let err = checked("floor", &[("arm64", (11, 0, 0)), ("x86_64", (10, 12, 0))])
            .expect_err("wrong floor");
        assert!(err.contains("targets macOS 10.12"), "{err}");
        assert!(err.contains("MACOSX_DEPLOYMENT_TARGET"), "{err}");
    }

    /// A "universal" binary with one slice runs on exactly the Macs the person
    /// who built it owns.
    #[test]
    fn a_single_arch_binary_is_refused() {
        let err = checked("single", &[("arm64", (11, 0, 0))]).expect_err("one slice");
        assert!(err.contains("no x86_64 slice"), "{err}");
        assert!(err.contains("x86_64-apple-darwin"), "{err}");
    }

    /// The arch list lives in three places — here, the workflow's `rustup target
    /// add`, and its two `cargo build --target` lines — and only the last of
    /// those decides what is actually compiled. Dropping one `cargo build` line
    /// is the realistic drift, and it yields a single-arch "universal" binary.
    /// `Verified::checked` catches that at package time, but only once somebody
    /// runs a release; reading the workflow catches it on every lane.
    #[test]
    fn the_workflow_builds_every_architecture_this_fuses() {
        let workflow = crate::read_repo_file(&[".github", "workflows", "release.yml"]);
        for (triple, _) in MAC_ARCHES {
            assert!(
                workflow.contains(&format!("--target {triple}")),
                "release.yml never builds {triple}, so `lipo` would fuse one slice"
            );
        }
    }

    /// Every flag spelling is a legal version string, so a missing value used to
    /// be swallowed as the version and surface as a confusing complaint about
    /// the *next* argument.
    #[test]
    fn a_flag_shaped_value_is_rejected_rather_than_taken_as_the_version() {
        let err = args(&["--version", "--target", "macos"]).expect_err("flag as value");
        assert!(err.contains("got the flag `--target`"), "{err}");
    }

    /// `--sign` has no meaning for a zip. Accepting it and doing nothing would
    /// report success for a signing run that never signed.
    #[test]
    fn sign_is_refused_where_there_is_nothing_to_sign() {
        let err = args(&["--version", "1.0.0", "--target", "windows", "--sign", "me"])
            .expect_err("--sign on windows");
        assert!(err.contains("macOS target only"), "{err}");
    }

    #[test]
    fn the_signing_identity_defaults_to_ad_hoc() {
        let parsed = args(&["--version", "1.0.0", "--target", "macos"]).expect("parse");
        assert_eq!(parsed.identity, "-");
        let parsed = args(&[
            "--version",
            "1.0.0",
            "--target",
            "macos",
            "--sign",
            "Developer ID",
        ])
        .expect("parse");
        assert_eq!(parsed.identity, "Developer ID");
    }

    #[test]
    fn a_run_without_a_version_says_how_to_invoke_it() {
        let err = args(&["--target", "macos"]).expect_err("no version");
        assert!(err.contains("usage:"), "{err}");
        let err = args(&["--frobnicate"]).expect_err("unknown flag");
        assert!(err.contains("unknown `dist` argument"), "{err}");
    }
}

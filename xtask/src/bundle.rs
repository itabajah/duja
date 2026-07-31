//! The macOS application bundle, described as data.
//!
//! A `.app` is a directory with a fixed shape and one required document — the
//! `Info.plist` that tells Launch Services what the thing *is*. Composing that
//! document and laying out that directory are pure operations: no macOS API is
//! involved, only strings and `std::fs`. They live here, uncompiled-out on every
//! host, so the bundle's contract is unit-tested on the Windows and Linux lanes
//! too — exactly the split `duja-platform`'s [`autostart::plist`] module makes
//! for the `LaunchAgent` document.
//!
//! Only the three steps that genuinely need macOS tooling stay in [`dist`]:
//! `lipo` (fuse the two thin binaries), `codesign` (seal the bundle), and
//! `hdiutil` (wrap it in a disk image). Those are `Command` invocations, not
//! `cfg` blocks, so this crate still *compiles* them into every lane's clippy
//! run.
//!
//! # What the `Info.plist` commits to
//!
//! - **`LSUIElement`** is the load-bearing key. It makes Duja an *agent*: no
//!   Dock tile, no menu bar of its own, no app switcher entry — a menu-bar-only
//!   process, which is what a tray app is on macOS. The app also calls
//!   `become_accessory_app` at startup (`duja-app` `tray.rs`) for the unbundled
//!   case; once bundled, this key is what actually decides, and winit stops
//!   overriding the activation policy.
//! - **`CFBundleIdentifier`** must equal the `launchd` job label
//!   `duja-platform` registers for launch-at-login, or the same program would
//!   carry two identities — pinned by a test below that reads that constant out
//!   of the other crate.
//! - **`LSMinimumSystemVersion`** must equal the deployment target the release
//!   workflow builds both slices against, or the bundle advertises a floor the
//!   binary was not compiled for. Also pinned by a test, against the workflow.
//!
//! # Not shipped yet: an icon
//!
//! There is no `CFBundleIconFile`, so Finder, the Login Items list and Force
//! Quit show the generic application icon. Duja's icon art is *drawn in code*
//! (`duja-ui` `icon.rs`) and no raster asset exists in the tree, so producing an
//! `.icns` needs either a PNG encoder in this deliberately dependency-free crate
//! or a macOS-only `sips`/`iconutil` pipeline that cannot be verified without a
//! Mac. Tracked in `docs/debt.md`. The bundle is complete and launchable without
//! it — present but plain, never absent.
//!
//! [`autostart::plist`]: https://github.com/itabajah/duja/blob/main/crates/duja-platform/src/autostart/plist.rs
//! [`dist`]: crate::dist

use std::path::{Path, PathBuf};

use crate::version::Version;

/// The user-visible application name.
pub(crate) const APP_NAME: &str = "Duja";

/// The bundle directory's name (`<APP_NAME>.app`).
pub(crate) const APP_DIR_NAME: &str = "Duja.app";

/// `CFBundleExecutable` — the binary Launch Services starts, and the file name
/// it must have inside `Contents/MacOS`.
pub(crate) const MAIN_EXECUTABLE: &str = "duja";

/// The bundled CLI. A *nested* Mach-O, not the bundle's main executable, so it
/// is signed on its own before the enclosing bundle is sealed.
pub(crate) const HELPER_EXECUTABLE: &str = "dujactl";

/// `CFBundleIdentifier` — the reverse-DNS app identity. Byte-identical to the
/// `launchd` label in `duja-platform`'s `autostart::plist`, which the test
/// [`the_bundle_identifier_is_the_launch_agent_label`] enforces.
///
/// [`the_bundle_identifier_is_the_launch_agent_label`]: self
pub(crate) const BUNDLE_ID: &str = "io.github.itabajah.duja";

/// `LSMinimumSystemVersion` — the oldest macOS this bundle claims to run on.
///
/// **A support decision, not a technical necessity.** A universal binary records
/// a deployment target *per slice*, and Launch Services gates launch on this key
/// alone, so a bundle advertising 10.13 with an `x86_64` slice built for 10.13
/// would run on an Intel Mac at 10.13 — the `arm64` slice's own 11.0 floor is
/// never consulted on hardware that could not use it anyway. That is the ordinary
/// arrangement; an earlier version of this comment claimed it was dishonest,
/// which was simply wrong.
///
/// 11.0 is chosen because it is the lowest floor **both** slices can be built at
/// from one `MACOSX_DEPLOYMENT_TARGET`, and going below it means asserting
/// support for releases no lane tests and nobody has run Duja on. The cost is
/// bounded and specific: Big Sur covers Intel Macs from roughly 2013–2014
/// onwards, so what is excluded is hardware a decade older than that — not the
/// Intel population whose DDC confirmations `docs/debt.md` is waiting on.
/// Lowering it needs a per-arch deployment target *and* someone with the
/// hardware; that is recorded as debt rather than guessed at.
///
/// The claim is enforced on the artifact, not on paper: the release workflow
/// builds both slices with `MACOSX_DEPLOYMENT_TARGET` set to this value, and
/// `dist`'s `Verified::checked` reads the `minos` back out of the fused binary
/// and refuses to sign a bundle whose slices disagree with it.
pub(crate) const MIN_MACOS: &str = "11.0";

/// The four-byte legacy type/creator record in `Contents/PkgInfo`.
///
/// Modern Launch Services reads `CFBundlePackageType` from the plist and ignores
/// this file; it is written anyway because some older tooling (and a few
/// archivers) still look for it, and it costs eight bytes.
const PKG_INFO: &str = "APPL????";

/// The staged directory name for the macOS artifact — also the DMG's stem, and
/// the folder `hdiutil` turns into the disk image's root.
pub(crate) fn stage_dir_name(version: &Version) -> String {
    format!("duja-{version}-macos-universal")
}

/// The disk image's file name.
pub(crate) fn dmg_file_name(version: &Version) -> String {
    format!("{}.dmg", stage_dir_name(version))
}

/// The volume name the mounted disk image shows in Finder's sidebar.
pub(crate) fn volume_name(version: &Version) -> String {
    format!("{APP_NAME} {version}")
}

/// Compose the bundle's `Info.plist`.
///
/// `version` fills both version keys: `CFBundleShortVersionString` (the
/// human-facing "0.2.0") and `CFBundleVersion` (the build identifier). Duja has
/// no separate build counter, so they are the same string — which is also what
/// makes a pre-release tag like `0.2.0-beta.1` land in
/// `CFBundleShortVersionString`. That is not the strict dotted-integer form the
/// App Store validates on submission; Duja does not ship through the App Store,
/// and Launch Services does not validate this key at launch. Whether Apple's
/// notary service would object is untested — nothing here has ever been
/// notarized — so if a pre-release tag is ever notarized, check it.
///
/// The text is emitted rather than escaped: [`Version`] has already rejected
/// every character that means something to XML, and every other value here is a
/// compile-time constant from this module.
pub(crate) fn info_plist(version: &Version) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>CFBundleDevelopmentRegion</key>\n\
         \t<string>en</string>\n\
         \t<key>CFBundleDisplayName</key>\n\
         \t<string>{APP_NAME}</string>\n\
         \t<key>CFBundleExecutable</key>\n\
         \t<string>{MAIN_EXECUTABLE}</string>\n\
         \t<key>CFBundleIdentifier</key>\n\
         \t<string>{BUNDLE_ID}</string>\n\
         \t<key>CFBundleInfoDictionaryVersion</key>\n\
         \t<string>6.0</string>\n\
         \t<key>CFBundleName</key>\n\
         \t<string>{APP_NAME}</string>\n\
         \t<key>CFBundlePackageType</key>\n\
         \t<string>APPL</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\
         \t<string>{version}</string>\n\
         \t<key>CFBundleVersion</key>\n\
         \t<string>{version}</string>\n\
         \t<key>LSApplicationCategoryType</key>\n\
         \t<string>public.app-category.utilities</string>\n\
         \t<key>LSMinimumSystemVersion</key>\n\
         \t<string>{MIN_MACOS}</string>\n\
         \t<key>LSUIElement</key>\n\
         \t<true/>\n\
         \t<key>NSHighResolutionCapable</key>\n\
         \t<true/>\n\
         \t<key>NSHumanReadableCopyright</key>\n\
         \t<string>Duja contributors. Licensed under MIT OR Apache-2.0.</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// The already-built inputs [`assemble`] copies into a bundle.
pub(crate) struct BundleInputs<'a> {
    /// The universal `duja` binary — becomes `Contents/MacOS/duja`.
    pub(crate) main: &'a Path,
    /// The universal `dujactl` binary — becomes `Contents/MacOS/dujactl`.
    pub(crate) helper: &'a Path,
    /// Files copied verbatim into `Contents/Resources` (licences, README).
    pub(crate) resources: &'a [PathBuf],
}

/// A `.app` directory, addressed by the paths macOS defines for it.
#[derive(Debug)]
pub(crate) struct Bundle {
    /// The `…/Duja.app` directory itself.
    root: PathBuf,
}

impl Bundle {
    /// The bundle that lives (or will live) directly inside `dir`.
    pub(crate) fn inside(dir: &Path) -> Self {
        Bundle {
            root: dir.join(APP_DIR_NAME),
        }
    }

    /// The `Duja.app` directory — what `codesign` seals and Finder shows.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// `Duja.app/Contents`.
    pub(crate) fn contents(&self) -> PathBuf {
        self.root.join("Contents")
    }

    /// `Duja.app/Contents/MacOS` — every executable in the bundle.
    pub(crate) fn macos(&self) -> PathBuf {
        self.contents().join("MacOS")
    }

    /// `Duja.app/Contents/Resources` — the non-code payload.
    pub(crate) fn resources(&self) -> PathBuf {
        self.contents().join("Resources")
    }

    /// `Duja.app/Contents/Info.plist`.
    pub(crate) fn info_plist_path(&self) -> PathBuf {
        self.contents().join("Info.plist")
    }

    /// The path an executable named `name` occupies inside the bundle.
    pub(crate) fn executable(&self, name: &str) -> PathBuf {
        self.macos().join(name)
    }
}

/// Build `dir/Duja.app` from `inputs`: the directory skeleton, the `Info.plist`,
/// `PkgInfo`, both executables, and the resource files.
///
/// Pure filesystem work — it runs, and is tested, on every host. Signing and the
/// disk image are the caller's job and come *after* this, because the bundle
/// seal covers the `Info.plist` and everything under `Contents`.
///
/// # Errors
/// Returns a human-readable message naming the path that failed if a directory
/// cannot be created, an input is missing, or a copy/write fails.
pub(crate) fn assemble(
    dir: &Path,
    version: &Version,
    inputs: &BundleInputs<'_>,
) -> Result<Bundle, String> {
    let bundle = Bundle::inside(dir);
    for sub in [bundle.macos(), bundle.resources()] {
        std::fs::create_dir_all(&sub).map_err(|e| format!("creating {}: {e}", sub.display()))?;
    }

    write(&bundle.info_plist_path(), &info_plist(version))?;
    write(&bundle.contents().join("PkgInfo"), PKG_INFO)?;

    for (src, name) in [
        (inputs.main, MAIN_EXECUTABLE),
        (inputs.helper, HELPER_EXECUTABLE),
    ] {
        let dest = bundle.executable(name);
        copy(src, &dest)?;
        make_executable(&dest)?;
    }
    let resources = bundle.resources();
    for src in inputs.resources {
        let name = src
            .file_name()
            .ok_or_else(|| format!("{} has no file name", src.display()))?;
        copy(src, &resources.join(name))?;
    }
    Ok(bundle)
}

/// Write `contents` to `path`, naming the path on failure.
fn write(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Copy `src` to `dest`, reporting a missing source as its own message rather
/// than as an opaque `NotFound`.
fn copy(src: &Path, dest: &Path) -> Result<(), String> {
    if !src.exists() {
        return Err(format!("missing {}", src.display()));
    }
    std::fs::copy(src, dest)
        .map(|_| ())
        .map_err(|e| format!("copying {} to {}: {e}", src.display(), dest.display()))
}

/// Ensure a staged executable carries the execute bit.
///
/// `std::fs::copy` already preserves the source's mode, so on the normal path
/// (a `lipo` output) this is a no-op. It is here because the failure it prevents
/// is total — a `Contents/MacOS/duja` without `+x` makes the whole bundle
/// refuse to launch — and because a future input that arrives via an artifact
/// download or a `write` would not carry the bit at all. The `cfg` hides no
/// decision: `PermissionsExt` simply does not exist off unix, and neither does
/// the concept.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("making {} executable: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The raw value element that follows `<key>{key}</key>`: the inner text for
    /// a `<string>`, or the element itself (`<true/>`) otherwise.
    fn value_of<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
        let needle = format!("<key>{key}</key>");
        let start = plist.find(&needle)?.saturating_add(needle.len());
        let rest = plist.get(start..)?.trim_start();
        if let Some(inner) = rest.strip_prefix("<string>") {
            inner.get(..inner.find("</string>")?)
        } else {
            rest.get(..rest.find('>')?.saturating_add(1))
        }
    }

    /// A unique, empty temp directory for one test.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("duja-xtask-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// A file with recognisable contents, standing in for a built binary.
    fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("stub");
        path
    }

    fn version() -> Version {
        Version::parse("9.9.9-test").expect("version")
    }

    // ---- the Info.plist -------------------------------------------------

    /// The one key that makes Duja a menu-bar app rather than a Dock app.
    /// Without it a tray-only utility shows a Dock tile and an app-switcher
    /// entry it has no window for.
    #[test]
    fn the_bundle_is_a_menu_bar_agent() {
        let plist = info_plist(&version());
        assert_eq!(value_of(&plist, "LSUIElement"), Some("<true/>"));
    }

    #[test]
    fn both_version_keys_carry_the_release_version() {
        let plist = info_plist(&version());
        assert_eq!(
            value_of(&plist, "CFBundleShortVersionString"),
            Some("9.9.9-test")
        );
        assert_eq!(value_of(&plist, "CFBundleVersion"), Some("9.9.9-test"));
    }

    #[test]
    fn the_plist_declares_an_application_at_the_advertised_floor() {
        let plist = info_plist(&version());
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(plist.trim_end().ends_with("</plist>"));
        assert_eq!(value_of(&plist, "CFBundlePackageType"), Some("APPL"));
        assert_eq!(value_of(&plist, "LSMinimumSystemVersion"), Some(MIN_MACOS));
        assert_eq!(value_of(&plist, "NSHighResolutionCapable"), Some("<true/>"));
    }

    /// Every occurrence of `pattern`\'s trailing capture in `text`, where the
    /// value runs to the next `terminator`.
    ///
    /// Collects **all** matches rather than the first: a workflow that grows a
    /// second job with its own `MACOSX_DEPLOYMENT_TARGET`, or a module that
    /// grows a second `LABEL`, must not be able to hide a drifted value behind
    /// an agreeing one.
    fn captures(text: &str, pattern: &str, terminator: char) -> Vec<String> {
        text.split(pattern)
            .skip(1)
            .filter_map(|rest| rest.split(terminator).next())
            .map(|v| v.trim().trim_matches(['"', '\'']).to_owned())
            .collect()
    }

    /// Assert every occurrence agrees with `expected`, and that there is at
    /// least one — an absent pattern means the pin stopped pinning, which is a
    /// failure, not a pass.
    fn every_occurrence_is(found: &[String], expected: &str, what: &str) {
        assert!(!found.is_empty(), "nothing to check: {what}");
        for value in found {
            assert_eq!(value, expected, "{what}");
        }
    }

    /// The identifier is not a free-form label: `duja-platform` registers the
    /// launch-at-login job under the *same* reverse-DNS string, and the release
    /// workflow asserts it on the shipped plist. Three copies, so read the other
    /// two rather than restating them, and check every occurrence in each.
    #[test]
    fn the_bundle_identifier_is_the_launch_agent_label() {
        let plist_rs =
            crate::read_repo_file(&["crates", "duja-platform", "src", "autostart", "plist.rs"]);
        every_occurrence_is(
            &captures(&plist_rs, "LABEL: &str = \"", '"'),
            BUNDLE_ID,
            "the bundle identifier and the launchd job label must be one string",
        );

        let workflow = crate::read_repo_file(&[".github", "workflows", "release.yml"]);
        every_occurrence_is(
            &captures(
                &workflow,
                "CFBundleIdentifier raw -o - \"$plist\")\" = \"",
                '"',
            ),
            BUNDLE_ID,
            "release.yml checks the shipped plist against a stale identifier",
        );
    }

    /// `LSMinimumSystemVersion` is a claim about the *binary*, and only the build
    /// honours it. The release workflow sets `MACOSX_DEPLOYMENT_TARGET` for both
    /// slices; if that value is changed or dropped, the plist would go on
    /// advertising a floor nothing compiled against. (`dist`'s `Verified::checked`
    /// enforces the same thing on the artifact — this catches the drift before a
    /// release ever runs.)
    #[test]
    fn the_deployment_target_matches_the_minimum_the_bundle_advertises() {
        let workflow = crate::read_repo_file(&[".github", "workflows", "release.yml"]);
        every_occurrence_is(
            &captures(&workflow, "MACOSX_DEPLOYMENT_TARGET:", '\n'),
            MIN_MACOS,
            "the workflow's deployment target and the plist's minimum must agree",
        );
    }

    /// `CFBundleExecutable` and the staged file names are *cargo* binary names:
    /// `dist` looks for `target/<triple>/release/<name>`, which is whatever the
    /// `[[bin]]` sections say. Renaming a binary would leave the plist naming a
    /// file that is never produced — and the layout tests could not see it,
    /// because they take both sides of the comparison from these constants.
    #[test]
    fn the_executable_names_are_the_ones_cargo_produces() {
        for (crate_dir, expected) in [
            ("duja-app", MAIN_EXECUTABLE),
            ("dujactl", HELPER_EXECUTABLE),
        ] {
            let manifest = crate::read_repo_file(&["crates", crate_dir, "Cargo.toml"]);
            let after_bin = manifest
                .split("[[bin]]")
                .nth(1)
                .unwrap_or_else(|| panic!("no [[bin]] section in crates/{crate_dir}/Cargo.toml"));
            every_occurrence_is(
                &captures(after_bin, "name = \"", '"')
                    .into_iter()
                    .take(1)
                    .collect::<Vec<_>>(),
                expected,
                "the bundle stages a binary name cargo does not produce",
            );
        }
    }

    // ---- the assembled directory ----------------------------------------

    #[test]
    fn an_assembled_bundle_has_the_layout_macos_requires() {
        let dir = temp_dir("layout");
        let main = stub(&dir, "duja-fat", "MAIN");
        let helper = stub(&dir, "dujactl-fat", "HELPER");
        let licence = stub(&dir, "LICENSE-MIT", "MIT");
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).expect("stage");

        let resources = vec![licence];
        let bundle = assemble(
            &stage,
            &version(),
            &BundleInputs {
                main: &main,
                helper: &helper,
                resources: &resources,
            },
        )
        .expect("assemble");

        assert_eq!(bundle.root(), stage.join("Duja.app"));
        assert!(bundle.info_plist_path().is_file());
        assert_eq!(
            std::fs::read_to_string(bundle.contents().join("PkgInfo")).expect("PkgInfo"),
            "APPL????"
        );
        // The payload is the built binaries, not empty placeholders.
        assert_eq!(
            std::fs::read_to_string(bundle.executable(MAIN_EXECUTABLE)).expect("main"),
            "MAIN"
        );
        assert_eq!(
            std::fs::read_to_string(bundle.executable(HELPER_EXECUTABLE)).expect("helper"),
            "HELPER"
        );
        assert_eq!(
            std::fs::read_to_string(bundle.resources().join("LICENSE-MIT")).expect("licence"),
            "MIT"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `CFBundleExecutable` is a *file name lookup*: Launch Services opens
    /// `Contents/MacOS/<that name>`. A bundle whose plist names a file the
    /// assembly never wrote is a bundle that will not start, and nothing in the
    /// plist alone would show it.
    #[test]
    fn the_main_executable_key_names_the_file_that_is_actually_copied() {
        let dir = temp_dir("exec-key");
        let main = stub(&dir, "duja-fat", "MAIN");
        let helper = stub(&dir, "dujactl-fat", "HELPER");
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).expect("stage");

        let bundle = assemble(
            &stage,
            &version(),
            &BundleInputs {
                main: &main,
                helper: &helper,
                resources: &[],
            },
        )
        .expect("assemble");

        let plist = std::fs::read_to_string(bundle.info_plist_path()).expect("plist");
        let named = value_of(&plist, "CFBundleExecutable").expect("CFBundleExecutable");
        assert!(
            bundle.macos().join(named).is_file(),
            "Contents/MacOS/{named} does not exist"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_input_names_the_file_rather_than_failing_opaquely() {
        let dir = temp_dir("missing");
        let helper = stub(&dir, "dujactl-fat", "HELPER");
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).expect("stage");

        let err = assemble(
            &stage,
            &version(),
            &BundleInputs {
                main: &dir.join("never-built"),
                helper: &helper,
                resources: &[],
            },
        )
        .expect_err("missing main");
        assert!(err.contains("never-built"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- names ------------------------------------------------------------

    #[test]
    fn the_artifact_names_carry_the_version_and_say_universal() {
        let v = Version::parse("0.2.0").expect("version");
        assert_eq!(stage_dir_name(&v), "duja-0.2.0-macos-universal");
        assert_eq!(dmg_file_name(&v), "duja-0.2.0-macos-universal.dmg");
        assert_eq!(volume_name(&v), "Duja 0.2.0");
        assert_eq!(APP_DIR_NAME, format!("{APP_NAME}.app"));
    }
}

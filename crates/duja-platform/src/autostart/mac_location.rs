//! Whether a macOS copy of Duja is somewhere a login item can point at.
//!
//! # The failure this exists to prevent
//!
//! The `LaunchAgent` plist records an absolute **path** ([`plist`](super::plist)
//! writes `ProgramArguments[0]` from `current_exe()`), and `launchd` execs that
//! path at every login. Duja ships as a disk image, which invites exactly one
//! sequence: mount the `.dmg`, double-click `Duja.app` to try it, like it, turn
//! on "start with the system", eject the image. The plist now names a path that
//! no longer exists — so the login item is permanently dead, and
//! [`is_enabled`](super::Autostart::is_enabled)'s presence policy keeps
//! reporting it as *enabled* because the file is still there. A setting that
//! says "on" and does nothing, forever, with no error: the "vanished" failure
//! the project's degrade rule forbids.
//!
//! Refusing to enable is the honest answer. The app's `apply_autostart` already
//! re-reads the real state after a failed write, so the toggle springs back
//! instead of lying.
//!
//! # Two doomed locations, not one
//!
//! The obvious one is `/Volumes/…`, where macOS mounts removable media and disk
//! images. **It is not the one the sequence above actually produces.**
//!
//! A `.dmg` downloaded by a browser carries the quarantine flag, and Launch
//! Services will not run a quarantined app in place: **App Translocation**
//! (macOS 10.12 onwards) mounts a throwaway read-only mirror and runs the app
//! from `/private/var/folders/…/AppTranslocation/<uuid>/d/Duja.app/…` instead.
//! `current_exe()` is `_NSGetExecutablePath` on Apple targets, so that is the
//! path Duja sees and the path the plist would record. That mount is torn down
//! when the app **quits** — sooner than ejecting — so the translocated case is
//! strictly worse than the `/Volumes/` one that hides it.
//!
//! Translocation is decided by the quarantine flag, not by the signature, so an
//! ad-hoc signature does not avoid it; signing the disk image would, which is on
//! the inert `MACOS_SIGN` path. Dragging the app to `/Applications` in Finder
//! clears the flag — which is exactly the remedy this module tells the user
//! about, and the reason one sentence covers both locations.
//!
//! # Why paths, and what it costs
//!
//! Asking the *mount* whether it is ephemeral would be narrower, but that needs
//! `statfs` and turns a pure predicate into an FFI call testable on one host —
//! the wrong trade for a check whose whole point is being right before anything
//! is written.
//!
//! The cost is one false positive: a user who keeps applications on a second
//! internal APFS volume (also mounted under `/Volumes/`) is told to move Duja to
//! `/Applications` when their login item would in fact have survived. That is a
//! refusal with correct advice, not a broken feature — and it is the direction
//! to be wrong in, since the alternative is a silently dead login item.
//!
//! Compiled on macOS and, under `cfg(test)`, on every host — same arrangement as
//! [`plist`](super::plist), so the rule is unit-tested on the Windows and Linux
//! lanes too.

use std::path::{Component, Path};

/// The directory macOS mounts removable volumes and disk images under.
///
/// Not *every* mount lands here — `hdiutil attach -mountpoint` puts one wherever
/// it is told, and this project's own release workflow does exactly that. It is
/// where the mounts a **user** produces by opening a disk image go, which is the
/// case that matters.
const MOUNTED_VOLUMES: &str = "/Volumes/";

/// The path component App Translocation inserts. Matched as a component rather
/// than by the `/private/var/folders/` prefix, because the enclosing temporary
/// directory's layout is an implementation detail while the marker is not.
const TRANSLOCATION: &str = "AppTranslocation";

/// Whether `exe` sits somewhere that can disappear out from under a login item:
/// a mounted volume, or an App Translocation mirror.
///
/// Path-only, so it is decided the same way on every host and needs no
/// filesystem access.
pub(crate) fn is_on_an_ephemeral_mount(exe: &Path) -> bool {
    if exe.to_string_lossy().starts_with(MOUNTED_VOLUMES) {
        return true;
    }
    exe.components().any(|component| match component {
        Component::Normal(name) => name == TRANSLOCATION,
        _ => false,
    })
}

/// What to tell the user when [`is_on_an_ephemeral_mount`] refuses.
///
/// Names the remedy rather than the rule: "drag it to Applications" is the
/// action whether the copy is on a disk image, an external drive, or a
/// translocated mirror — and in the last case dragging it in Finder is also what
/// clears the quarantine flag that caused the translocation in the first place.
pub(crate) fn ephemeral_mount_message(exe: &Path) -> String {
    format!(
        "Duja is running from a temporary location ({}) — a disk image, an \
         external volume, or the read-only copy macOS makes for a quarantined \
         app. A login item pointing there stops working as soon as that location \
         goes away. Drag Duja.app to /Applications and enable it from there.",
        exe.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// What actually happens when a user opens the downloaded `.dmg` and
    /// double-clicks the app: macOS runs a translocated mirror, not the volume.
    /// This is the headline case, and a `/Volumes/`-only check misses it — which
    /// is what the first version of this module did.
    #[test]
    fn a_translocated_copy_is_refused() {
        assert!(is_on_an_ephemeral_mount(&PathBuf::from(
            "/private/var/folders/qx/8p_1t2r54_s0/T/AppTranslocation/\
             6F5A9C21-0E3B-4E51-9A7E-3C0D1A2B4E88/d/Duja.app/Contents/MacOS/duja"
        )));
    }

    /// The same sequence when the image is signed (translocation does not
    /// apply), or when the user runs the app from an already-mounted volume.
    #[test]
    fn an_app_running_from_a_mounted_disk_image_is_refused() {
        assert!(is_on_an_ephemeral_mount(&PathBuf::from(
            "/Volumes/Duja 0.2.0/Duja.app/Contents/MacOS/duja"
        )));
    }

    /// An external drive has the same eject-and-it-is-gone property, so it gets
    /// the same answer.
    #[test]
    fn an_app_on_an_external_drive_is_refused_too() {
        assert!(is_on_an_ephemeral_mount(&PathBuf::from(
            "/Volumes/Backup SSD/Apps/Duja.app/Contents/MacOS/duja"
        )));
    }

    /// The installed location, and the places a developer copy lives, must all
    /// still be allowed — a guard that refuses the normal case would be worse
    /// than the bug it prevents. The last two are near misses on purpose: an
    /// ordinary path under the same temporary root is fine, and so is a
    /// directory that merely has the marker as a substring.
    #[test]
    fn the_installed_and_development_locations_are_allowed() {
        for path in [
            "/Applications/Duja.app/Contents/MacOS/duja",
            "/Users/someone/Applications/Duja.app/Contents/MacOS/duja",
            "/Users/someone/duja/target/release/duja",
            "/System/Volumes/Data/Applications/Duja.app/Contents/MacOS/duja",
            "/usr/local/bin/duja",
            "/private/var/folders/qx/8p_1t2r54_s0/T/duja-build/duja",
            "/Users/someone/AppTranslocationNotes/duja",
        ] {
            assert!(
                !is_on_an_ephemeral_mount(&PathBuf::from(path)),
                "{path} should be allowed"
            );
        }
    }

    /// A path that merely *mentions* the volumes directory is not on one; that
    /// check is a prefix, not a substring.
    #[test]
    fn a_path_that_only_mentions_volumes_is_allowed() {
        assert!(!is_on_an_ephemeral_mount(&PathBuf::from(
            "/Users/someone/Volumes/duja"
        )));
    }

    #[test]
    fn the_message_names_the_path_and_the_remedy() {
        let message = ephemeral_mount_message(&PathBuf::from("/Volumes/Duja 0.2.0/Duja.app"));
        assert!(
            message.contains("/Volumes/Duja 0.2.0/Duja.app"),
            "{message}"
        );
        assert!(message.contains("/Applications"), "{message}");
    }
}

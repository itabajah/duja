//! Whether a macOS copy of Duja is somewhere a login item can point at.
//!
//! # The failure this exists to prevent
//!
//! The `LaunchAgent` plist records an absolute **path** ([`plist`](super::plist)
//! writes `ProgramArguments[0]` from `current_exe()`), and `launchd` execs that
//! path at every login. Duja ships as a disk image, which invites exactly one
//! sequence: mount the `.dmg`, double-click `Duja.app` *on the mounted volume*
//! to try it, like it, turn on "start with the system", eject the image. The
//! plist now names `/Volumes/Duja 0.2.0/Duja.app/Contents/MacOS/duja`, which no
//! longer exists — so the login item is permanently dead, and
//! [`is_enabled`](super::Autostart::is_enabled)'s presence policy keeps
//! reporting it as *enabled* because the file is still there. A setting that
//! says "on" and does nothing, forever, with no error: the "vanished" failure
//! the project's degrade rule forbids.
//!
//! Refusing to enable is the honest answer. The app's `apply_autostart` already
//! re-reads the real state after a failed write, so the toggle springs back
//! instead of lying.
//!
//! # Why a path prefix, and what it costs
//!
//! Every removable volume and every mounted disk image appears under
//! `/Volumes/`; the startup disk is `/`, and its data volume is under
//! `/System/Volumes/`, so neither matches. Asking the *mount* whether it is
//! read-only would be narrower, but that needs `statfs` and turns a pure
//! predicate into an FFI call testable on one host — the wrong trade for a
//! check whose whole point is being correct before anything is written.
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

use std::path::Path;

/// The directory macOS mounts removable volumes and disk images under.
const MOUNTED_VOLUMES: &str = "/Volumes/";

/// Whether `exe` sits on a volume that can be unmounted out from under a login
/// item.
///
/// Path-only, so it is decided the same way on every host and needs no
/// filesystem access.
pub(crate) fn is_on_a_mounted_volume(exe: &Path) -> bool {
    exe.to_string_lossy().starts_with(MOUNTED_VOLUMES)
}

/// What to tell the user when [`is_on_a_mounted_volume`] refuses.
///
/// Names the remedy rather than the rule: "move it to Applications" is the
/// action, and it is the action whether the volume is a disk image or an
/// external drive.
pub(crate) fn mounted_volume_message(exe: &Path) -> String {
    format!(
        "Duja is running from a mounted volume ({}); a login item pointing there \
         would stop working as soon as it is ejected. Move Duja.app to \
         /Applications and enable it from there.",
        exe.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The exact sequence the disk image invites.
    #[test]
    fn an_app_running_from_a_mounted_disk_image_is_refused() {
        assert!(is_on_a_mounted_volume(&PathBuf::from(
            "/Volumes/Duja 0.2.0/Duja.app/Contents/MacOS/duja"
        )));
    }

    /// An external drive has the same eject-and-it-is-gone property, so it gets
    /// the same answer.
    #[test]
    fn an_app_on_an_external_drive_is_refused_too() {
        assert!(is_on_a_mounted_volume(&PathBuf::from(
            "/Volumes/Backup SSD/Apps/Duja.app/Contents/MacOS/duja"
        )));
    }

    /// The installed location, and the two places a developer copy lives, must
    /// all still be allowed — a guard that refuses the normal case would be
    /// worse than the bug it prevents.
    #[test]
    fn the_installed_and_development_locations_are_allowed() {
        for path in [
            "/Applications/Duja.app/Contents/MacOS/duja",
            "/Users/someone/Applications/Duja.app/Contents/MacOS/duja",
            "/Users/someone/duja/target/release/duja",
            "/System/Volumes/Data/Applications/Duja.app/Contents/MacOS/duja",
            "/usr/local/bin/duja",
        ] {
            assert!(
                !is_on_a_mounted_volume(&PathBuf::from(path)),
                "{path} should be allowed"
            );
        }
    }

    /// A path that merely *contains* the string is not on a mounted volume; the
    /// check is a prefix, not a substring.
    #[test]
    fn a_path_that_only_mentions_volumes_is_allowed() {
        assert!(!is_on_a_mounted_volume(&PathBuf::from(
            "/Users/someone/Volumes/duja"
        )));
    }

    #[test]
    fn the_message_names_the_path_and_the_remedy() {
        let message = mounted_volume_message(&PathBuf::from("/Volumes/Duja 0.2.0/Duja.app"));
        assert!(
            message.contains("/Volumes/Duja 0.2.0/Duja.app"),
            "{message}"
        );
        assert!(message.contains("/Applications"), "{message}");
    }
}

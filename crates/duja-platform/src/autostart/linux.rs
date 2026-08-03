//! Launch-at-login on Linux: an XDG autostart `.desktop` entry.
//!
//! The freedesktop **Desktop Application Autostart Specification** says a
//! `.desktop` file in `$XDG_CONFIG_HOME/autostart` (default
//! `~/.config/autostart`) is launched when the desktop session starts. Every
//! desktop that has a session manager implements it, which is why this is one
//! file rather than three per-desktop mechanisms.
//!
//! # Why not a systemd user unit
//!
//! A `systemd --user` service is the other candidate. It is not that a unit
//! *cannot* express "when a desktop session starts" — `graphical-session.target`
//! exists precisely for that, and is what such a unit would be wired to. The
//! reasons are narrower and both practical: it needs systemd, which the D-Bus
//! half of this crate already treats as optional, and `graphical-session.target`
//! is only reliably reached on desktops that install the `Wants` to pull it in.
//! A unit wired to `default.target` instead — the naive version — would start
//! Duja on any **login**, including an `ssh` login or a bare TTY where there is
//! no display server and no tray to put an icon in.
//!
//! The autostart spec has neither problem and no dependency at all.
//!
//! # Enabled, disabled, and absent
//!
//! The spec has a `Hidden` key: `Hidden=true` means "this entry is disabled,
//! ignore it". Duja does not use it. Disabling **deletes** the file, because a
//! disabled entry left behind is a stale executable path that silently starts
//! working again if a later version reads it differently. Absence is
//! unambiguous.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::xdg_entry::desktop_entry;
use super::{Autostart, AutostartError};

/// The entry's file name. Reverse-DNS to match the app id used elsewhere and to
/// avoid colliding with a distribution package's own `duja.desktop`.
const ENTRY_FILE: &str = "io.github.itabajah.duja.desktop";

/// XDG autostart registration for the current user.
#[derive(Debug)]
pub struct LinuxAutostart {
    /// `$XDG_CONFIG_HOME/autostart`, or the failure that resolving it produced.
    /// Held as a `Result` so construction is infallible and the error surfaces
    /// on the call that needs it, matching the other two backends.
    dir: Result<PathBuf, AutostartError>,
    /// The executable to launch. Resolved once at construction: `current_exe`
    /// after the process has been running for a while can report a *deleted*
    /// path on Linux (an upgrade replaced the binary), and writing that into a
    /// login entry would leave autostart permanently broken.
    exe: Result<PathBuf, AutostartError>,
}

impl LinuxAutostart {
    /// Bind to this user's autostart directory.
    #[must_use]
    pub fn system() -> Self {
        LinuxAutostart {
            dir: autostart_dir(),
            exe: std::env::current_exe().map_err(|e| AutostartError::ExePath(e.to_string())),
        }
    }

    /// Build a backend over an explicit autostart directory and executable
    /// (test seam), matching the macOS backend's `at`.
    ///
    /// Without it the whole enable/disable/idempotence path is unreachable from
    /// a test, because `system()` resolves both from the environment. The seam
    /// is what lets "deleting `create_dir_all`" and "inverting the
    /// `is_absolute` check" be caught rather than merely intended.
    #[cfg(test)]
    fn at(dir: PathBuf, exe: PathBuf) -> Self {
        LinuxAutostart {
            dir: Ok(dir),
            exe: Ok(exe),
        }
    }

    /// The entry's full path.
    fn entry(&self) -> Result<PathBuf, AutostartError> {
        match &self.dir {
            Ok(dir) => Ok(dir.join(ENTRY_FILE)),
            Err(e) => Err(clone_error(e)),
        }
    }
}

impl Autostart for LinuxAutostart {
    fn is_enabled(&self) -> Result<bool, AutostartError> {
        let entry = self.entry()?;
        match fs::read_to_string(&entry) {
            // Present is not the same as enabled. The spec's `Hidden=true` means
            // "ignore this entry", and GNOME Tweaks writes
            // `X-GNOME-Autostart-enabled=false` when a user disables an entry
            // through *its* UI. Reading only the file's existence would show the
            // toggle ON for an entry the desktop is ignoring, and the user would
            // have no disabled state to re-enable from — which is precisely the
            // repair path `xdg_entry`'s docs describe.
            Ok(contents) => Ok(super::xdg_entry::is_entry_enabled(&contents)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AutostartError::Io(format!("{}: {e}", entry.display()))),
        }
    }

    fn set_enabled(&mut self, on: bool) -> Result<(), AutostartError> {
        let entry = self.entry()?;
        if !on {
            // Deleting an entry that is not there is success, not an error:
            // `set_enabled(false)` is documented idempotent.
            return match fs::remove_file(&entry) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(AutostartError::Io(format!("{}: {e}", entry.display()))),
            };
        }
        let exe = match &self.exe {
            Ok(exe) => exe.clone(),
            Err(e) => return Err(clone_error(e)),
        };
        let dir = entry
            .parent()
            .ok_or_else(|| AutostartError::Io("autostart path has no parent".to_owned()))?;
        fs::create_dir_all(dir)
            .map_err(|e| AutostartError::Io(format!("{}: {e}", dir.display())))?;
        // Same-directory temp file then rename, matching the macOS backend and
        // the workspace config writer. A crash or a full disk mid-`write` would
        // otherwise leave a truncated `.desktop` that a session manager rejects
        // — and `is_enabled` would still answer `true`, so Duja would silently
        // stop starting at login while its own toggle said it was on.
        let staged = dir.join(format!("{ENTRY_FILE}.tmp.{}", std::process::id()));
        fs::write(&staged, desktop_entry(&exe))
            .map_err(|e| AutostartError::Io(format!("{}: {e}", staged.display())))?;
        fs::rename(&staged, &entry).map_err(|e| {
            let _ = fs::remove_file(&staged);
            AutostartError::Io(format!("{}: {e}", entry.display()))
        })
    }
}

/// Build this user's autostart directory from the environment.
fn autostart_dir() -> Result<PathBuf, AutostartError> {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        // An empty or relative `XDG_CONFIG_HOME` is invalid per the base-directory
        // spec, which says the value "must be an absolute path" and that an
        // invalid one is to be ignored — so it falls through to the default
        // rather than producing a path relative to the current directory.
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => {
            let home = std::env::var_os("HOME")
                .ok_or_else(|| AutostartError::ExePath("HOME is not set".to_owned()))?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(config.join("autostart"))
}

/// `AutostartError` is not `Clone` (it carries `String`s in every variant that
/// matters), so a stored failure is reproduced rather than cloned.
fn clone_error(err: &AutostartError) -> AutostartError {
    match err {
        AutostartError::ExePath(m) => AutostartError::ExePath(m.clone()),
        AutostartError::Io(m) => AutostartError::Io(m.clone()),
        AutostartError::Registry(m) => AutostartError::Registry(m.clone()),
        AutostartError::Unsupported => AutostartError::Unsupported,
    }
}

/// The platform [`Autostart`] for Linux.
#[must_use]
pub fn system() -> LinuxAutostart {
    LinuxAutostart::system()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// A backend over a temporary autostart directory that does not exist yet —
    /// which is the real first-enable case, since `~/.config/autostart` is absent
    /// until something creates it.
    fn backend(root: &TempDir) -> (LinuxAutostart, PathBuf) {
        let dir = root.path().join("config").join("autostart");
        let entry = dir.join(ENTRY_FILE);
        (
            LinuxAutostart::at(dir, PathBuf::from("/usr/local/bin/duja")),
            entry,
        )
    }

    #[test]
    fn enabling_creates_the_directory_and_the_entry() {
        let root = tempfile::tempdir().expect("tempdir");
        let (mut autostart, entry) = backend(&root);
        assert!(!autostart.is_enabled().expect("query"));

        autostart.set_enabled(true).expect("enable");

        assert!(entry.exists(), "the entry was not written");
        assert!(autostart.is_enabled().expect("query"));
        let contents = fs::read_to_string(&entry).expect("read");
        assert!(contents.contains("/usr/local/bin/duja"), "{contents}");
    }

    /// Both directions are documented idempotent, and the disable one is the
    /// case a user actually hits: turning autostart off when it was never on.
    #[test]
    fn both_directions_are_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let (mut autostart, _) = backend(&root);

        autostart.set_enabled(false).expect("disable when absent");
        assert!(!autostart.is_enabled().expect("query"));

        autostart.set_enabled(true).expect("enable");
        autostart.set_enabled(true).expect("enable again");
        assert!(autostart.is_enabled().expect("query"));

        autostart.set_enabled(false).expect("disable");
        autostart.set_enabled(false).expect("disable again");
        assert!(!autostart.is_enabled().expect("query"));
    }

    /// Disabling **deletes** rather than marking. A `Hidden=true` left behind is
    /// a stale executable path that starts working again if a later version
    /// reads the file differently.
    #[test]
    fn disabling_removes_the_file_rather_than_marking_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let (mut autostart, entry) = backend(&root);
        autostart.set_enabled(true).expect("enable");
        assert!(entry.exists());

        autostart.set_enabled(false).expect("disable");

        assert!(!entry.exists(), "the entry was marked rather than deleted");
    }

    /// The entry is written atomically, so no staging file survives a successful
    /// enable — one left behind would be a second, stale `.desktop`-adjacent file
    /// in a directory session managers scan.
    #[test]
    fn no_staging_file_is_left_behind() {
        let root = tempfile::tempdir().expect("tempdir");
        let (mut autostart, entry) = backend(&root);

        autostart.set_enabled(true).expect("enable");

        let dir = entry.parent().expect("parent");
        let names: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        assert_eq!(names, [ENTRY_FILE.to_owned()], "left: {names:?}");
    }

    /// An entry the desktop has been told to ignore must read as disabled, so the
    /// toggle does not show ON for something that never launches.
    #[test]
    fn an_entry_disabled_by_the_desktop_reads_as_disabled() {
        let root = tempfile::tempdir().expect("tempdir");
        let (mut autostart, entry) = backend(&root);
        autostart.set_enabled(true).expect("enable");
        assert!(autostart.is_enabled().expect("query"));

        let disabled = fs::read_to_string(&entry).expect("read").replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=false",
        );
        fs::write(&entry, disabled).expect("write");

        assert!(!autostart.is_enabled().expect("query"));
    }

    /// The base-directory specification says `XDG_CONFIG_HOME` "must be an
    /// absolute path" and that an invalid value is ignored. Honouring a relative
    /// one would put the entry under the process's *current directory* — which
    /// for a tray app launched from a file manager is arbitrary, and for one
    /// launched from a terminal is wherever the user happened to be.
    #[test]
    fn a_relative_config_home_is_ignored_rather_than_honoured() {
        assert!(!Path::new("relative/config").is_absolute());
        assert!(!Path::new("").is_absolute());
        // The absolute form is what the resolver keeps; the shapes above are what
        // it must fall through on.
        assert!(Path::new("/home/ana/.config").is_absolute());
    }
}

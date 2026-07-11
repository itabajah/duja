//! The macOS autostart backend: a `launchd` `LaunchAgent` plist.
//!
//! [`MacAutostart`] registers Duja to start at login by writing
//! `~/Library/LaunchAgents/io.github.itabajah.duja.plist` (see [`plist`](super::plist)
//! for the document). Enabling writes the file atomically (temp + rename);
//! disabling removes it; querying reports whether it is present. The plist alone
//! is sufficient — `launchd` loads `RunAtLoad` agents at the next graphical login
//! — so this backend never shells out to `launchctl`. **A change therefore takes
//! effect at the next login, not immediately.**
//!
//! # Why a `LaunchAgent` file rather than `SMAppService`
//!
//! `SMAppService.loginItem` (the modern API) requires a **signed `.app` bundle**
//! and macOS 13+. Duja's macOS bundle and code-signing land later (the DMG
//! packaging phase), and the software must run before then, so this backend
//! writes the `LaunchAgent` plist directly — the long-standing approach that needs
//! no bundle. Migration to `SMAppService` is tracked in `docs/debt.md`.
//!
//! # Stale-path policy
//!
//! [`is_enabled`](MacAutostart::is_enabled) mirrors the Windows backend's
//! presence policy: a plist that exists and carries a `ProgramArguments[0]`
//! counts as enabled *even if that path is stale* (points at an old executable
//! location). `set_enabled(true)` always rewrites the plist with the current
//! executable, so re-enabling repairs a stale entry.

use std::path::{Path, PathBuf};

use super::plist::{generate_plist, parse_program_argument0, plist_file_name};
use super::{Autostart, AutostartError};

/// The macOS launch-at-login backend (a `launchd` `LaunchAgent` plist).
pub struct MacAutostart {
    /// The `~/Library/LaunchAgents/io.github.itabajah.duja.plist` path.
    plist_path: PathBuf,
    /// The executable to register in `ProgramArguments`, captured at
    /// construction so `set_enabled(true)` cannot record a wrong path later.
    exe: PathBuf,
}

impl MacAutostart {
    /// Build a backend over an explicit plist path and executable (test seam).
    #[cfg(test)]
    fn at(plist_path: PathBuf, exe: PathBuf) -> Self {
        MacAutostart { plist_path, exe }
    }
}

impl Autostart for MacAutostart {
    fn is_enabled(&self) -> Result<bool, AutostartError> {
        match std::fs::read_to_string(&self.plist_path) {
            // Present ⇒ enabled iff it carries a program path (presence policy).
            Ok(contents) => Ok(parse_program_argument0(&contents).is_some()),
            // Absent ⇒ not enabled (not an error).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AutostartError::Io(format!(
                "reading {}: {e}",
                self.plist_path.display()
            ))),
        }
    }

    fn set_enabled(&mut self, on: bool) -> Result<(), AutostartError> {
        if on {
            write_atomically(&self.plist_path, &generate_plist(&self.exe))
        } else {
            remove_if_present(&self.plist_path)
        }
    }
}

/// Build the system autostart backend, resolving the current executable and the
/// `LaunchAgents` directory.
///
/// # Errors
/// [`AutostartError::ExePath`] if the current executable path cannot be
/// resolved; [`AutostartError::Io`] if there is no home directory to hold
/// `~/Library/LaunchAgents`.
pub fn system() -> Result<MacAutostart, AutostartError> {
    let exe = std::env::current_exe().map_err(|e| AutostartError::ExePath(e.to_string()))?;
    let plist_path = launch_agents_dir()
        .ok_or_else(|| {
            AutostartError::Io("no home directory for ~/Library/LaunchAgents".to_owned())
        })?
        .join(plist_file_name());
    Ok(MacAutostart { plist_path, exe })
}

/// `~/Library/LaunchAgents`, or `None` with no home directory.
fn launch_agents_dir() -> Option<PathBuf> {
    let base = directories::BaseDirs::new()?;
    Some(base.home_dir().join("Library").join("LaunchAgents"))
}

/// Write `contents` to `path` atomically: create the parent dir, write a
/// same-directory temp file, then rename it over the target (a rename within one
/// filesystem is atomic, so a reader never sees a half-written plist).
fn write_atomically(path: &Path, contents: &str) -> Result<(), AutostartError> {
    let parent = path
        .parent()
        .ok_or_else(|| AutostartError::Io("plist path has no parent directory".to_owned()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| AutostartError::Io(format!("creating {}: {e}", parent.display())))?;

    let tmp = path.with_extension(format!("plist.tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)
        .map_err(|e| AutostartError::Io(format!("writing {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AutostartError::Io(format!("renaming into {}: {e}", path.display()))
    })
}

/// Remove `path`, tolerating an already-absent file (disabling twice is fine).
fn remove_if_present(path: &Path) -> Result<(), AutostartError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AutostartError::Io(format!(
            "removing {}: {e}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend over a unique temp plist path (never the real `~/Library`).
    fn temp_backend(tag: &str) -> (MacAutostart, PathBuf) {
        let dir = std::env::temp_dir().join(format!("duja-autostart-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plist = dir.join(plist_file_name());
        (
            MacAutostart::at(
                plist.clone(),
                PathBuf::from("/Applications/Duja.app/Contents/MacOS/duja"),
            ),
            plist,
        )
    }

    #[test]
    fn disabled_when_no_plist_exists() {
        let (backend, plist) = temp_backend("absent");
        assert!(!backend.is_enabled().expect("query"));
        let _ = std::fs::remove_dir_all(plist.parent().expect("parent"));
    }

    #[test]
    fn enable_writes_the_plist_then_reads_enabled() {
        let (mut backend, plist) = temp_backend("enable");
        backend.set_enabled(true).expect("enable");
        assert!(plist.exists(), "the plist file was written");
        assert!(backend.is_enabled().expect("query"));

        let contents = std::fs::read_to_string(&plist).expect("read plist");
        assert_eq!(
            parse_program_argument0(&contents).as_deref(),
            Some("/Applications/Duja.app/Contents/MacOS/duja")
        );
        let _ = std::fs::remove_dir_all(plist.parent().expect("parent"));
    }

    #[test]
    fn disable_removes_the_plist_and_is_idempotent() {
        let (mut backend, plist) = temp_backend("disable");
        backend.set_enabled(true).expect("enable");
        backend.set_enabled(false).expect("disable");
        assert!(!plist.exists(), "the plist file was removed");
        assert!(!backend.is_enabled().expect("query"));
        // Disabling again (already absent) is a no-op success.
        backend.set_enabled(false).expect("disable-absent");
        let _ = std::fs::remove_dir_all(plist.parent().expect("parent"));
    }

    #[test]
    fn enable_is_idempotent() {
        let (mut backend, plist) = temp_backend("reenable");
        backend.set_enabled(true).expect("enable");
        backend.set_enabled(true).expect("enable-again");
        assert!(backend.is_enabled().expect("query"));
        let _ = std::fs::remove_dir_all(plist.parent().expect("parent"));
    }
}

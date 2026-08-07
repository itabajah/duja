//! Resolved on-disk locations for Duja's config, volatile state, crash marker,
//! and logs.
//!
//! Paths come from [`directories::ProjectDirs`] for
//! `("io.github", "itabajah", "duja")` — the platform-correct per-user
//! locations. The config file is the user-facing settings; state, marker and logs
//! are volatile machine data and live under the data dir so a config backup never
//! drags them along.
//!
//! Resolved, per platform (`config_dir()` / `data_dir()`, i.e. the **roaming**
//! Windows root, not the local one — worth stating because
//! `.github/ISSUE_TEMPLATE/bug.yml` quotes the log path to bug reporters, and this
//! comment previously claimed `%LOCALAPPDATA%`, where nothing is ever written):
//!
//! | | config | state / marker / logs |
//! |---|---|---|
//! | Windows | `%APPDATA%\itabajah\duja\config` | `%APPDATA%\itabajah\duja\data` |
//! | macOS | `~/Library/Application Support/io.github.itabajah.duja` | same |
//! | Linux | `$XDG_CONFIG_HOME/duja` | `$XDG_DATA_HOME/duja` |

use std::path::PathBuf;

use directories::ProjectDirs;

/// The file name of the user-facing configuration.
const CONFIG_FILE: &str = "config.toml";
/// The file name of the volatile per-display level state.
const STATE_FILE: &str = "state.toml";
/// The crash marker written before the first gamma engage (see
/// `duja_dimmer::mark_dirty`, which was Windows-only until X11 turned out to
/// leave the same residue).
const MARKER_FILE: &str = "gamma.dirty";
/// The subdirectory that holds the rotating log files.
const LOG_DIR: &str = "logs";

/// Fully-resolved Duja paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DujaPaths {
    /// The user's `config.toml`.
    pub(crate) config: PathBuf,
    /// The volatile `state.toml` (last per-display levels).
    pub(crate) state: PathBuf,
    /// The gamma crash marker.
    pub(crate) crash_marker: PathBuf,
    /// The directory holding rotating log files.
    pub(crate) log_dir: PathBuf,
}

impl DujaPaths {
    /// Resolve the standard per-user locations, or `None` if the platform has no
    /// home directory (a headless/service context we degrade from).
    pub(crate) fn resolve() -> Option<Self> {
        let dirs = ProjectDirs::from("io.github", "itabajah", "duja")?;
        Some(DujaPaths {
            config: dirs.config_dir().join(CONFIG_FILE),
            state: dirs.data_dir().join(STATE_FILE),
            crash_marker: dirs.data_dir().join(MARKER_FILE),
            log_dir: dirs.data_dir().join(LOG_DIR),
        })
    }

    /// Resolve the standard per-user locations, or fall back to a temp-dir root
    /// when no home directory is resolvable. Always yields a usable set of paths,
    /// so file logging and state persistence keep working off the temp root
    /// instead of being silently disabled (the tray runs from this same fallback).
    pub(crate) fn resolve_or_fallback() -> Self {
        Self::resolve().unwrap_or_else(Self::fallback)
    }

    /// All paths under a `duja` directory in the OS temp dir, used when no home
    /// directory resolves. Logged as a degraded mode.
    fn fallback() -> Self {
        let root = std::env::temp_dir().join("duja");
        tracing::warn!(root = %root.display(), "no home directory; using a temp data root");
        Self::under_root(&root)
    }

    /// Build all paths under an explicit `root` directory.
    fn under_root(root: &std::path::Path) -> Self {
        DujaPaths {
            config: root.join(CONFIG_FILE),
            state: root.join(STATE_FILE),
            crash_marker: root.join(MARKER_FILE),
            log_dir: root.join(LOG_DIR),
        }
    }

    /// Build all paths under an explicit root (used by tests with a temp dir).
    #[cfg(test)]
    pub(crate) fn under(root: &std::path::Path) -> Self {
        Self::under_root(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn under_root_places_every_file() {
        let root = Path::new("/tmp/duja-test-root");
        let p = DujaPaths::under(root);
        assert!(p.config.ends_with("config.toml"));
        assert!(p.state.ends_with("state.toml"));
        assert!(p.crash_marker.ends_with("gamma.dirty"));
        assert!(p.log_dir.ends_with("logs"));
    }

    #[test]
    fn resolve_yields_duja_qualified_paths() {
        // On any dev/CI host with a home dir this resolves; assert the app
        // qualifier shows up in the config path.
        if let Some(p) = DujaPaths::resolve() {
            let s = p.config.to_string_lossy().to_lowercase();
            assert!(s.contains("duja"), "config path = {s}");
        }
    }

    /// `.github/ISSUE_TEMPLATE/bug.yml` tells bug reporters to open a literal log
    /// path, so which Windows root this resolves to is now a documented promise
    /// rather than an implementation detail. `ProjectDirs::data_dir()` is
    /// `{FOLDERID_RoamingAppData}\…\data` — the **roaming** root; the local one is
    /// `data_local_dir()`, which Duja does not use. This module's own doc comment
    /// said `%LOCALAPPDATA%` until `#95`, where a live check found the logs under
    /// `%APPDATA%` instead, so the claim is pinned here.
    #[cfg(windows)]
    #[test]
    fn windows_logs_live_under_the_roaming_appdata_root() {
        let Some(p) = DujaPaths::resolve() else {
            return;
        };
        let Ok(roaming) = std::env::var("APPDATA") else {
            return;
        };
        assert!(
            p.log_dir.starts_with(&roaming),
            "log_dir {:?} must be under %APPDATA% ({roaming}) — bug.yml quotes it",
            p.log_dir
        );
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            assert!(
                !p.log_dir.starts_with(&local),
                "log_dir {:?} is not under %LOCALAPPDATA%",
                p.log_dir
            );
        }
    }

    #[test]
    fn fallback_paths_land_under_the_temp_root() {
        // When no home dir resolves the app degrades onto a temp root rather than
        // disabling file logging/state; assert the derived paths are non-empty and
        // sit under the OS temp dir, so `init_logging` still has somewhere to write.
        let p = DujaPaths::fallback();
        let temp = std::env::temp_dir();
        assert!(
            p.log_dir.starts_with(&temp),
            "log_dir {:?} not under temp {:?}",
            p.log_dir,
            temp
        );
        assert!(p.log_dir.ends_with("logs"));
        assert!(p.crash_marker.starts_with(&temp));
        assert!(!p.log_dir.as_os_str().is_empty());
    }
}

//! Resolved on-disk locations for Duja's config, volatile state, crash marker,
//! and logs.
//!
//! Paths come from [`directories::ProjectDirs`] for
//! `("io.github", "itabajah", "duja")` — the platform-correct per-user
//! locations (on Windows, `%APPDATA%\itabajah\duja` for config and
//! `%LOCALAPPDATA%\itabajah\duja\data` for state/logs). The config file is the
//! user-facing settings; state, marker and logs are volatile machine data and
//! live under the data dir so a config backup never drags them along.

use std::path::PathBuf;

use directories::ProjectDirs;

/// The file name of the user-facing configuration.
const CONFIG_FILE: &str = "config.toml";
/// The file name of the volatile per-display level state.
const STATE_FILE: &str = "state.toml";
/// The crash marker written before the first gamma engage (see
/// [`duja_dimmer::mark_dirty`]).
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

    /// Build all paths under an explicit root (used by tests with a temp dir).
    #[cfg(test)]
    pub(crate) fn under(root: &std::path::Path) -> Self {
        DujaPaths {
            config: root.join(CONFIG_FILE),
            state: root.join(STATE_FILE),
            crash_marker: root.join(MARKER_FILE),
            log_dir: root.join(LOG_DIR),
        }
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
}

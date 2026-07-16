//! Volatile per-display state, stored separately from the user's config.
//!
//! The last brightness level for each display changes far more often than
//! settings do (every slider move), so it lives in its own small file written
//! through the same crash-safe [`persist`] path. Keeping
//! it separate means frequent level writes never rewrite — and never risk
//! corrupting — `config.toml`.
//!
//! Level writes are meant to be debounced; the pure [`should_write`] helper
//! encodes the ≥2 s trailing-edge rule, while the actual timer lives with the
//! caller (matching the clock-passed-in discipline used elsewhere in the core).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::error::ConfigError;
use crate::config::persist;

/// The schema version stamped into the state file.
const STATE_VERSION: u32 = 1;

/// The minimum quiet period between successive state writes (trailing-edge).
pub const STATE_WRITE_DEBOUNCE: Duration = Duration::from_secs(2);

/// Whether enough time has elapsed since the last state write to write again.
///
/// Returns `true` when nothing has been written yet (`last_write` is `None`) or
/// at least [`STATE_WRITE_DEBOUNCE`] has passed since `last_write`. Pure and
/// clock-free: the caller supplies both instants, so the timing is fully
/// testable and the actual scheduling stays in the caller.
#[must_use]
pub fn should_write(now: Instant, last_write: Option<Instant>) -> bool {
    match last_write {
        None => true,
        Some(previous) => now.saturating_duration_since(previous) >= STATE_WRITE_DEBOUNCE,
    }
}

/// The last recorded brightness level for one display, plus when it was set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelEntry {
    /// The unified user brightness level, `0..=100`.
    pub user_level_pct: u8,
    /// When this level was recorded, in seconds since the Unix epoch.
    ///
    /// A plain integer (not a wall-clock type) keeps the state file trivially
    /// serializable and the core free of a time-zone dependency; the caller
    /// supplies the timestamp.
    pub updated_at_unix: i64,
}

/// The volatile state file: last levels keyed by [`StableDisplayId`] string.
///
/// [`StableDisplayId`]: crate::id::StableDisplayId
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StateFile {
    /// The state schema version.
    pub schema_version: u32,
    /// When the update check last ran, in seconds since the Unix epoch.
    ///
    /// Lives here — not in the user's config — because it is volatile bookkeeping
    /// the user never edits. `None` until the first manual check. A scalar, so it
    /// serializes ahead of the `levels` table.
    pub last_update_check_unix: Option<i64>,
    /// Last-known level per display id. Declared last so it serializes after the
    /// scalar keys.
    pub levels: BTreeMap<String, LevelEntry>,
}

impl Default for StateFile {
    fn default() -> Self {
        StateFile {
            schema_version: STATE_VERSION,
            last_update_check_unix: None,
            levels: BTreeMap::new(),
        }
    }
}

impl StateFile {
    /// Load the state file, returning defaults when the file is missing.
    ///
    /// # Errors
    /// [`ConfigError::Io`] on a read failure, or [`ConfigError::Deserialize`]
    /// if the file exists but is not valid state TOML. The caller decides
    /// whether to treat a corrupt (disposable) state file as "reset to
    /// default"; this function never does so silently.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match persist::read_to_string_opt(path)? {
            None => Ok(StateFile::default()),
            Some(text) => toml_edit::de::from_str(&text).map_err(ConfigError::Deserialize),
        }
    }

    /// Atomically write the state file.
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] if the state cannot be rendered to TOML, or
    /// [`ConfigError::Io`] if the atomic write fails.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml_edit::ser::to_string(self).map_err(ConfigError::Serialize)?;
        persist::write_atomic(path, &text)
    }

    /// Record the latest level for a display, replacing any previous entry.
    pub fn record(&mut self, id: impl Into<String>, user_level_pct: u8, updated_at_unix: i64) {
        self.levels.insert(
            id.into(),
            LevelEntry {
                user_level_pct,
                updated_at_unix,
            },
        );
    }

    /// The last recorded level for `id`, if any.
    #[must_use]
    pub fn level(&self, id: &str) -> Option<u8> {
        self.levels.get(id).map(|entry| entry.user_level_pct)
    }

    /// Record that the update check ran at `unix` (seconds since the epoch).
    pub fn record_update_check(&mut self, unix: i64) {
        self.last_update_check_unix = Some(unix);
    }

    /// When the update check last ran, if ever.
    #[must_use]
    pub fn last_update_check(&self) -> Option<i64> {
        self.last_update_check_unix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base.checked_add(Duration::from_secs(secs))
            .expect("no overflow")
    }

    #[test]
    fn state_write_debounce_helper() {
        let base = Instant::now();
        // Never written before: always allowed.
        assert!(should_write(base, None));
        // Within the 2 s window: suppressed.
        assert!(!should_write(at(base, 1), Some(base)));
        // Just under the threshold: still suppressed.
        assert!(!should_write(
            base.checked_add(Duration::from_millis(1999))
                .expect("no overflow"),
            Some(base)
        ));
        // Exactly at the threshold: allowed (>=).
        assert!(should_write(at(base, 2), Some(base)));
        // Well past: allowed.
        assert!(should_write(at(base, 10), Some(base)));
    }

    #[test]
    fn debounce_tolerates_non_monotonic_now() {
        // A `now` earlier than `last_write` (clock oddity) must not panic and
        // must suppress the write rather than wildly allowing it.
        let base = Instant::now();
        let earlier = base;
        let later = at(base, 5);
        assert!(!should_write(earlier, Some(later)));
    }

    #[test]
    fn default_state_is_empty_and_versioned() {
        let state = StateFile::default();
        assert_eq!(state.schema_version, STATE_VERSION);
        assert!(state.levels.is_empty());
        assert_eq!(state.level("anything"), None);
    }

    #[test]
    fn empty_state_toml_yields_defaults() {
        let state: StateFile = toml_edit::de::from_str("").expect("empty is valid");
        assert_eq!(state, StateFile::default());
    }

    #[test]
    fn record_and_read_back_levels() {
        let mut state = StateFile::default();
        state.record("GSM-5B09-312NTAB1C234", 42, 1_700_000_000);
        state.record("DEL-A131-s12345", 80, 1_700_000_001);
        assert_eq!(state.level("GSM-5B09-312NTAB1C234"), Some(42));
        assert_eq!(state.level("DEL-A131-s12345"), Some(80));
        // Re-recording replaces the previous value.
        state.record("GSM-5B09-312NTAB1C234", 15, 1_700_000_002);
        assert_eq!(state.level("GSM-5B09-312NTAB1C234"), Some(15));
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");

        let mut state = StateFile::default();
        state.record("GSM-5B09-312NTAB1C234", 55, 1_700_000_000);
        state.save(&path).expect("save");

        let loaded = StateFile::load(&path).expect("load");
        assert_eq!(loaded, state);
    }

    #[test]
    fn last_update_check_records_and_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");

        let mut state = StateFile::default();
        assert_eq!(state.last_update_check(), None);
        state.record("GSM-5B09-A", 40, 1_700_000_000);
        state.record_update_check(1_700_000_500);
        state.save(&path).expect("save");

        let loaded = StateFile::load(&path).expect("load");
        assert_eq!(loaded.last_update_check(), Some(1_700_000_500));
        assert_eq!(loaded.level("GSM-5B09-A"), Some(40));
        assert_eq!(loaded, state);
    }

    #[test]
    fn missing_state_file_loads_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent-state.toml");
        assert_eq!(StateFile::load(&path).expect("load"), StateFile::default());
    }

    #[test]
    fn corrupt_state_file_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        persist::write_atomic(&path, "levels = \"not a table\"\n").expect("write garbage");
        assert!(matches!(
            StateFile::load(&path),
            Err(ConfigError::Deserialize(_))
        ));
    }
}

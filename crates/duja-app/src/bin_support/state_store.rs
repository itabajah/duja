//! The app-side user-level book with debounced persistence.
//!
//! Duja is the owner of each display's *user* slider level (the engine only ever
//! sees the continuum's hardware target). Those levels live in the volatile
//! [`StateFile`], written through core's crash-safe atomic path. Because a slider
//! drag changes the level dozens of times a second, writes are **debounced**:
//! [`record`](StateStore::record) only updates memory;
//! [`maybe_flush`](StateStore::maybe_flush) persists at most once per
//! `STATE_WRITE_DEBOUNCE` (≥ 2 s) using core's pure [`should_write`] rule, so the
//! disk never churns while the user is dragging.
//! A clean exit calls [`flush`](StateStore::flush) to save the final value.
//!
//! The debounce clock is injected (`now: Instant`) — no timer runs while idle;
//! the app calls [`maybe_flush`](StateStore::maybe_flush) opportunistically from
//! its notification loop.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::PathBuf;
use std::time::Instant;

use duja_core::config::state::{StateFile, should_write};

/// A loaded state file plus the debounce bookkeeping for writing it back.
#[derive(Debug)]
pub(crate) struct StateStore {
    path: PathBuf,
    file: StateFile,
    last_write: Option<Instant>,
    dirty: bool,
}

impl StateStore {
    /// Load the state file at `path`, falling back to an empty state if it is
    /// missing or unreadable/corrupt (a disposable file — never fatal).
    pub(crate) fn load(path: PathBuf) -> Self {
        let file = match StateFile::load(&path) {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!(error = %err, "state file unreadable; starting from empty state");
                StateFile::default()
            }
        };
        StateStore {
            path,
            file,
            last_write: None,
            dirty: false,
        }
    }

    /// The last recorded user level for `id`, if any.
    pub(crate) fn level(&self, id: &str) -> Option<u8> {
        self.file.level(id)
    }

    /// Record a new user level for `id` in memory (marks the store dirty).
    pub(crate) fn record(&mut self, id: &str, user_level_pct: u8, updated_at_unix: i64) {
        self.file
            .record(id.to_owned(), user_level_pct, updated_at_unix);
        self.dirty = true;
    }

    /// Record that the update check just ran at `unix` (marks the store dirty).
    pub(crate) fn record_update_check(&mut self, unix: i64) {
        self.file.record_update_check(unix);
        self.dirty = true;
    }

    /// Persist the state if it is dirty and the debounce window has elapsed.
    ///
    /// Returns `true` if a write happened. Never an error path for the caller: a
    /// failed write is logged and the store stays dirty so a later flush retries.
    pub(crate) fn maybe_flush(&mut self, now: Instant) -> bool {
        if !self.dirty || !should_write(now, self.last_write) {
            return false;
        }
        self.write(now)
    }

    /// Force a persist now (e.g. on clean shutdown), ignoring the debounce.
    /// Returns `true` if a write happened.
    pub(crate) fn flush(&mut self, now: Instant) -> bool {
        if !self.dirty {
            return false;
        }
        self.write(now)
    }

    /// Perform the atomic write and update the debounce clock.
    fn write(&mut self, now: Instant) -> bool {
        match self.file.save(&self.path) {
            Ok(()) => {
                self.last_write = Some(now);
                self.dirty = false;
                true
            }
            Err(err) => {
                tracing::warn!(error = %err, path = %self.path.display(), "failed to persist state");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(base: Instant, secs: u64) -> Instant {
        base.checked_add(Duration::from_secs(secs))
            .expect("no overflow")
    }

    fn store() -> (StateStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        (StateStore::load(path), dir)
    }

    #[test]
    fn first_record_flushes_immediately() {
        let (mut s, _dir) = store();
        let base = Instant::now();
        s.record("GSM-5B09-A", 40, 1_700_000_000);
        // First write is allowed (last_write is None).
        assert!(s.maybe_flush(base));
        assert_eq!(s.level("GSM-5B09-A"), Some(40));
    }

    #[test]
    fn writes_within_the_window_are_debounced() {
        let (mut s, _dir) = store();
        let base = Instant::now();
        s.record("GSM-5B09-A", 40, 1_700_000_000);
        assert!(s.maybe_flush(base));
        // A change 1s later is suppressed (< 2s window).
        s.record("GSM-5B09-A", 55, 1_700_000_001);
        assert!(!s.maybe_flush(at(base, 1)));
        // At 2s the write lands.
        assert!(s.maybe_flush(at(base, 2)));
    }

    #[test]
    fn maybe_flush_is_noop_when_not_dirty() {
        let (mut s, _dir) = store();
        assert!(!s.maybe_flush(Instant::now()));
    }

    #[test]
    fn flush_forces_a_write_regardless_of_debounce() {
        let (mut s, _dir) = store();
        let base = Instant::now();
        s.record("A", 40, 1);
        assert!(s.maybe_flush(base));
        s.record("A", 60, 2);
        // Debounced maybe_flush would suppress, but flush forces it.
        assert!(!s.maybe_flush(at(base, 1)));
        assert!(s.flush(at(base, 1)));
        assert!(!s.flush(at(base, 1))); // now clean
    }

    #[test]
    fn persisted_levels_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        {
            let mut s = StateStore::load(path.clone());
            s.record("DEL-A131-x", 72, 1_700_000_000);
            assert!(s.flush(Instant::now()));
        }
        let reloaded = StateStore::load(path);
        assert_eq!(reloaded.level("DEL-A131-x"), Some(72));
    }
}

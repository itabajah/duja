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

use std::path::PathBuf;
use std::time::Instant;

use duja_core::config::ConfigError;
use duja_core::config::state::{StateFile, should_write};

/// A loaded state file plus the debounce bookkeeping for writing it back.
#[derive(Debug)]
pub(crate) struct StateStore {
    path: PathBuf,
    file: StateFile,
    last_write: Option<Instant>,
    dirty: bool,
    /// Whether something that must not be overwritten is still at
    /// [`path`](Self::path).
    ///
    /// True in the two cases where writing would destroy something and not
    /// writing is the lesser harm: an unreadable file that could not be moved
    /// aside, and a read failure that may be **transient**, where the file may
    /// be perfectly good and a later launch will read it. See
    /// [`StateStore::load`], and note what this costs: the update-check
    /// timestamp lives in the same file, so a blocked session also re-checks for
    /// updates at every launch.
    blocked: bool,
}

/// What [`StateStore::load`] appends to the file **name** when it moves an
/// unreadable file out of the way.
///
/// Appended rather than substituted, which is what `Path::with_extension` does:
/// that turns `state.toml` into `state.unreadable` and loses the name a user
/// would recognise. The first version of this used it, so the code produced
/// `state.unreadable` while three sentences of prose said
/// `state.toml.unreadable`. The tests sided with the code, because they built
/// the expected path with the same call, so nothing could catch the
/// disagreement. Building the name explicitly is what makes the two agree.
const QUARANTINE_SUFFIX: &str = ".unreadable";

/// Whether re-reading a file that failed this way would fail the same way.
///
/// The question the quarantine turns on, and **not** the same as "is it a
/// `ConfigError::Io`", which is what an earlier version of this tested. The
/// read path maps **invalid UTF-8 to `Io(InvalidData)`** - `read_capped` builds
/// it that way deliberately, and `persist.rs` pins it - so a `state.toml` saved
/// as UTF-16, which is what Notepad's "Save as -> Unicode" produces, arrives as
/// an `Io` error and is a *content* failure that will never clear. Sending it to
/// the transient arm meant levels silently never persisting again, every
/// session: verbatim the outcome the quarantine was chosen to remove, reached by
/// a different door.
///
/// So the test is on the **kind**, not the variant. `InvalidData` is the file
/// being wrong, and it is the only kind this promotes.
///
/// That is narrower than the headline criterion, deliberately, and the gap is a
/// known residual rather than an oversight. A `PermissionDenied` - a directory
/// at the path, a file the user cannot read - *would* fail the same way on every
/// launch, so by the criterion alone it should quarantine. It does not, because
/// "cannot read it" and "cannot move it" are the same permission in most of the
/// ways that happens, and a `false` here costs a session's persistence while a
/// wrong `true` costs the file. The cost is real: levels silently never persist,
/// which is what this design exists to remove. It is the one case where the safe
/// answer is still the unhappy one.
fn repeats(err: &ConfigError) -> bool {
    match err {
        // Too big to read, or read and unparseable. Both are about the bytes.
        ConfigError::TooLarge { .. } | ConfigError::Deserialize(_) => true,
        ConfigError::Io(io) => io.kind() == std::io::ErrorKind::InvalidData,
        // `ConfigError` is `#[non_exhaustive]`. A future variant is unknown
        // rather than known-permanent, and the safe unknown is "do not move the
        // user's file".
        _ => false,
    }
}

/// A quarantine name nothing is using yet.
///
/// `fs::rename` **replaces** an existing destination file on every supported
/// platform, so a user who corrupted the file twice would have lost their first
/// copy silently - no log line, nothing to notice it by. That is the same class
/// of harm the quarantine exists to prevent, one step further along. Two drafts
/// of this comment then got the *exception* wrong in turn: an occupied name does
/// block the rename when the occupant is a **directory**, empty or not - POSIX
/// makes `rename(file, dir)` `EISDIR` regardless, and Windows answers
/// `PermissionDenied` for both, measured. What it does not block is an occupant
/// *file*, which is exactly what a real second corruption leaves behind.
///
/// So a taken name gets a numeric suffix, and the search is **bounded**. A
/// machine that has produced a hundred distinct corrupt state files has a
/// problem this cannot help with, and an unbounded scan of a directory somebody
/// else may be writing into is the worse answer. `None` means every candidate
/// was taken, and the caller then refuses to write - the same conclusion it
/// reaches when the rename itself fails.
fn free_quarantine_path(path: &std::path::Path) -> Option<PathBuf> {
    let first = quarantine_path(path);
    if !first.exists() {
        return Some(first);
    }
    (1_u32..100).find_map(|n| {
        let mut name = first.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{n}"));
        let candidate = first.with_file_name(name);
        (!candidate.exists()).then_some(candidate)
    })
}

/// Where an unreadable `path` is moved to: its own name with
/// [`QUARANTINE_SUFFIX`] appended.
fn quarantine_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(QUARANTINE_SUFFIX);
    path.with_file_name(name)
}

impl StateStore {
    /// Load the state file at `path`, moving it aside if it cannot be read.
    ///
    /// # A missing file and an unreadable one are not the same thing
    ///
    /// Both used to produce an empty [`StateFile`] and a store that would
    /// happily write over `path` at the next flush. For a **missing** file that
    /// is exactly right - every first run has none. For an **unreadable** one it
    /// is destructive and silent: the next `record()` marks the store dirty and
    /// the flush atomically replaces the user's file with defaults.
    /// `docs/debt-archive.md` D-113 found the reachable version - a `state.toml`
    /// over `MAX_CONFIG_LEN` loaded fine before P8 wave 5, is refused by the read
    /// cap afterwards, and was then overwritten.
    ///
    /// # Why some failures quarantine and others refuse to write
    ///
    /// The first version of this fix latched and refused every write for the
    /// life of the process, whatever went wrong. A review priced the two failure
    /// modes that argument covered:
    ///
    /// - [`ConfigError::TooLarge`] needs somewhere between **thirteen and
    ///   fifteen thousand** entries. Measured through `StateFile::save` rather
    ///   than estimated: 68 bytes per entry for a ten-character id, 79 for a
    ///   twenty-one-character one, so the count depends on how long the ids
    ///   happen to be. An earlier draft gave a single figure, "~71 bytes", which
    ///   no id length in the tree actually produces. Either way it is
    ///   essentially unreachable, and it is the case the refusal was argued
    ///   from.
    /// - `Deserialize` needs **one bad byte** in a hand-edit. Entirely reachable,
    ///   and there the *old* behaviour self-healed: one reset, then persistence
    ///   continued. Refusing turns that into levels silently never persisting
    ///   again, every session, until the user finds a file they have no reason to
    ///   look at - and the settings banner cannot help, because it reports
    ///   `config.toml` writes and this is a different file.
    ///
    /// So those two take the remedy the first version listed as considered and
    /// not taken - it named the file `state.toml.corrupt`, so this is that idea
    /// rather than that exact plan. Rename and start fresh preserves the file
    /// **and** keeps persistence working.
    ///
    /// That version gave three reasons for deferring it, and the measurement
    /// above answers only one. "Moving a user's file is a policy decision about
    /// their filesystem" is settled by the cost of not doing it. "The row asked
    /// for the refusal" is settled by the row being wrong about which failure it
    /// was protecting against. **"It needs somewhere to say so in the UI" is not
    /// settled**: the only signal is a `tracing::warn!`, and the settings banner
    /// reports `config.toml` writes rather than this file. That is a real
    /// residual and it is named rather than quietly dropped.
    ///
    /// **A failure that may be *transient* does not quarantine**, and the test
    /// for that is not the `ConfigError` variant - which is what the second
    /// version of this fix used, and it was wrong twice over. It first matched
    /// every error the same way, so antivirus holding the handle would have moved
    /// the user's file for a condition that clears on its own. Narrowing it to
    /// the `TooLarge` and `Deserialize` *variants* then missed that the read path
    /// maps invalid UTF-8 to `Io(InvalidData)`. The question is
    /// [`repeats`]'s: would re-reading fail the same way? Those that would are
    /// quarantined; those that might not refuse to write and touch nothing, so
    /// the next launch reads the file it always would have.
    ///
    /// If a quarantine rename **also** fails - a read-only directory, a
    /// permission problem - the file can be neither preserved nor safely
    /// replaced, and [`blocked`](Self::blocked) refuses every write there too.
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut blocked = false;
        let file = match StateFile::load(&path) {
            Ok(file) => file,
            // The content is definitively unusable, so re-reading will give the
            // same answer for as long as the bytes are what they are, and
            // quarantining is safe. See `repeats` for why the test is not simply
            // "is it a `ConfigError::Io`".
            Err(err) if repeats(&err) => {
                match free_quarantine_path(&path)
                    .ok_or_else(|| std::io::Error::other("every quarantine name is taken"))
                    .and_then(|aside| std::fs::rename(&path, &aside).map(|()| aside))
                {
                    Ok(aside) => tracing::warn!(
                        error = %err,
                        moved_to = %aside.display(),
                        "state file could not be read; it has been moved aside and a \
                         fresh one will be written"
                    ),
                    Err(move_err) => {
                        blocked = true;
                        tracing::warn!(
                            error = %err,
                            move_error = %move_err,
                            path = %path.display(),
                            "state file could not be read or moved aside; running from \
                             empty state and refusing to overwrite it. Levels and the \
                             update-check timestamp will not persist until this file \
                             is moved or removed by hand and Duja is restarted."
                        );
                    }
                }
                StateFile::default()
            }
            // Everything else may be **transient**: antivirus holding the
            // handle, a network share hiccup, a sharing violation. Renaming a
            // user's file aside for a condition that would have cleared on its
            // own is a new destructive behaviour, introduced in the name of
            // removing one. Refuse to write instead - nothing moves, nothing is
            // replaced, and the next launch reads the file it always would
            // have.
            Err(err) => {
                blocked = true;
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "state file could not be read; running from empty state and \
                     refusing to overwrite it, in case the failure is transient. \
                     Levels and the update-check timestamp will not persist this \
                     session"
                );
                StateFile::default()
            }
        };
        StateStore {
            path,
            file,
            last_write: None,
            dirty: false,
            blocked,
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

    /// The Unix timestamp of the last update check, if one has ever run.
    pub(crate) fn last_update_check(&self) -> Option<i64> {
        self.file.last_update_check()
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
    ///
    /// Refuses outright if an unreadable file is still in the way - see
    /// [`load`](Self::load), which only leaves one there when it could not be
    /// moved aside either. The store stays dirty, which costs nothing: every
    /// caller treats `false` as "not written yet" and simply asks again.
    fn write(&mut self, now: Instant) -> bool {
        if self.blocked {
            return false;
        }
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

    /// A store over a file whose contents `text` cannot be parsed or read.
    fn store_over(text: &str) -> (StateStore, std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        std::fs::write(&path, text).expect("write the fixture file");
        (StateStore::load(path.clone()), path, dir)
    }

    /// **An unreadable state file is preserved rather than destroyed**, and
    /// persistence keeps working.
    ///
    /// D-113's destructive half, with the case that motivated it: a `state.toml`
    /// over `MAX_CONFIG_LEN`. Before P8 wave 5's read cap it loaded fine;
    /// afterwards `read_to_string_opt` refuses it outright - the file is **not
    /// read at all**, which is worth stating precisely because two drafts of this
    /// said "read as garbage" - `load` substitutes defaults, and the first
    /// `record()` marks the store dirty so the next flush atomically destroys it.
    ///
    /// Goes red against the pre-fix `load`, at the site: it discarded the error,
    /// so nothing downstream could know the file had not been read.
    #[test]
    fn an_unreadable_state_file_is_moved_aside_rather_than_destroyed() {
        let oversized = "x".repeat(duja_core::config::persist::MAX_CONFIG_LEN + 1);
        let (mut s, path, _dir) = store_over(&oversized);

        // The user's bytes are still on disk, under a name that says why.
        let aside = quarantine_path(&path);
        assert!(!path.exists(), "the unreadable file is not left in the way");
        assert_eq!(
            std::fs::read_to_string(&aside)
                .expect("moved aside, not deleted")
                .len(),
            oversized.len(),
            "and byte-for-byte what it was"
        );

        // And the session persists normally, which is the half the first version
        // of this fix gave up.
        s.record("GSM-5B09-A", 40, 1_700_000_000);
        assert!(s.flush(Instant::now()), "a fresh file must be writable");
        assert!(
            std::fs::read_to_string(&path)
                .expect("a fresh state file")
                .contains("user_level_pct = 40")
        );
    }

    /// The same for a file that is small but unparseable - and this is the case
    /// that actually happens.
    ///
    /// `TooLarge` needs thirteen to fifteen thousand entries, depending on how
    /// long the ids are; a `Deserialize` failure needs one bad byte in a hand-edit. The first
    /// version of this fix refused every write for the life of the process, which
    /// here would mean levels silently never persisting again, every session,
    /// over a typo - worse than the behaviour it replaced, which at least
    /// self-healed. A review priced both, and that is what chose the quarantine.
    #[test]
    fn an_unparseable_state_file_is_moved_aside_too() {
        let (mut s, path, _dir) = store_over(
            "levels = \"not a table\"
",
        );

        assert_eq!(
            std::fs::read_to_string(quarantine_path(&path)).expect("preserved"),
            "levels = \"not a table\"
"
        );

        s.record("GSM-5B09-A", 40, 1_700_000_000);
        assert!(s.flush(Instant::now()), "and persistence self-heals");
    }

    /// **Invalid UTF-8 quarantines**, because it is the file being wrong.
    ///
    /// The case that caught the second version of this fix. `read_capped` maps
    /// invalid UTF-8 to `Io(InvalidData)`, so a `state.toml` saved as UTF-16 -
    /// Notepad's "Save as -> Unicode" - arrives as an `Io` error while being a
    /// *content* failure that never clears. Matching on the `ConfigError`
    /// **variant** sent it to the transient arm, and levels then silently never
    /// persisted again, every session: verbatim the outcome this whole design
    /// exists to remove, reached by a different door.
    ///
    /// It doubles as the fixture the transient test needs, for a reason worth
    /// stating: a UTF-16 file is unreadable **and writable**, so "the write was
    /// refused" is a real assertion about it. A *directory* at the path - which
    /// is what the first transient test used - is unreadable and unwritable, so
    /// that assertion passed with the guard deleted.
    #[test]
    fn invalid_utf8_quarantines_because_re_reading_cannot_fix_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        // UTF-16LE with a BOM, which is bytes no UTF-8 read can accept.
        let utf16: Vec<u8> = std::iter::once(0xFF_u8)
            .chain(std::iter::once(0xFE))
            .chain("levels = {}".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        std::fs::write(&path, &utf16).expect("write the fixture");

        let mut s = StateStore::load(path.clone());
        assert_eq!(
            std::fs::read(quarantine_path(&path)).expect("preserved"),
            utf16,
            "the user's bytes are kept"
        );

        s.record("GSM-5B09-A", 40, 1_700_000_000);
        assert!(s.flush(Instant::now()), "and persistence self-heals");
    }

    /// **A transient I/O failure neither quarantines nor overwrites.**
    ///
    /// The distinction the criterion turns on: `InvalidData` is the file being
    /// wrong, everything else `std::io` reports here is the world being busy -
    /// antivirus holding the handle, a network share hiccup, a sharing
    /// violation. Moving a user's file for one of those would be a new
    /// destructive behaviour introduced while removing one.
    ///
    /// A directory at the path is the only `Io` kind a test can arrange
    /// portably, and its limitation is why the assertion below is on `blocked`
    /// directly: a directory is also *unwritable*, so `!flush()` passes here
    /// whatever the guard says. An earlier version asserted only the flush and
    /// claimed the guard was "pinned by" two other tests - it is not, and a
    /// review proved it by flipping this arm alone with nothing reddening.
    #[test]
    fn a_transient_io_failure_does_not_quarantine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        std::fs::create_dir(&path).expect("a directory where the file should be");
        std::fs::write(path.join("keep-me"), b"x").expect("make it non-empty");

        let mut s = StateStore::load(path.clone());
        // The decision itself, not a consequence of it. A directory is
        // unreadable *and unwritable*, so `!flush()` passes here whatever
        // `blocked` says - a review measured that flipping this arm's
        // `blocked = true` to `false` reddened nothing in the workspace. The
        // field is in scope from this child module, so there is no reason to
        // borrow the assertion from a test about a different assignment.
        assert!(s.blocked, "a transient failure must refuse to write");

        s.record("GSM-5B09-A", 40, 1_700_000_000);
        let _ = s.flush(Instant::now());

        assert!(path.join("keep-me").exists(), "nothing was moved");
        assert!(
            !quarantine_path(&path).exists(),
            "and no quarantine copy for a failure that may clear on its own"
        );
    }

    /// A second corruption does **not** silently destroy the first quarantine.
    ///
    /// `fs::rename` replaces an existing destination *file* on every supported
    /// platform, so a user who corrupts the file twice would have lost their
    /// first copy with no log line and no test. The archive text even implied
    /// otherwise, calling an occupied name a reason the rename fails - true only
    /// when the occupant is a non-empty directory, which is not what a real
    /// second corruption leaves behind.
    #[test]
    fn a_second_quarantine_does_not_overwrite_the_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");

        std::fs::write(
            &path,
            "levels = \"first\"
",
        )
        .expect("first corruption");
        drop(StateStore::load(path.clone()));
        std::fs::write(
            &path,
            "levels = \"second\"
",
        )
        .expect("second corruption");
        drop(StateStore::load(path.clone()));

        assert_eq!(
            std::fs::read_to_string(quarantine_path(&path)).expect("the first is kept"),
            "levels = \"first\"
"
        );
    }

    /// When every quarantine name is taken, writing is refused.
    ///
    /// The narrow case the original refuse-to-write remedy was always right for:
    /// nothing can preserve the bytes, so overwriting them is the one avoidable
    /// harm left. A rename that fails for its own reasons - a read-only
    /// directory, a permission problem - reaches the same arm; this fixture
    /// exercises the *bound*, because that is the part `free_quarantine_path`
    /// added and the part a test can arrange portably.
    ///
    /// The fixture has to be an **unparseable file**, not a directory at the
    /// path. A directory produces an `Io` error whose kind is not `InvalidData`,
    /// so it takes the transient arm and never reaches a rename at all - a
    /// version of this test built that way passed through the wrong branch
    /// entirely and proved nothing. It was built that way for one revision.
    #[test]
    fn a_file_that_cannot_be_moved_aside_is_never_written_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        std::fs::write(
            &path,
            "levels = \"not a table\"
",
        )
        .expect("an unparseable file");

        // Occupy every candidate `free_quarantine_path` will consider.
        let base = quarantine_path(&path);
        std::fs::write(&base, b"an earlier corruption").expect("the first name");
        for n in 1_u32..100 {
            let mut name = base.file_name().unwrap_or_default().to_os_string();
            name.push(format!(".{n}"));
            std::fs::write(base.with_file_name(name), b"and another").expect("a later name");
        }

        let mut s = StateStore::load(path.clone());
        s.record("GSM-5B09-A", 40, 1_700_000_000);

        let base_now = Instant::now();
        assert!(!s.maybe_flush(base_now), "the debounced write must refuse");
        assert!(!s.flush(at(base_now, 60)), "and so must the shutdown flush");
        assert_eq!(
            std::fs::read_to_string(&path).expect("still there"),
            "levels = \"not a table\"
",
            "the user's file is untouched"
        );
        assert_eq!(
            std::fs::read_to_string(&base).expect("still there"),
            "an earlier corruption",
            "and so is the quarantine that was already there"
        );
    }

    /// The direction that matters just as much: a **missing** file is not an
    /// unreadable one, and refusing to write it would be a different bug - every
    /// first run has no state file, so a store that latched on absence would
    /// never persist anything for anybody.
    #[test]
    fn a_missing_state_file_still_writes_normally() {
        let (mut s, _dir) = store();
        s.record("GSM-5B09-A", 40, 1_700_000_000);
        assert!(s.flush(Instant::now()), "a first run must persist");
    }

    /// And an existing, readable file is loaded and written back as before.
    #[test]
    fn a_readable_state_file_is_loaded_and_still_writable() {
        let (mut s, path, _dir) = store_over(
            "[levels.\"GSM-5B09-A\"]\nuser_level_pct = 55\nupdated_at_unix = 1700000000\n",
        );
        assert_eq!(s.level("GSM-5B09-A"), Some(55));

        s.record("GSM-5B09-A", 70, 1_700_000_001);
        assert!(s.flush(Instant::now()));
        // `contains("70")` would be vacuous - the fixture's own
        // `updated_at_unix = 1700000000` already contains it, and a `write` that
        // touched no disk at all still passed against that. Match the field.
        assert!(
            std::fs::read_to_string(&path)
                .expect("written")
                .contains("user_level_pct = 70")
        );
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

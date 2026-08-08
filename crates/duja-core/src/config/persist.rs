//! Crash-safe filesystem persistence — the only filesystem I/O in `duja-core`.
//!
//! Writes go through a temporary file in the *same directory* as the target,
//! which is flushed, fsynced, and then atomically renamed over the target. A
//! crash at any point leaves the target either fully intact (old contents) or
//! fully replaced — never half-written. On Unix the parent directory is fsynced
//! after the rename so the rename itself is durable across a power loss.
//!
//! Reads treat a missing file as `Ok(None)` (a normal first-run condition, not
//! an error); every other failure becomes a typed [`ConfigError`].

use std::fs;
use std::io::Write as _;
use std::path::Path;

use crate::config::error::ConfigError;

/// The largest `config.toml` or `state.toml` this will read.
///
/// One MiB, matching [`crate::quirks::MAX_QUIRKS_LEN`] — the same class of file
/// and the same reason. `SECURITY.md`'s threat model says of these files "typed
/// parsing only, **size caps**, no user-supplied regex"; the quirk DB had one
/// and these did not, so P8 wave 5 made the claim true rather than editing it
/// out. The largest config a real user reaches is a few kilobytes: the schema is
/// fixed and the only unbounded part is one table keyed by display id.
///
/// It is a cap on *this process's own* allocation, not an access control. A
/// process that can write here is already the user, and could do worse; what it
/// stops is a config file - corrupted, or grown by some other bug - turning a
/// launch into an out-of-memory abort with no log, which is a shape a tray app
/// cannot recover from because the recovery path is the thing that failed.
///
/// Two things in the file are unbounded by the schema, so "it is fixed" would be
/// the wrong reason to think a real config stays small: the `[monitors]` table is
/// keyed by display id, and `[hotkeys]` is a free-form action-to-chord map.
/// Neither is bounded, and the document layer is format-preserving, so unknown
/// keys and comments survive a load-save round trip too. The argument for 1 MiB
/// is arithmetic rather than structural: a fully-populated monitor block runs
/// 250 to 400 bytes, which puts the cap somewhere past three thousand displays.
pub const MAX_CONFIG_LEN: usize = 1024 * 1024;

/// One byte past the cap: what the bounded read is allowed to pull.
///
/// Named rather than written as `MAX_CONFIG_LEN + 1` at the call site, because
/// `arithmetic_side_effects` is a workspace lint that CI promotes to an error
/// (`-D warnings`), and a `saturating_add` in an expression whose whole purpose
/// is "exactly one more" reads worse than a constant that says so. The extra
/// byte is what makes a file of exactly the cap distinguishable from a truncated
/// larger one.
const READ_LIMIT: u64 = MAX_CONFIG_LEN as u64 + 1;

/// Read `path` to a string, mapping a missing file to `Ok(None)`.
///
/// Refuses anything over [`MAX_CONFIG_LEN`]. The metadata length is a cheap
/// pre-check; `read_capped` is the enforcement, because metadata can be wrong
/// in both directions - a file can grow between the two calls, and Linux
/// reports a length of zero for `/proc` entries that have content. A
/// metadata-only cap is one a symlink walks straight past.
///
/// # What the callers do with an error
///
/// Not one thing, and the first version of this comment claimed otherwise.
/// `ConfigDocument::load` and `StateFile::load` **propagate**; it is *their*
/// callers that vary. `state_store::load` and `tray::load_config` log and fall
/// back to defaults. `tray/state.rs`'s reload keeps the in-memory copy. And
/// `settings_apply::persist_config_change` propagates, so an over-cap config
/// makes a settings write from the tray fail rather than silently reset the
/// user's file - which is the right answer there, and worth knowing is the
/// answer.
///
/// # Errors
/// - [`ConfigError::TooLarge`] if the file exceeds the cap.
/// - [`ConfigError::Io`] for any other failure than the file not existing.
pub fn read_to_string_opt(path: &Path) -> Result<Option<String>, ConfigError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(ConfigError::Io(err)),
    };
    if let Ok(meta) = file.metadata()
        && meta.len() > MAX_CONFIG_LEN as u64
    {
        return Err(ConfigError::TooLarge {
            at_least: meta.len(),
            max: MAX_CONFIG_LEN,
        });
    }
    read_capped(file).map(Some)
}

/// Read at most [`MAX_CONFIG_LEN`] bytes of UTF-8 from `reader`.
///
/// Split out from [`read_to_string_opt`] so the enforcement is reachable by a
/// test. It was not, and that mattered: a review deleted this bound entirely and
/// the whole suite stayed green, because the metadata pre-check shadows it for
/// every ordinary file. The one shape that reaches it is the one no test can
/// create through a `&Path` - a reader whose metadata lied. (`updates.rs` splits
/// its own response cap the same way, for the same reason.)
///
/// Reads **bytes** and length-checks before the UTF-8 conversion, which is not
/// tidiness either. Cutting at `READ_LIMIT` can land mid-sequence, and
/// `Take::read_to_string` then fails with `InvalidData` - so the over-cap file
/// would have returned [`ConfigError::Io`], defeating the whole reason
/// [`ConfigError::TooLarge`] exists, in precisely the branch that is the
/// enforcement.
///
/// # Errors
/// - [`ConfigError::TooLarge`] if `reader` yields more than the cap.
/// - [`ConfigError::Io`] on a read failure or invalid UTF-8.
pub(crate) fn read_capped(reader: impl std::io::Read) -> Result<String, ConfigError> {
    use std::io::Read as _;

    let mut bytes = Vec::new();
    reader
        .take(READ_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(ConfigError::Io)?;
    if bytes.len() > MAX_CONFIG_LEN {
        return Err(ConfigError::TooLarge {
            at_least: READ_LIMIT,
            max: MAX_CONFIG_LEN,
        });
    }
    String::from_utf8(bytes)
        .map_err(|e| ConfigError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

/// Atomically write `contents` to `path`, creating parent directories as needed.
///
/// The write is durable and crash-safe: it lands in a same-directory temporary
/// file that is flushed and fsynced before an atomic rename replaces `path`.
///
/// # The cap is on this side too, and it was not
///
/// [`read_to_string_opt`] refuses anything over [`MAX_CONFIG_LEN`] and this
/// function used to refuse nothing, so **Duja could write a file it would
/// subsequently refuse to read**. `[monitors]` and `[hotkeys]` are both
/// unbounded by the schema and the document layer is format-preserving, so
/// unknown keys and comments accumulate across a load-save round trip as well.
///
/// Refusing here rather than at the next read is not merely earlier, which is
/// what a first draft of `docs/debt-archive.md` D-113 assumed. It is the
/// difference between a failure the *acting* user sees and one that surfaces at
/// some later launch to whoever is sitting there - and, for `state.toml`,
/// between a write that is refused and one that lands and is then refused *on
/// the way back in*, leaving the user's levels to be replaced by defaults.
/// ("Read as garbage" is what D-113 says and it is not what happens: an
/// over-cap file is not read at all.) `duja-app`'s `state_store` carries that
/// other half.
///
/// # Errors
/// - [`ConfigError::TooLarge`] if `contents` exceeds [`MAX_CONFIG_LEN`], before
///   anything is created or written.
/// - [`ConfigError::Io`] if the directory cannot be created, the temporary file
///   cannot be written or fsynced, or the rename fails.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), ConfigError> {
    // Before `create_dir_all`, deliberately: a refused write must leave the
    // filesystem exactly as it found it, including not creating a config
    // directory for a document that is never going to land in it.
    if contents.len() > MAX_CONFIG_LEN {
        return Err(ConfigError::TooLarge {
            at_least: contents.len() as u64,
            max: MAX_CONFIG_LEN,
        });
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(dir) = parent {
        fs::create_dir_all(dir).map_err(ConfigError::Io)?;
    }
    let dir = parent.unwrap_or_else(|| Path::new("."));

    // Temp file in the *same directory* so the final rename stays on one
    // filesystem and is therefore atomic.
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(ConfigError::Io)?;
    tmp.write_all(contents.as_bytes())
        .map_err(ConfigError::Io)?;
    tmp.flush().map_err(ConfigError::Io)?;
    tmp.as_file().sync_all().map_err(ConfigError::Io)?;

    // Atomic rename over the target. `persist` maps to a replacing rename on
    // every supported platform.
    tmp.persist(path)
        .map_err(|err| ConfigError::Io(err.error))?;

    #[cfg(unix)]
    if let Some(dir) = parent {
        // Best-effort directory fsync so the rename survives a crash. The data
        // is already durable via the file fsync above, and some filesystems do
        // not support directory fsync, so a failure here is not fatal.
        if let Ok(dir_file) = fs::File::open(dir) {
            drop(dir_file.sync_all());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document over the cap is refused **before** anything reaches the disk.
    ///
    /// D-113's headline: the read side capped and the write side did not, so
    /// Duja could write a file it would subsequently refuse to read. Goes red
    /// against the pre-fix `write_atomic`, which wrote the file and returned
    /// `Ok(())` - and then `read_to_string_opt` on the same path returned
    /// `TooLarge`, which is the round trip this asserts cannot happen.
    #[test]
    fn a_document_over_the_cap_is_refused_rather_than_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let oversized = "x".repeat(MAX_CONFIG_LEN + 1);

        match write_atomic(&path, &oversized) {
            Err(ConfigError::TooLarge { at_least, max }) => {
                assert_eq!(at_least, oversized.len() as u64);
                assert_eq!(max, MAX_CONFIG_LEN);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
        assert!(
            !path.exists(),
            "a refused write must leave nothing behind, not a partial file"
        );
    }

    /// The write cap and the read cap agree at the boundary.
    ///
    /// Two caps written as two comparisons is two places for an off-by-one, and
    /// the failure it would produce is exactly the one this row is about: a
    /// document that writes and then will not read. Exactly the cap is legal on
    /// both sides.
    #[test]
    fn a_document_of_exactly_the_cap_survives_the_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let at_the_line = "x".repeat(MAX_CONFIG_LEN);

        write_atomic(&path, &at_the_line).expect("exactly the cap is within it");
        let read_back = read_to_string_opt(&path).expect("and reads back");
        assert_eq!(read_back.as_deref(), Some(at_the_line.as_str()));
    }

    /// A refused write does not create the config directory either.
    ///
    /// `write_atomic` does `create_dir_all` before it writes, so a cap check
    /// placed after it would leave an empty `%APPDATA%\Duja\` behind on a first
    /// run that failed - which reads to the next launch, and to a user looking
    /// at their filesystem, as though Duja had got further than it did.
    #[test]
    fn a_refused_write_does_not_create_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("Duja").join("config.toml");

        let refused = write_atomic(&nested, &"x".repeat(MAX_CONFIG_LEN + 1));

        assert!(matches!(refused, Err(ConfigError::TooLarge { .. })));
        assert!(
            !dir.path().join("Duja").exists(),
            "the parent directory must not have been created"
        );
    }

    /// `SECURITY.md` claims "typed parsing only, **size caps**" for these files,
    /// and until P8 wave 5 there was no cap: `read_to_string` allocated whatever
    /// the file was. A claim in a security policy that the code does not keep is
    /// the serious kind of wrong.
    #[test]
    fn a_file_over_the_cap_is_refused_rather_than_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "x".repeat(MAX_CONFIG_LEN + 1)).expect("write");
        match read_to_string_opt(&path) {
            Err(ConfigError::TooLarge { at_least, max }) => {
                assert_eq!(at_least, MAX_CONFIG_LEN as u64 + 1);
                assert_eq!(max, MAX_CONFIG_LEN);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// The enforcement, driven directly - because through a `&Path` it is
    /// unreachable: the metadata pre-check answers first for every ordinary
    /// file. A review deleted this bound and the whole suite stayed green.
    #[test]
    fn the_bounded_read_refuses_a_reader_whose_metadata_would_have_lied() {
        let over = vec![b'z'; MAX_CONFIG_LEN + 1];
        match read_capped(over.as_slice()) {
            Err(ConfigError::TooLarge { at_least, max }) => {
                assert_eq!(at_least, MAX_CONFIG_LEN as u64 + 1);
                assert_eq!(max, MAX_CONFIG_LEN);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// And it must still be `TooLarge` when the cut lands mid-UTF-8.
    ///
    /// This is the case that made the first version return `Io(InvalidData)`:
    /// `Take::read_to_string` validates as it goes, so a multi-byte character
    /// straddling `READ_LIMIT` failed as invalid UTF-8 rather than as an
    /// oversized file - defeating the whole reason `TooLarge` exists, in the one
    /// branch that is the enforcement. Reading bytes and checking the length
    /// before converting is what fixes it.
    #[test]
    fn an_over_cap_reader_is_too_large_even_when_it_is_cut_mid_character() {
        let mut over = vec![b'z'; MAX_CONFIG_LEN - 1];
        // A three-byte euro sign straddling the limit.
        over.extend_from_slice("\u{20ac}".as_bytes());
        over.extend_from_slice(&[b'z'; 16]);
        assert!(over.len() > MAX_CONFIG_LEN);
        match read_capped(over.as_slice()) {
            Err(ConfigError::TooLarge { .. }) => {}
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// Genuinely invalid UTF-8 under the cap is still an I/O error, so the fix
    /// above did not turn every decode failure into a size complaint.
    #[test]
    fn invalid_utf8_under_the_cap_is_still_an_io_error() {
        match read_capped([0xff_u8, 0xfe, 0xfd].as_slice()) {
            Err(ConfigError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
            }
            other => panic!("expected Io(InvalidData), got {other:?}"),
        }
    }

    /// And the boundary is a cap, not a fence one byte inside it: a file of
    /// exactly `MAX_CONFIG_LEN` must still load. Off-by-one here would reject a
    /// legitimate config with a message about a limit it did not reach.
    #[test]
    fn a_file_of_exactly_the_cap_still_reads_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let body = "y".repeat(MAX_CONFIG_LEN);
        std::fs::write(&path, &body).expect("write");
        let read = read_to_string_opt(&path)
            .expect("at the cap")
            .expect("some");
        assert_eq!(read.len(), MAX_CONFIG_LEN);
        assert_eq!(read, body, "the read was truncated rather than complete");
    }

    #[test]
    fn reading_a_missing_file_is_none_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.toml");
        assert_eq!(read_to_string_opt(&missing).expect("no error"), None);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        write_atomic(&path, "schema_version = 1\n").expect("write");
        assert_eq!(
            read_to_string_opt(&path).expect("read"),
            Some("schema_version = 1\n".to_owned())
        );
    }

    #[test]
    fn write_atomic_replaces_existing_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        write_atomic(&path, "first = 1\n").expect("first write");
        write_atomic(&path, "second = 2\n").expect("second write");
        assert_eq!(
            read_to_string_opt(&path).expect("read"),
            Some("second = 2\n".to_owned())
        );
    }

    #[test]
    fn write_atomic_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("config.toml");
        write_atomic(&nested, "ok = true\n").expect("write into new dirs");
        assert_eq!(
            read_to_string_opt(&nested).expect("read"),
            Some("ok = true\n".to_owned())
        );
    }

    #[test]
    fn atomic_write_crash_simulation_leaves_old_file_intact() {
        // Model a crash *after* the new contents were written to a same-dir
        // temp file but *before* the rename: the temp file exists, yet the
        // target still holds the old, complete config.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        write_atomic(&path, "schema_version = 1\nkeep = true\n").expect("commit v1");

        // Interrupted write of a new version: temp file created and flushed,
        // then the process "crashes" before persist() renames it.
        let mut interrupted = tempfile::NamedTempFile::new_in(dir.path()).expect("temp");
        interrupted
            .write_all(b"schema_version = 2\nkeep = false\n")
            .expect("write temp");
        interrupted.flush().expect("flush temp");
        // Deliberately drop without persist() — simulating the crash.
        drop(interrupted);

        // The committed config is untouched.
        assert_eq!(
            read_to_string_opt(&path).expect("read"),
            Some("schema_version = 1\nkeep = true\n".to_owned())
        );
    }
}

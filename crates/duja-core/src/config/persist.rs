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

/// Read `path` to a string, mapping a missing file to `Ok(None)`.
///
/// # Errors
/// [`ConfigError::Io`] for any failure other than the file not existing.
pub fn read_to_string_opt(path: &Path) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ConfigError::Io(err)),
    }
}

/// Atomically write `contents` to `path`, creating parent directories as needed.
///
/// The write is durable and crash-safe: it lands in a same-directory temporary
/// file that is flushed and fsynced before an atomic rename replaces `path`.
///
/// # Errors
/// [`ConfigError::Io`] if the directory cannot be created, the temporary file
/// cannot be written or fsynced, or the rename fails.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), ConfigError> {
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

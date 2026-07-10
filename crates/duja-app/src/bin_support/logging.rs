//! Process logging: a WARN rotating file log by default, DEBUG to stderr under
//! `--verbose`.
//!
//! `tracing` is the facade; the default sink is a size-rotated file under the
//! data dir (`logs/duja.log`, 3 × 5 MB). `tracing-appender` only rotates on a
//! time schedule, so this module carries a tiny size-based rotator instead
//! (rename `duja.log` → `duja.log.1` → `duja.log.2`, drop the oldest). The
//! rotation *decision* is a pure, unit-tested helper; the file plumbing is
//! best-effort (a logging failure never takes down the app).
//!
//! Levels honour `RUST_LOG` when set, else default to WARN (file) / DEBUG
//! (`--verbose`, stderr). Callers log stable ids only — never raw EDID bytes.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::EnvFilter;

/// The rotating log file's base name.
const LOG_FILE: &str = "duja.log";
/// Per-file size cap before rotation (5 MB).
const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Total files kept (the live file plus two rotated generations).
const MAX_FILES: usize = 3;

/// Install the global tracing subscriber.
///
/// `--verbose` routes DEBUG to stderr; otherwise WARN goes to the rotating file
/// under `log_dir`. If `log_dir` is `None` (no resolvable data dir) the default
/// path falls back to WARN-on-stderr so logs are never silently dropped.
///
/// Idempotent-ish: `tracing` allows the global subscriber to be set once; a
/// second call is a no-op (the error is swallowed).
pub(crate) fn init(log_dir: Option<&Path>, verbose: bool) {
    if verbose {
        let filter = env_filter("debug");
        let _ = tracing_subscriber::fmt()
            .with_writer(io::stderr)
            .with_env_filter(filter)
            .with_ansi(false)
            .try_init();
        return;
    }

    if let Some(dir) = log_dir {
        let _ = fs::create_dir_all(dir);
        let writer = RotatingWriter::new(dir.to_path_buf(), LOG_FILE, MAX_BYTES, MAX_FILES);
        let _ = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_env_filter(env_filter("warn"))
            .with_ansi(false)
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_writer(io::stderr)
            .with_env_filter(env_filter("warn"))
            .with_ansi(false)
            .try_init();
    }
}

/// Build an [`EnvFilter`] honouring `RUST_LOG`, defaulting to `default_level`.
fn env_filter(default_level: &str) -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(default_level.parse().unwrap_or_default())
        .from_env_lossy()
}

/// Whether a write of `incoming` bytes to a file already `current` bytes long
/// should trigger a rotation first. A brand-new (empty) file never rotates, so a
/// single oversized record still lands rather than rotating an empty file.
fn should_rotate(current: u64, incoming: usize, max_bytes: u64) -> bool {
    current > 0 && current.saturating_add(incoming as u64) > max_bytes
}

/// A cheap, clonable handle to a size-rotated log file.
#[derive(Clone)]
struct RotatingWriter {
    inner: Arc<Mutex<Rotator>>,
}

impl RotatingWriter {
    fn new(dir: PathBuf, base: &str, max_bytes: u64, max_files: usize) -> Self {
        RotatingWriter {
            inner: Arc::new(Mutex::new(Rotator {
                dir,
                base: base.to_owned(),
                max_bytes,
                max_files,
            })),
        }
    }
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Recover a poisoned lock rather than unwrapping: a logging mutex is
        // never a correctness-critical section.
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The rotator's shared state: where the files live and the size policy.
struct Rotator {
    dir: PathBuf,
    base: String,
    max_bytes: u64,
    max_files: usize,
}

impl Rotator {
    /// The live log file path.
    fn base_path(&self) -> PathBuf {
        self.dir.join(&self.base)
    }

    /// The path of rotated generation `n` (`duja.log.1`, `duja.log.2`, …).
    fn nth_path(&self, n: usize) -> PathBuf {
        self.dir.join(format!("{}.{n}", self.base))
    }

    /// Append `buf` to the live file, rotating first if it would exceed the cap.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let path = self.base_path();
        let current = fs::metadata(&path).map_or(0, |m| m.len());
        if should_rotate(current, buf.len(), self.max_bytes) {
            self.rotate();
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(buf)?;
        Ok(buf.len())
    }

    /// Shift the generations down and free the live path for a fresh file.
    ///
    /// `duja.log.(N-2)` → `duja.log.(N-1)` … `duja.log` → `duja.log.1`. The
    /// oldest generation is overwritten by the rename. Best-effort: a failed
    /// rename just means that generation is skipped.
    fn rotate(&self) {
        for k in (1..self.max_files).rev() {
            let from = if k == 1 {
                self.base_path()
            } else {
                self.nth_path(k.saturating_sub(1))
            };
            let _ = fs::rename(from, self.nth_path(k));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_never_rotates() {
        assert!(!should_rotate(0, 10_000, MAX_BYTES));
    }

    #[test]
    fn rotates_only_when_the_write_would_overflow() {
        assert!(!should_rotate(100, 100, 1000));
        assert!(!should_rotate(900, 100, 1000)); // exactly at the cap, still fits
        assert!(should_rotate(901, 100, 1000)); // one over
    }

    #[test]
    fn write_creates_and_appends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut w = RotatingWriter::new(dir.path().to_path_buf(), "duja.log", MAX_BYTES, 3);
        assert_eq!(w.write(b"hello ").expect("write"), 6);
        assert_eq!(w.write(b"world").expect("write"), 5);
        let contents = fs::read_to_string(dir.path().join("duja.log")).expect("read");
        assert_eq!(contents, "hello world");
    }

    #[test]
    fn rotation_shifts_generations_and_caps_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 20-byte cap so each ~10-byte line rotates the previous one.
        let mut w = RotatingWriter::new(dir.path().to_path_buf(), "duja.log", 20, 3);
        w.write_all(b"AAAAAAAAAA").expect("write a"); // fills duja.log
        w.write_all(b"BBBBBBBBBB1234").expect("write b"); // 10+14>20 -> rotate, log.1=A
        w.write_all(b"CCCCCCCCCC5678").expect("write c"); // rotate again -> log.2=B, log.1=C

        assert!(dir.path().join("duja.log").exists());
        assert!(dir.path().join("duja.log.1").exists());
        assert!(dir.path().join("duja.log.2").exists());
        // Never a 3rd rotated generation (MAX_FILES = 3).
        assert!(!dir.path().join("duja.log.3").exists());
        // The oldest surviving generation holds the first line.
        let oldest = fs::read_to_string(dir.path().join("duja.log.2")).expect("read");
        assert_eq!(oldest, "AAAAAAAAAA");
    }
}

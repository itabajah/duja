//! Process logging: a WARN rotating file log by default, DEBUG to stderr under
//! `--verbose`.
//!
//! `tracing` is the facade; the default sink is a size-rotated file under the
//! data dir (`logs/duja.log`, 3 × 5 MB). The `tracing-appender` crate only
//! rotates on a time schedule, so this module carries its own size-based rotator
//! (rename `duja.log` → `duja.log.1` → `duja.log.2`, drop the oldest). The
//! rotation *decision* is a pure, unit-tested helper; the file plumbing is
//! best-effort (a logging failure never takes down the app).
//!
//! Levels honour `RUST_LOG` when set, else default to WARN (file) / DEBUG
//! (`--verbose`, stderr). Callers log stable ids only — never raw EDID bytes.
//!
//! # Why [`Targets`] and not `EnvFilter`
//!
//! `EnvFilter` is the usual choice and it is the one this module used until P8.
//! It parses a strictly larger grammar — span-field predicates like
//! `target[span{field=value}]=level` — and it pays for that with a regex engine:
//! turning the `env-filter` feature off drops `regex-automata`, `regex-syntax`
//! and `matchers`, which measured 345 KiB of `.text` and 664,064 bytes of file
//! in a binary that was over its budget. See
//! <https://github.com/itabajah/duja/blob/main/docs/adr/0012-binary-size-budget-variance.md>.
//!
//! [`Targets`] parses the part of the grammar Duja actually uses: a
//! comma-separated list of `target=level`, or a bare `level`. Swapping the two
//! is **not** transparent, and the three differences are worth knowing before
//! debugging a filter that did not do what you expected. Two of them are
//! repaired here; one is not, because it cannot be.
//!
//! - **Repaired: whitespace.** `EnvFilter` trims every directive.
//!   [`Targets`] does not, and — this is the sharp edge — an unrecognised token
//!   does not become an *error*, it becomes a **target name at TRACE**. So
//!   `RUST_LOG="duja_app=debug, warn"` would silently read `" warn"` as a module
//!   to log at TRACE and leave everything else off, and `RUST_LOG="warn "` would
//!   turn logging off entirely. [`level_filter`] trims each segment first.
//! - **Repaired: empty segments.** `LevelFilter::from_str` maps `""` to
//!   `ERROR`, so a bare `RUST_LOG=` (set, empty) parses *successfully* as a
//!   global `error` directive and silences the WARN file log — no fallback would
//!   fire, because nothing failed. `EnvFilter` dropped empty directives and fell
//!   back. [`level_filter`] drops them too, so `RUST_LOG=` and `RUST_LOG=a,,b`
//!   behave as they did.
//! - **Not repaired: rejection is all-or-nothing.** Given
//!   `RUST_LOG=duja_app=nonsense,debug`, `EnvFilter::from_env_lossy` drops the
//!   unparseable directive and honours `debug`; [`level_filter`] discards the
//!   whole string and uses `default_level`. Restoring that would mean
//!   re-implementing per-directive validation, and the failure it prevents —
//!   logging at a level nobody asked for — is the one worth defaulting to safe.
//!
//! What is **not** a difference: a bare unknown token like `RUST_LOG=nonsense`
//! means "the target `nonsense` at TRACE" under both, which is why the trimming
//! above matters so much.

use std::backtrace::Backtrace;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// The rotating log file's base name.
const LOG_FILE: &str = "duja.log";
/// The crash-record file's base name (written by the panic hook).
pub(crate) const CRASH_FILE: &str = "duja-crash.log";
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
    // Read the environment once: three call sites asking separately would be
    // three chances for them to disagree.
    let rust_log = std::env::var("RUST_LOG").ok();
    let spec = rust_log.as_deref();

    if verbose {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(false)
            .with_filter(level_filter(spec, LevelFilter::DEBUG));
        let _ = tracing_subscriber::registry().with(layer).try_init();
        return;
    }

    if let Some(dir) = log_dir {
        let _ = fs::create_dir_all(dir);
        let writer = RotatingWriter::new(dir.to_path_buf(), LOG_FILE, MAX_BYTES, MAX_FILES);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_filter(level_filter(spec, LevelFilter::WARN));
        let _ = tracing_subscriber::registry().with(layer).try_init();
    } else {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(false)
            .with_filter(level_filter(spec, LevelFilter::WARN));
        let _ = tracing_subscriber::registry().with(layer).try_init();
    }
}

/// Install a panic hook that writes a crash record to disk **synchronously**
/// before the process tears down.
///
/// A panic inside a Slint/FFI callback unwinds into `extern "C"` and aborts
/// (`0xe06d7363` → `0xc0000409`); the default hook only prints to stderr, which a
/// `windows_subsystem = "windows"` release binary does not have — so the live-QA
/// crash left **zero** diagnostics. This hook runs at panic time (before the
/// abort) and writes the thread, panic message, location and a backtrace with
/// plain [`std::fs`] (no buffering, an explicit flush), so the next field crash
/// is recoverable from `crash_log`. It chains to the previous hook afterwards.
///
/// `crash_log` is `None` for console/`--verbose` modes (stderr is live there).
pub(crate) fn install_panic_hook(crash_log: Option<PathBuf>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = crash_log.as_deref() {
            let location = info.location().map(ToString::to_string);
            let record = format_crash_record(
                std::thread::current().name(),
                &panic_message(info),
                location.as_deref(),
            );
            let _ = write_crash_record(path, &record);
        }
        previous(info);
    }));
}

/// Extract a human-readable message from a panic payload.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

/// Format a crash record: a timestamped block with the thread, location, message
/// and a captured backtrace. Pure (backtrace aside) so it is unit-testable.
fn format_crash_record(thread: Option<&str>, message: &str, location: Option<&str>) -> String {
    let unix_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let backtrace = Backtrace::force_capture();
    format!(
        "--- duja crash ---\nunix_time={unix_time}\nthread={}\nlocation={}\nmessage={message}\nbacktrace:\n{backtrace}\n",
        thread.unwrap_or("unknown"),
        location.unwrap_or("unknown"),
    )
}

/// Append `record` to `path` synchronously (creating the parent dir), flushing
/// before returning. Best-effort — a logging failure never matters more than the
/// crash itself.
fn write_crash_record(path: &Path, record: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(record.as_bytes())?;
    file.flush()
}

/// Resolve `RUST_LOG` (already read, so this stays pure) into a [`Targets`]
/// filter, falling back to `default_level` for everything.
///
/// `rust_log` is `None` when the variable is unset. The normalization below is
/// not cosmetic — it is what keeps two `EnvFilter` behaviours that [`Targets`]
/// silently drops, and the module header says what each one costs without it.
///
/// Note what this does **not** do: a spec that parses is used *as it stands*,
/// with no default merged into it. `RUST_LOG=duja_app=debug` therefore silences
/// every other target, which is what `EnvFilter` did too — its default directive
/// applied only when the variable was empty or unset.
fn level_filter(rust_log: Option<&str>, default_level: LevelFilter) -> Targets {
    let fallback = Targets::new().with_default(default_level);
    let Some(spec) = rust_log else {
        return fallback;
    };
    // Trim each directive and drop the empty ones, both of which `EnvFilter` did
    // and `Targets` does not. Rejoined rather than parsed piecewise so that
    // `Targets`'s own parser stays the only thing that decides what a directive
    // means.
    let normalized = spec
        .split(',')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    if normalized.is_empty() {
        return fallback;
    }
    normalized.parse::<Targets>().unwrap_or(fallback)
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

    use tracing::Level;

    #[test]
    fn unset_rust_log_applies_the_default_level_to_everything() {
        let filter = level_filter(None, LevelFilter::WARN);
        assert!(filter.would_enable("duja_app::tray", &Level::WARN));
        assert!(filter.would_enable("some_dependency", &Level::ERROR));
        assert!(!filter.would_enable("duja_app::tray", &Level::INFO));
    }

    #[test]
    fn rust_log_overrides_the_default_level() {
        let filter = level_filter(Some("debug"), LevelFilter::WARN);
        assert!(filter.would_enable("duja_app::tray", &Level::DEBUG));
    }

    #[test]
    fn rust_log_scopes_a_level_to_one_target() {
        let filter = level_filter(Some("duja_app=debug,warn"), LevelFilter::WARN);
        assert!(filter.would_enable("duja_app::tray", &Level::DEBUG));
        // The trailing bare `warn` is the default for everything else, so a
        // chatty dependency stays quiet.
        assert!(!filter.would_enable("i_slint_core", &Level::DEBUG));
        assert!(filter.would_enable("i_slint_core", &Level::WARN));
    }

    /// The one behaviour that still differs from the `EnvFilter` this replaced,
    /// pinned rather than left to be rediscovered.
    ///
    /// `duja_app=nonsense` is an unparseable **level**, which is the case that
    /// separates the two: `EnvFilter::from_env_lossy` drops that directive and
    /// honours the trailing `debug`; [`level_filter`] discards the whole string
    /// and the default stands. (An earlier version of this test used
    /// `[worker{id=3}]=trace` and claimed `EnvFilter` "would drop it" — wrong on
    /// both counts. `EnvFilter` *parses* span-field predicates, that being the
    /// grammar it exists for, and what makes `Targets` reject that string is
    /// simply the second `=`. It demonstrated the grammar difference while
    /// claiming to demonstrate the leniency one.)
    #[test]
    fn an_unparseable_level_falls_back_rather_than_dropping_one_directive() {
        let filter = level_filter(Some("duja_app=nonsense,debug"), LevelFilter::WARN);
        assert!(!filter.would_enable("duja_app::tray", &Level::DEBUG));
        assert!(filter.would_enable("duja_app::tray", &Level::WARN));
    }

    /// The span-field grammar `Targets` does not have. Separate from the test
    /// above because it fails for a different reason and would otherwise be
    /// covered only by accident.
    #[test]
    fn a_span_field_predicate_is_not_supported_and_falls_back() {
        let filter = level_filter(Some("[worker{id=3}]=trace"), LevelFilter::WARN);
        assert!(!filter.would_enable("duja_app::tray", &Level::TRACE));
        assert!(filter.would_enable("duja_app::tray", &Level::WARN));
    }

    /// `RUST_LOG=` set-but-empty must not silence the log.
    ///
    /// This is the regression the normalization exists for, and it is invisible
    /// without it: `LevelFilter::from_str("")` is `Ok(ERROR)`, so a bare
    /// `RUST_LOG=` parses **successfully** into a global `error` directive and
    /// the WARN file log goes quiet. No fallback fires, because nothing failed.
    /// `EnvFilter` dropped empty directives and fell back to the default.
    /// `RUST_LOG= duja` is an ordinary thing to type.
    #[test]
    fn an_empty_rust_log_is_the_same_as_an_unset_one() {
        for spec in ["", "   ", ",", " , "] {
            let filter = level_filter(Some(spec), LevelFilter::WARN);
            assert!(
                filter.would_enable("duja_app::tray", &Level::WARN),
                "RUST_LOG={spec:?} silenced the WARN log"
            );
        }
    }

    /// Whitespace around a directive must not turn a level into a target.
    ///
    /// The other half of what the normalization repairs, and the more dangerous
    /// half: `Targets` does not trim, and an unrecognised token becomes a
    /// **target name at TRACE** rather than an error. Untrimmed,
    /// `"duja_app=debug, warn"` reads `" warn"` as a module to log at TRACE and
    /// turns everything else off, and `"warn "` disables logging entirely -
    /// both silently, and both shapes a person types by hand.
    #[test]
    fn whitespace_around_a_directive_does_not_change_what_it_means() {
        let spaced = level_filter(Some("duja_app=debug, warn"), LevelFilter::ERROR);
        assert!(spaced.would_enable("duja_app::tray", &Level::DEBUG));
        assert!(spaced.would_enable("i_slint_core", &Level::WARN));
        assert!(!spaced.would_enable("i_slint_core", &Level::DEBUG));

        let trailing = level_filter(Some("warn "), LevelFilter::ERROR);
        assert!(trailing.would_enable("anything", &Level::WARN));
    }

    /// A spec that parses is used as it stands, with no default merged in.
    ///
    /// Written because every other test here passes if [`level_filter`] ends
    /// with `.map(|t| t.with_default(default_level))` - a materially different
    /// filter, and one that would make `RUST_LOG=duja_app=debug` keep logging
    /// every dependency at WARN. `EnvFilter`'s default directive applied only
    /// when the variable was empty or unset, and so does this.
    #[test]
    fn a_parsed_spec_does_not_inherit_the_default_level() {
        let filter = level_filter(Some("duja_app=debug"), LevelFilter::WARN);
        assert!(filter.would_enable("duja_app::tray", &Level::DEBUG));
        assert!(
            !filter.would_enable("i_slint_core", &Level::WARN),
            "the default level leaked into a spec that parsed"
        );
    }

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
    fn crash_record_is_written_synchronously_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A nested path proves the parent dir is created.
        let path = dir.path().join("logs").join(CRASH_FILE);
        let record = format_crash_record(Some("duja-main"), "boom happened", Some("tray.rs:1:2"));
        write_crash_record(&path, &record).expect("write");
        let contents = fs::read_to_string(&path).expect("read");
        assert!(contents.contains("message=boom happened"), "{contents}");
        assert!(contents.contains("thread=duja-main"), "{contents}");
        assert!(contents.contains("location=tray.rs:1:2"), "{contents}");
        assert!(contents.contains("backtrace:"), "{contents}");
    }

    #[test]
    fn panic_hook_leaves_a_crash_record_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CRASH_FILE);
        // Take the current hook and put it back at the end, so this test does not
        // leak its own hook into a sibling test sharing the process. `take_hook`
        // leaves the default installed, which is what `install_panic_hook` then
        // captures and chains to.
        //
        // This used to `set_hook(saved)` immediately (a no-op round trip) and end
        // with a bare `take_hook()`, which restores the *default* rather than
        // whatever was there — so the comment promised an isolation the code did
        // not provide. Still latent: nothing else in this binary installs a hook,
        // and `tests/engine.rs`, which now does, compiles into a different test
        // binary (`duja-app::engine` vs `duja-app::bin/duja`) and so a different
        // process. Fixed because the code should do what its comment says, not
        // because anything is currently broken by it.
        let saved = std::panic::take_hook();
        install_panic_hook(Some(path.clone()));
        let result = std::panic::catch_unwind(|| panic!("simulated field crash"));
        std::panic::set_hook(saved);

        assert!(result.is_err());
        let contents = fs::read_to_string(&path).expect("crash record must exist");
        assert!(contents.contains("simulated field crash"), "{contents}");
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

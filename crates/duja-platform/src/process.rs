//! This process's own resource usage, for the `--soak` harness.
//!
//! Two of `docs/perf-budgets.md`'s rows name `--soak` as the instrument that
//! measures them — "Idle RSS (flyout closed) <= 35 MB" and "Soak (24 h) RSS
//! growth < 5 MB; flat GDI/USER handle counts" — and until P8 wave 3 there was
//! no such flag and no way to read either number. This module is the half that
//! reads them.
//!
//! # What is measured, and how it differs from the budget's wording
//!
//! `docs/perf-budgets.md` says "35 MB **private**". What this module reports is
//! the **whole resident set** - private pages plus resident *shareable* ones
//! (DLL and shared-object code, mapped section views). That is not the same
//! number, and the difference is not small: measured on one live process here,
//! `WorkingSetSize` was 73,158,656 against a private working set of 41,459,712.
//!
//! It over-counts, which is the safe direction to be wrong in against a ceiling,
//! and it is stated here rather than papered over because an earlier version of
//! this comment claimed the opposite - that `WorkingSetSize` "is what Task
//! Manager's active private working set column is derived from". It is not, and
//! no PSAPI field yields that column: `PrivateUsage` is private *commit* rather
//! than resident.
//!
//! - **Windows**: `WorkingSetSize` from `GetProcessMemoryInfo`.
//! - **Linux**: field 2 of `/proc/self/statm`, resident pages times the page
//!   size - the same figure `ps` prints as RSS, and shared pages are in it for
//!   the same reason. No `unsafe`; one crate, `rustix`, for the page size.
//! - **macOS**: **not implemented.** `task_info(MACH_TASK_BASIC_INFO)` is the
//!   right call and it is Mach FFI, which is more `unsafe` than a soak harness
//!   should introduce into a crate whose macOS surface nobody has ever run.
//!   `self_metrics` returns `None` there rather than a wrong number, and the
//!   caller reports "unavailable on this platform" rather than "0 MB".
//!
//! GUI object counts are Windows-only by nature: `GetGuiResources` counts GDI
//! and USER objects, which are a Win32 concept with no counterpart elsewhere.
//! They are **not** kernel handles - an earlier version of this sentence called
//! them "Win32 kernel objects", four lines above a section explaining that the
//! two are different things. The budget row that mentions them says "flat GDI/USER handle
//! counts" and was written for the Windows train.
//!
//! # Kernel handles, which are a different thing from GUI objects
//!
//! [D-112] is the row: `--soak` assembles the pump, the engine and the IPC
//! server, and **none of those creates a GUI object**. A named-pipe instance is
//! a *kernel* handle, which `GetGuiResources` does not count, so a passing
//! GDI/USER verdict on a headless run means "the pump and the engine leaked no
//! GUI objects" - true, cheap, and not what the budget row asks. The row calls
//! counting kernel handles "the cheap half" and "probably the right first step".
//!
//! - **Windows**: `GetProcessHandleCount`, which counts every open kernel
//!   handle the process owns - pipes, files, events, threads, registry keys.
//!   This is the counter a leaked pipe instance moves.
//! - **Linux**: the number of entries in `/proc/self/fd`, which is the same
//!   question in the shape Linux answers it. **The count includes the handle
//!   the read itself holds open** - `read_dir` opens one directory stream and
//!   `ReadDir` owns it for the whole walk - so the absolute figure is exactly
//!   one higher than a caller might expect; growth, which is what the budget
//!   measures, is unaffected because every sample pays the same one.
//! - **macOS**: `None`, for the same reason RSS is - `proc_pidinfo` is FFI
//!   nobody here has run.
//!
//! [D-112]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-112

/// A snapshot of this process's resource usage.
///
/// Every field is optional-by-platform rather than defaulted, because a zero
/// here would be indistinguishable from a real measurement of zero and would
/// make a soak report a lie on the platform that could not measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMetrics {
    /// Resident set size in bytes.
    pub rss_bytes: u64,
    /// Live GDI objects, or `None` where the concept does not exist.
    pub gdi_objects: Option<u32>,
    /// Live USER objects, or `None` where the concept does not exist.
    pub user_objects: Option<u32>,
    /// Open kernel handles (Windows) or open file descriptors (Linux), or
    /// `None` on a platform that counts neither - macOS today.
    ///
    /// **`None` never means "the read failed".** A failed read makes the whole
    /// sample `None`, so the soak counts it as unreadable rather than reporting
    /// a family as uncounted.
    ///
    /// **Not a GUI object count**, and the distinction is the whole of
    /// [D-112](https://github.com/itabajah/duja/blob/main/docs/debt.md#d-112):
    /// the soak's own IPC server creates named-pipe instances, which move this
    /// counter and are invisible to `GetGuiResources`.
    pub kernel_handles: Option<u32>,
}

/// Read this process's metrics, or `None` if this platform cannot.
#[must_use]
pub fn self_metrics() -> Option<ProcessMetrics> {
    imp::self_metrics()
}

#[cfg(windows)]
mod imp {
    use super::ProcessMetrics;

    pub(super) fn self_metrics() -> Option<ProcessMetrics> {
        // All of them or nothing. `gui_objects` answers `None` on a *failed
        // query*, and `Option` cannot tell that apart from "this platform has no
        // such concept" - which is what `None` means on Linux. Returning a
        // half-read sample here made the soak print "not counted on this
        // platform" about Windows, which counts them fine, and skip the handle
        // budget entirely while still reporting PASS. A sample missing half the
        // budget is not a measurement, so it is reported as unreadable and the
        // soak's own `unreadable` tally sees it.
        let rss = crate::win::sys::working_set_bytes()?;
        let gdi = crate::win::sys::gui_objects(true)?;
        let user = crate::win::sys::gui_objects(false)?;
        let kernel = crate::win::sys::kernel_handles()?;
        Some(ProcessMetrics {
            rss_bytes: rss,
            gdi_objects: Some(gdi),
            user_objects: Some(user),
            kernel_handles: Some(kernel),
        })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::ProcessMetrics;

    pub(super) fn self_metrics() -> Option<ProcessMetrics> {
        // All-or-nothing, the same rule as the Windows arm and for the same
        // reason: `None` in a metrics field has to mean "this platform does not
        // count that" and *only* that, because the soak's report prints "not
        // counted on this platform" for it and skips the budget. A first version
        // let a failed `/proc/self/fd` read through as `None` on the argument
        // that it and `statm` are unrelated files - which would have made a
        // Linux box with an unreadable `/proc/self/fd` report "not counted on
        // this platform" about a platform that counts it, skip the handle
        // budget, and PASS. That is verbatim the regression `SoakRun::
        // handle_growth`'s doc exists to memorialise, and the argument for it
        // named no scenario anyone had seen: both files are the same mount and
        // the same task directory.
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages = super::resident_pages(&statm)?;
        let kernel = super::count_dir_entries(std::path::Path::new("/proc/self/fd"))?;
        // `rustix::param::page_size` rather than a hardcoded 4096: arm64 Linux
        // is commonly 16 KiB, and a soak that reports a quarter of the real RSS
        // would pass a budget it is breaking.
        let page = rustix::param::page_size() as u64;
        Some(ProcessMetrics {
            rss_bytes: pages.saturating_mul(page),
            gdi_objects: None,
            user_objects: None,
            kernel_handles: Some(kernel),
        })
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::ProcessMetrics;

    /// macOS and anything else: see the module header. `None`, not a guess.
    pub(super) fn self_metrics() -> Option<ProcessMetrics> {
        None
    }
}

/// Field 2 of a `/proc/<pid>/statm` line: resident pages.
///
/// Split out and compiled everywhere so the parse is tested on all three lanes
/// rather than only on the one that has a `/proc`. The file's format is seven
/// space-separated integers; only the second is wanted, and a kernel that
/// printed fewer must not panic here.
#[cfg(any(test, target_os = "linux"))]
fn resident_pages(statm: &str) -> Option<u64> {
    statm.split_whitespace().nth(1)?.parse().ok()
}

/// How many entries `dir` holds, or `None` if it cannot be read.
///
/// This is Linux's open-descriptor count (`/proc/self/fd`), split out and
/// compiled everywhere so it is tested on all three lanes against an ordinary
/// directory rather than only on the one that has a `/proc`.
///
/// **A failure part-way through the walk answers `None`, not a smaller number.**
/// A first version used `.flatten()`, on the stated grounds that "a descriptor
/// closed by another thread yields an error for that entry". Linux's `ReadDir`
/// does no such thing: it builds each entry from the `getdents64` record alone,
/// with no per-entry `stat`, so a vanished descriptor produces no error at all -
/// it is either still in the buffer (counted, stale) or already gone (not
/// counted). What `Some(Err(_))` actually signals there is a *stream* failure,
/// which also ends the walk. Flattening that away returned a **truncated count
/// indistinguishable from a genuine smaller one**, which is the same
/// zero-versus-failure ambiguity this function's sibling test exists to rule
/// out, and it is worse than useless in a growth budget: a truncated baseline
/// invents growth, and a truncated final sample hides it.
///
/// The error arm is **not exercised by a test**, because forcing a mid-walk
/// `readdir64` failure is not something a portable test can arrange. It is
/// written the safe way rather than the demonstrated way, and this sentence is
/// here so that is a known gap rather than an assumed guarantee.
#[cfg(any(test, target_os = "linux"))]
fn count_dir_entries(dir: &std::path::Path) -> Option<u32> {
    let mut counted: u32 = 0;
    for entry in std::fs::read_dir(dir).ok()? {
        entry.ok()?;
        counted = counted.checked_add(1)?;
    }
    Some(counted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_pages_is_the_second_field() {
        // A real line from a running process: size, resident, shared, text,
        // lib, data, dt.
        assert_eq!(resident_pages("6234 1893 1204 41 0 1421 0\n"), Some(1893));
    }

    #[test]
    fn a_short_or_junk_statm_is_none_rather_than_a_panic() {
        assert_eq!(resident_pages(""), None);
        assert_eq!(resident_pages("6234"), None, "only one field");
        assert_eq!(resident_pages("6234 notanumber"), None);
        // Leading whitespace must not shift the field index.
        assert_eq!(resident_pages("  6234 1893 1204"), Some(1893));
    }

    /// Zero would be a legitimate count for an empty directory, so a failure
    /// has to answer something else entirely - the same rule the Windows
    /// `gui_objects` reader follows for its own zero-versus-failure ambiguity.
    ///
    /// Both shapes `read_dir` can refuse: a path that is not there, and a path
    /// that is there and is not a directory. The name used to say "unreadable"
    /// while the body only tried the first.
    #[test]
    fn a_path_that_is_not_a_readable_directory_is_none_rather_than_zero() {
        assert_eq!(
            count_dir_entries(std::path::Path::new(
                "definitely-not-a-directory-in-this-tree"
            )),
            None,
            "a path that does not exist"
        );

        let file = std::env::temp_dir().join(format!("duja-notadir-{}", std::process::id()));
        std::fs::write(&file, b"x").expect("a scratch file under the temp dir");
        let _cleanup = FileCleanup(file.clone());
        assert_eq!(
            count_dir_entries(&file),
            None,
            "a path that exists and is a file"
        );
    }

    /// Removes its file on the unwind as well as on success.
    struct FileCleanup(std::path::PathBuf);

    impl Drop for FileCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Removes its directory on the unwind as well as on success.
    ///
    /// A guard rather than a trailing call because the assertions it protects
    /// panic on failure, and a trailing cleanup is skipped by the panic - which
    /// is how an earlier test in this repository left a key in the author's own
    /// registry.
    struct Cleanup(std::path::PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_entry_count_tracks_what_is_in_the_directory() {
        let dir = std::env::temp_dir().join(format!(
            "duja-fdcount-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory under the temp dir");
        let _cleanup = Cleanup(dir.clone());

        assert_eq!(
            count_dir_entries(&dir),
            Some(0),
            "a fresh directory is empty"
        );
        for i in 0..3_u32 {
            std::fs::write(dir.join(i.to_string()), b"x").expect("a file in the scratch directory");
        }
        assert_eq!(count_dir_entries(&dir), Some(3));
    }
}

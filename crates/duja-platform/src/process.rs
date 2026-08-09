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
//! and USER handles, which are Win32 kernel objects with no counterpart
//! elsewhere. The budget row that mentions them says "flat GDI/USER handle
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
//!   the read itself holds open**, so the absolute figure is one higher than a
//!   caller might expect; growth, which is what the budget measures, is
//!   unaffected because every sample pays the same one.
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
    /// `None` where neither can be read.
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
        // All three or nothing. `gui_objects` answers `None` on a *failed
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
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages = super::resident_pages(&statm)?;
        // `rustix::param::page_size` rather than a hardcoded 4096: arm64 Linux
        // is commonly 16 KiB, and a soak that reports a quarter of the real RSS
        // would pass a budget it is breaking.
        let page = rustix::param::page_size() as u64;
        Some(ProcessMetrics {
            rss_bytes: pages.saturating_mul(page),
            gdi_objects: None,
            user_objects: None,
            // All-or-nothing is the Windows arm's rule and deliberately not this
            // one: there, a failed `GetGuiResources` was indistinguishable from
            // "this platform has no such concept" and silently disabled half the
            // budget. Here RSS and the descriptor count come from two unrelated
            // files, so a `/proc` that answers one and not the other is a real
            // state (a container with a restricted `/proc`), and reporting the
            // RSS we did read is better than discarding it.
            kernel_handles: super::count_dir_entries(std::path::Path::new("/proc/self/fd")),
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
/// **An entry that vanishes mid-walk is skipped, not fatal.** `/proc/self/fd` is
/// a live view of this process's descriptor table, and the walk itself opens
/// one; a descriptor closed by another thread between `read_dir` and the
/// iteration yields an error for that entry, and a sample that returned `None`
/// there would be reported as unreadable for something entirely normal.
#[cfg(any(test, target_os = "linux"))]
fn count_dir_entries(dir: &std::path::Path) -> Option<u32> {
    let entries = std::fs::read_dir(dir).ok()?;
    let counted = entries.flatten().count();
    u32::try_from(counted).ok()
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

    #[test]
    fn an_unreadable_directory_is_none_rather_than_zero() {
        // Zero would be a legitimate count for an empty directory, so the
        // failure has to be a different value entirely - the same rule the
        // Windows `gui_objects` reader follows for its own zero-versus-failure
        // ambiguity.
        assert_eq!(
            count_dir_entries(std::path::Path::new(
                "definitely-not-a-directory-in-this-tree"
            )),
            None
        );
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

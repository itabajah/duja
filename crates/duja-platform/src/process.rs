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
        let rss = crate::win::sys::working_set_bytes()?;
        Some(ProcessMetrics {
            rss_bytes: rss,
            gdi_objects: crate::win::sys::gui_objects(true),
            user_objects: crate::win::sys::gui_objects(false),
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
}

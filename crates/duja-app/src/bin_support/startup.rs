//! Startup recovery: acting on a gamma crash marker left by a dirty exit.
//!
//! A Windows gamma ramp persists after the process dies, so before engaging
//! gamma the app writes a marker file. On the next start, if that marker is
//! still present, the previous run crashed mid-dim and the screen may be stuck
//! dark: the app restores identity gamma on every display and clears the marker.
//!
//! The decision is kept as a small, cross-platform helper (marker presence via
//! the filesystem, restore via an injected hook) so the whole flow is unit-
//! testable without any Windows gamma call.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

/// Recover from a possible dirty gamma exit.
///
/// If the marker at `marker` exists, `restore` is invoked (the real app calls
/// `duja_dimmer::restore_all`) and the marker is removed. Returns whether
/// recovery ran. A missing marker is the normal clean-start case and does
/// nothing.
pub(crate) fn recover_from_crash_marker(marker: &Path, restore: impl FnOnce()) -> bool {
    if marker.exists() {
        restore();
        // Best-effort clear: a leftover marker would only trigger a harmless
        // extra restore next time.
        let _ = std::fs::remove_file(marker);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn crash_marker_forces_restore_on_next_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("gamma.dirty");
        std::fs::write(&marker, b"").expect("write marker");

        let restored = Cell::new(false);
        let ran = recover_from_crash_marker(&marker, || restored.set(true));

        assert!(ran, "recovery must run when the marker is present");
        assert!(restored.get(), "restore hook must fire");
        assert!(!marker.exists(), "marker must be cleared after recovery");
    }

    #[test]
    fn clean_start_does_not_restore() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("gamma.dirty");

        let restored = Cell::new(false);
        let ran = recover_from_crash_marker(&marker, || restored.set(true));

        assert!(!ran);
        assert!(!restored.get(), "no restore without a marker");
    }
}

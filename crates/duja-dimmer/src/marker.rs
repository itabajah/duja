//! The gamma crash marker: a file whose existence means "a ramp was engaged and
//! may not have been put back".
//!
//! Three `std::fs` calls with no platform anything in them, which is why they are
//! here rather than beside a backend. They were in `win/gamma.rs` until Linux
//! needed them, on the reasonable-at-the-time belief that the marker was a
//! Windows idea; it is not, it is an idea about **gamma ramps that outlive the
//! process**, and X11 has those too — an `XRandR` ramp is server state, so a
//! crashed Duja leaves the screen dark exactly as it would on Windows. macOS is
//! the odd one out, and deliberately writes no marker (`bin_support::gamma`
//! records how well that belief is established).
//!
//! Moving them also moved their test off the Windows lane, which is the smaller
//! half of the point and still worth stating: the idempotence both functions
//! promise is now pinned on all three CI lanes rather than one.

use std::path::Path;

/// Write the crash marker at `path` (atomic create).
///
/// The marker's mere existence signals "gamma was engaged and may not have been
/// restored". Creating it when it already exists is not an error — the previous
/// run was already dirty.
///
/// # Errors
/// The underlying [`std::io::Error`] if the file could not be created for a
/// reason other than already existing.
pub fn mark_dirty(path: &Path) -> std::io::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Remove the crash marker at `path`. A missing marker is success (idempotent).
///
/// # Errors
/// The underlying [`std::io::Error`] if removal failed for a reason other than
/// the file already being absent.
pub fn clear_marker(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether a crash marker exists at `path` (a dirty prior exit).
#[must_use]
pub fn marker_present(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::{clear_marker, mark_dirty, marker_present};

    #[test]
    fn marker_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("duja-dimmer-marker-{}.tmp", std::process::id()));
        let _ = clear_marker(&path);
        assert!(!marker_present(&path));
        mark_dirty(&path).unwrap();
        assert!(marker_present(&path));
        // Idempotent create.
        mark_dirty(&path).unwrap();
        assert!(marker_present(&path));
        clear_marker(&path).unwrap();
        assert!(!marker_present(&path));
        // Idempotent clear.
        clear_marker(&path).unwrap();
    }
}

//! Trusted per-user directories and files on unix.
//!
//! Two Duja subsystems keep per-user state in a directory they expect to own:
//! [`ipc::unix_socket`](crate::ipc) puts `ctl.sock` in one, and
//! [`single_instance`](crate::single_instance) puts `duja.lock` in one. Both used
//! to create that directory with a *recursive* builder, which returns `Ok` for a
//! directory that already exists **without inspecting it** — so a directory
//! another local user had created first was accepted, silently, as Duja's own.
//!
//! That is fail-open, and this module is the fail-closed replacement.
//!
//! # Ownership is refused; mode is repaired
//!
//! The two properties are not the same trust question, and treating them alike
//! was the first draft's mistake.
//!
//! **Ownership** cannot be repaired. A directory belonging to another uid is one
//! Duja must not put a socket or a lock in, and there is nothing to do about it
//! but refuse and let the caller degrade.
//!
//! **Mode** can be, and refusing on it would break working installations for no
//! gain. A directory that is *ours* but `0755` is one we may `chmod` — that is a
//! repair, not a compromise — and it arises legitimately: an unusual `umask`, an
//! earlier Duja that created it differently, or a caller that made the directory
//! before handing us the path (Duja's own IPC integration tests do exactly this).
//! Nor is the loose state itself a breach: the socket is `0600`, so `connect`
//! needs write permission on the inode and another user is refused at the socket
//! even while the directory is traversable.
//!
//! The `chmod` is `fchmod` on the descriptor already opened for the ownership
//! check, not a second path lookup, so the bits tightened belong to the object
//! just verified.
//!
//! # Why the leaf is strict and its parents are not
//!
//! [`ensure_private_dir`] creates missing parents permissively and applies the
//! rule only to the final component. The parents are pre-existing system
//! locations — `$XDG_RUNTIME_DIR`, `/tmp`, `~/Library/Application Support` — that
//! Duja does not own and cannot vouch for; the leaf (`duja`, `duja-<uid>`, or the
//! macOS bundle-id directory) is the one Duja creates, the one that holds the
//! socket and the lock, and the one an unprivileged attacker can actually race us
//! to. Applying the ownership rule to `/tmp` would refuse to start on every
//! normal system, since `/tmp` is `1777` and owned by root by design.
//!
//! # How durable the verification is
//!
//! Verification is a check followed by a use, and there is no `bindat`: the unix
//! socket API is path-based all the way down, so the check cannot be fused to the
//! bind the way `openat` would fuse it for a file. What makes the gap safe is not
//! timing but the permissions we just verified:
//!
//! - The directory is `0700` and ours, so no other user can create, rename or
//!   remove anything *inside* it.
//! - Replacing the directory itself needs write permission on its **parent**. For
//!   `$XDG_RUNTIME_DIR` and `~/Library` that parent is already ours. For `/tmp`
//!   the sticky bit (`1777`) is what forbids it: a user may create entries there
//!   but may only delete or rename their own. Duja's `/tmp` fallback therefore
//!   rests on the sticky bit, which is universal but is a real assumption and is
//!   named here rather than left implicit.
//!
//! So once a directory passes, an unprivileged attacker cannot swap it out from
//! under the caller. What verification cannot defend against is root, which is
//! outside the threat model (SECURITY.md).
//!
//! # What CI can and cannot test
//!
//! The mode repair and the symlink and not-a-directory refusals are exercised on
//! both unix lanes. The **foreign-owner** refusal is not: CI runs as a single
//! user and cannot create a directory owned by somebody else. So ownership
//! follows the same split the peer-credential check uses — the pure decision
//! ([`verdict`]) is unit-tested over every uid/mode combination, and only the
//! `stat` that feeds it is unverified.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

/// The permission bits that must be **clear** on a directory Duja trusts: every
/// group and other bit.
const GROUP_AND_OTHER: u32 = 0o077;

/// The owner bits that must be **set**: Duja has to read, write and traverse its
/// own directory. Checked so a directory left at `0000` by a pathological `umask`
/// is repaired rather than accepted and then failing at every use.
const OWNER_ALL: u32 = 0o700;

/// What to do about a directory that already exists where Duja wants a private
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Ours and already private: use it as-is.
    Accept,
    /// Ours but readable or writable beyond the owner: `chmod` it and use it.
    Tighten,
    /// Somebody else's. Nothing to repair; the caller degrades.
    Refuse,
}

/// Create `dir` as a private `0700` directory, or adopt the one already there.
///
/// Missing parents are created permissively (see the module docs); only the final
/// component is held to the rule. An existing directory is refused if it belongs
/// to another uid and tightened to `0700` if it is merely looser than it should
/// be.
///
/// # Errors
/// [`io::ErrorKind::PermissionDenied`] if `dir` exists but is a symlink, is not a
/// directory, or is owned by another uid. Any other I/O error from the underlying
/// `mkdir`/`open`/`stat`/`fchmod` is returned as-is.
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    // Non-recursive on purpose: `mkdir` is atomic and fails `EEXIST` rather than
    // succeeding silently, which is exactly the signal the recursive builder threw
    // away. A `umask` can only *clear* bits, so the created directory is never
    // more permissive than `0700` — and the `Tighten` arm below covers the
    // pathological `umask` that clears owner bits too.
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => adopt_existing_dir(dir),
        Err(e) => Err(e),
    }
}

/// Adopt an existing `dir`: refuse another user's, tighten a loose one of ours.
fn adopt_existing_dir(dir: &Path) -> io::Result<()> {
    // `O_NOFOLLOW` refuses a symlink standing in for the directory, and
    // `O_DIRECTORY` refuses a regular file; both fail at `open` rather than
    // leaving the caller to notice afterwards. Stat the descriptor rather than the
    // path so the bits examined belong to the object just opened.
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir)
        .map_err(|e| annotate(dir, &e))?;
    let meta = handle.metadata()?;
    match verdict(meta.uid(), meta.mode(), our_euid()) {
        Verdict::Accept => Ok(()),
        // `fchmod`, not `set_permissions`: the descriptor is the object whose
        // ownership was just checked, so there is no second path lookup for
        // anything to be swapped under.
        Verdict::Tighten => {
            rustix::fs::fchmod(&handle, rustix::fs::Mode::RWXU).map_err(io::Error::from)
        }
        Verdict::Refuse => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} belongs to uid {}, not to us, so Duja will not keep state in it",
                dir.display(),
                meta.uid()
            ),
        )),
    }
}

/// The pure decision behind [`adopt_existing_dir`].
///
/// Split out for the reason `peer_allowed` is: CI cannot produce a directory
/// owned by another user, so the syscall is unverifiable but the rule is not.
fn verdict(owner: u32, mode: u32, our_euid: u32) -> Verdict {
    if owner != our_euid {
        return Verdict::Refuse;
    }
    if mode & GROUP_AND_OTHER == 0 && mode & OWNER_ALL == OWNER_ALL {
        Verdict::Accept
    } else {
        Verdict::Tighten
    }
}

/// Open (creating if absent) a `0600` file inside an already-verified private
/// directory, refusing a symlink at the final component.
///
/// The directory's `0700` mode is the real barrier — nobody else can plant a
/// symlink inside it — so `O_NOFOLLOW` here is defence-in-depth against a
/// caller that reaches this without [`ensure_private_dir`] having passed.
///
/// `truncate(false)`: callers want a descriptor to `flock`, not a fresh file, and
/// truncating would discard bytes some future version may keep there.
///
/// # Errors
/// Whatever `open` returns, including `ELOOP` when the path is a symlink.
pub(crate) fn open_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// This process's effective uid.
fn our_euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Turn the two refusal-shaped `open` failures into the error kind the caller
/// documents, leaving every other failure alone.
///
/// `O_NOFOLLOW` on a symlink reports `ELOOP` and `O_DIRECTORY` on a regular file
/// reports `ENOTDIR`; both mean "something other than our directory is sitting
/// here", which is a permission decision rather than the plumbing failure their
/// raw kinds suggest.
fn annotate(dir: &Path, err: &io::Error) -> io::Error {
    let raw = err.raw_os_error();
    if raw == Some(libc::ELOOP) || raw == Some(libc::ENOTDIR) {
        return io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is a symlink or not a directory, so Duja will not use it",
                dir.display()
            ),
        );
    }
    io::Error::new(err.kind(), err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// A unique directory under the system temp dir for one test to own.
    fn scratch(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("duja-unix-dir-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_file(&path);
        path
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn creates_a_missing_directory_private() {
        let dir = scratch("create").join("leaf");
        ensure_private_dir(&dir).unwrap();
        assert!(dir.is_dir());
        assert_eq!(mode_of(&dir), 0o700, "leaf must be 0700");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn accepts_an_existing_private_directory() {
        let dir = scratch("accept");
        ensure_private_dir(&dir).unwrap();
        // Idempotent: the second call takes the verify path, not the create path.
        ensure_private_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Half the regression for the fail-open defect: a pre-existing directory of
    /// ours that is open to others is **repaired**, where the old recursive
    /// builder left it exactly as it found it.
    #[test]
    fn tightens_an_existing_directory_of_ours_that_is_open_to_others() {
        for mode in [0o755, 0o770, 0o707, 0o701, 0o710, 0o777] {
            let dir = scratch(&format!("mode-{mode:o}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();

            ensure_private_dir(&dir)
                .unwrap_or_else(|e| panic!("mode {mode:04o} is ours and should be repaired: {e}"));
            assert_eq!(
                mode_of(&dir),
                0o700,
                "mode {mode:04o} must be tightened to 0700"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A directory left unusable by a pathological `umask` is repaired too, so
    /// the caller does not accept it and then fail on every read.
    #[test]
    fn tightens_a_directory_missing_its_own_owner_bits() {
        let dir = scratch("mode-0000");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_symlink_standing_in_for_the_directory() {
        let real = scratch("symlink-target");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();

        let link = scratch("symlink-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // The target is private and ours, so only `O_NOFOLLOW` can catch this:
        // following the link would hand Duja a directory it never created.
        let err = ensure_private_dir(&link).expect_err("a symlink must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&real);
    }

    #[test]
    fn refuses_a_regular_file_where_the_directory_belongs() {
        let path = scratch("not-a-dir");
        std::fs::write(&path, b"squat").unwrap();

        let err = ensure_private_dir(&path).expect_err("a file must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        let _ = std::fs::remove_file(&path);
    }

    /// The half CI cannot reach through the filesystem: a directory owned by
    /// somebody else. Tested as the pure rule instead.
    ///
    /// The distinction this pins is the one the module exists for — a foreign
    /// owner is refused at *any* mode, including the `0700` that would look
    /// perfect from the outside, while our own directory is never refused no
    /// matter how loose it is.
    #[test]
    fn foreign_owner_is_refused_at_every_mode_and_ours_is_never_refused() {
        for mode in [0o40700, 0o40755, 0o40777, 0o40000] {
            assert_eq!(
                verdict(1001, mode, 1000),
                Verdict::Refuse,
                "another user's directory at mode {mode:o}"
            );
            assert_eq!(
                verdict(0, mode, 1000),
                Verdict::Refuse,
                "root-owned is still foreign at mode {mode:o}"
            );
            assert_ne!(
                verdict(1000, mode, 1000),
                Verdict::Refuse,
                "our own directory at mode {mode:o} is repairable, not refusable"
            );
        }
    }

    #[test]
    fn our_directory_is_accepted_only_when_already_exactly_private() {
        // `S_IFDIR` rides along in `st_mode` and must not be read as access.
        assert_eq!(verdict(1000, 0o40700, 1000), Verdict::Accept);
        assert_eq!(verdict(1000, 0o40750, 1000), Verdict::Tighten, "group read");
        assert_eq!(verdict(1000, 0o40705, 1000), Verdict::Tighten, "other r-x");
        assert_eq!(verdict(1000, 0o40701, 1000), Verdict::Tighten, "one bit");
        assert_eq!(
            verdict(1000, 0o40600, 1000),
            Verdict::Tighten,
            "no traverse"
        );
        assert_eq!(verdict(1000, 0o40000, 1000), Verdict::Tighten, "unusable");
    }

    #[test]
    fn private_file_refuses_a_symlink() {
        let dir = scratch("file-symlink");
        ensure_private_dir(&dir).unwrap();
        let target = dir.join("real");
        std::fs::write(&target, b"x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(
            open_private_file(&link).is_err(),
            "O_NOFOLLOW must refuse a symlinked lock path"
        );
        // The same call on the real path succeeds, so the refusal is the symlink
        // and not the directory.
        assert!(open_private_file(&target).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn private_file_creates_0600_and_keeps_existing_bytes() {
        let dir = scratch("file-create");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("duja.lock");

        drop(open_private_file(&path).unwrap());
        assert_eq!(mode_of(&path), 0o600);

        std::fs::write(&path, b"payload").unwrap();
        drop(open_private_file(&path).unwrap());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"payload",
            "opening must not truncate"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

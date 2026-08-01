//! Trusted per-user directories and files on unix.
//!
//! Two Duja subsystems keep per-user state in a directory they expect to own:
//! [`ipc::unix_socket`](crate::ipc) puts `ctl.sock` in one, and
//! [`single_instance`](crate::single_instance) puts `duja.lock` in one. Neither
//! used to establish that the directory was actually theirs, and the two failed
//! differently:
//!
//! - `single_instance`'s `ensure_dir_0700` was a *recursive* `DirBuilder`, which
//!   returns `Ok` for a directory that already exists **without inspecting it**.
//!   A directory another local user created first was adopted silently. That one
//!   was fail-open.
//! - The socket's `prepare_socket_dir` was `create_dir_all` followed by
//!   `set_mode(0700)`, and `chmod` on another uid's directory is `EPERM`, so a
//!   squatted directory made the server fail to start. That one failed *closed*,
//!   with a confusing error. Its real hole was different: `set_mode` follows
//!   symlinks, so a symlink planted at `/tmp/duja-<uid>` redirected both the
//!   `chmod` and the `bind`.
//!
//! This module replaces both with one rule, and closes the symlink hole with
//! `O_NOFOLLOW`.
//!
//! # The rule
//!
//! | state of the leaf directory | outcome |
//! |---|---|
//! | absent | created `0700` |
//! | a symlink, or not a directory | **refused** |
//! | owned by another uid | **refused** |
//! | group- or other-**writable** | **refused** |
//! | ours, loose in any other way | `fchmod`ed to `0700` |
//! | ours and exactly `0700` | accepted |
//!
//! # Why writable is refused and merely-readable is repaired
//!
//! This distinction is the whole design, and the first draft of this module got
//! it wrong twice — first refusing everything (which breaks legitimate cases),
//! then repairing everything (which is unsound).
//!
//! Write permission on a *directory* is permission to create, rename and unlink
//! entries **inside** it. So on a group- or world-writable directory, by the time
//! Duja arrives another user may already have planted the very files this module
//! is supposed to protect, and tightening the mode afterwards does not undo that:
//!
//! - `ctl.sock` replaced by **their** socket. [`PipeClient`](crate::PipeClient)
//!   performs no server-identity check — only the server checks its peer — so
//!   `dujactl` would talk to them and act on forged replies.
//! - `ctl.sock` simply unlinked, so clients get `NotRunning` and `dujactl`
//!   silently falls back to driving the hardware directly.
//! - `ctl.sock.lock` or `duja.lock` planted as a **regular file** they hold a
//!   `flock` on. `O_NOFOLLOW` does not catch a regular file, so Duja would fail to
//!   start after the bind lock's timeout, or read `already_running` and exit.
//!
//! None of that is reachable without write permission **at some point**, and the
//! qualifier is load-bearing: a directory that was `0777`, had something planted
//! in it, and was then tightened by its owner to `0755` reaches `Tighten` here and
//! is accepted. That is why [`open_private_file`] checks the *owner* of the file it
//! opens rather than trusting the directory — the one thing `O_NOFOLLOW` cannot do
//! is catch a planted regular file. The socket itself has no equivalent check; what
//! covers it is that `takeover_bind`'s probe would connect to the squatter's
//! listener and refuse to start rather than adopting it.
//!
//! Group or other **read and execute** grants only traverse and list: the socket is
//! `0600` and `connect` requires write permission on the inode, and the lock files
//! this module creates are `0600`, so nothing can be planted, replaced or removed
//! while that is the state. That state is also the one that
//! arises innocently — `create_dir_all` leaves `0755` under an ordinary `umask`,
//! which is exactly how a caller that makes the directory before handing over the
//! path produces it, Duja's own IPC integration tests included. Refusing it would
//! break working installations to prevent nothing.
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
//! Duja does not own and cannot vouch for; the leaf is the one Duja creates, the
//! one that holds the socket and the lock, and the one an unprivileged attacker
//! can actually race us to.
//!
//! **That is a property of the callers, not of this function.** `ensure_private_dir`
//! will apply the rule to whatever leaf it is handed, so a caller that passes a
//! *shared* directory gets it refused (`/tmp` is root-owned, so `verdict` says
//! `Refuse`). Production callers always resolve a Duja-specific leaf —
//! `$XDG_RUNTIME_DIR/duja`, `/tmp/duja-<uid>`, the macOS bundle-id directory — and
//! a `single_instance` **test** that did not was the one thing this rule broke
//! when it landed.
//!
//! # How durable the verification is
//!
//! Verification is a check followed by a use, and there is no `bindat`: the unix
//! socket API is path-based all the way down, so the check cannot be fused to the
//! bind the way `openat` would fuse it for a file. What makes the gap safe is not
//! timing but the permissions just established:
//!
//! - The directory ends up `0700` and ours, so no other user can create, rename or
//!   remove anything inside it.
//! - Replacing the directory itself needs write permission on its **parent**. For
//!   `$XDG_RUNTIME_DIR` and `~/Library` that parent is already ours. For `/tmp` the
//!   sticky bit (`1777`) is what forbids it: a user may create entries there but
//!   may only delete or rename their own.
//!
//! The `/tmp` case therefore rests on the sticky bit. Every Linux and macOS system
//! ships `/tmp` sticky, but that is a convention rather than something POSIX
//! mandates, and this code neither detects nor survives a `/tmp` without it. Named
//! here rather than left implicit. Root is outside the threat model (SECURITY.md).
//!
//! # What CI can and cannot test
//!
//! The mode repair, the writable refusal, the symlink refusal and the
//! not-a-directory refusal all run on both unix lanes. The **foreign-owner**
//! refusal does not: CI runs as a single user and cannot create a directory owned
//! by somebody else. So ownership follows the same split the peer-credential check
//! uses — the pure decision ([`verdict`]) is unit-tested over a chosen grid of
//! uid/mode pairs, and only the `stat` that feeds it is unverified.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Write permission for group or other. The bit that turns a loose directory
/// from untidy into unsound, because it is permission to plant entries.
const GROUP_OR_OTHER_WRITE: u32 = 0o022;

/// Every group and other permission bit.
const NON_OWNER_ACCESS: u32 = 0o077;

/// `setuid` / `setgid` / sticky. Duja never wants any of them on its own state
/// directory, and leaving them set while tightening the rest would be an odd
/// half-measure.
const SPECIAL_BITS: u32 = 0o7000;

/// The owner bits that must be **set**: Duja has to read, write and traverse its
/// own directory.
const OWNER_ALL: u32 = 0o700;

/// What to do about a directory that already exists where Duja wants a private
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Ours and already exactly private: use it as-is.
    Accept,
    /// Ours, and loose only in ways that cannot have let anyone plant anything:
    /// `chmod` it and use it.
    Tighten,
    /// Another user's, or writable by another user. Nothing safe to repair.
    Refuse,
}

/// Create `dir` as a private `0700` directory, or adopt the one already there.
///
/// Missing parents are created permissively; only the final component is held to
/// the rule, and the caller is responsible for that component being one Duja owns
/// (see the module docs).
///
/// # Errors
/// [`io::ErrorKind::PermissionDenied`] if `dir` exists but is a symlink, is not a
/// directory, is owned by another uid, or is writable beyond its owner. Errors
/// from `mkdir`, `stat` and `fchmod` are returned unchanged; errors from the
/// `open` are re-wrapped (see [`annotate`]).
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    // Non-recursive on purpose: `mkdir` is atomic and fails `EEXIST` rather than
    // succeeding silently, which is exactly the signal the recursive builder threw
    // away. It is also a single syscall at `0700`, where the old
    // `create_dir_all` + `set_mode` pair left the directory at `0755` under an
    // ordinary umask and only then tightened it.
    //
    // A `umask` can only *clear* bits, so what this creates is never more
    // permissive than `0700`. It can be less: a `umask` of `0700` would leave the
    // directory at `0000`, which Duja then cannot open. That is a self-inflicted
    // configuration and it fails loudly at first use rather than silently — the
    // `Tighten` arm below cannot rescue it, because it is only reached when the
    // directory already existed.
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => adopt_existing_dir(dir),
        Err(e) => Err(e),
    }
}

/// Adopt an existing `dir`: refuse another user's or a writable one, tighten a
/// merely-loose one of ours.
fn adopt_existing_dir(dir: &Path) -> io::Result<()> {
    // `O_NOFOLLOW` refuses a symlink standing in for the directory — the hole the
    // old `set_mode` had — and `O_DIRECTORY` refuses a regular file; both fail at
    // `open` rather than leaving the caller to notice afterwards. Stat the
    // descriptor rather than the path so the bits examined belong to the object
    // just opened.
    //
    // `read(true)` means this also fails `EACCES` on a directory of ours with no
    // owner-read bit. That is a refusal too, and an honest one: a directory Duja
    // cannot open is not one it can repair.
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
                "{} is uid {}'s or writable beyond its owner (mode {:04o}), \
                 so Duja will not keep state in it",
                dir.display(),
                meta.uid(),
                meta.mode() & 0o7777
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
    // Someone else could already have planted entries; tightening now is too late.
    if mode & GROUP_OR_OTHER_WRITE != 0 {
        return Verdict::Refuse;
    }
    if mode & NON_OWNER_ACCESS != 0 || mode & SPECIAL_BITS != 0 || mode & OWNER_ALL != OWNER_ALL {
        return Verdict::Tighten;
    }
    Verdict::Accept
}

/// Open (creating if absent) a `0600` file inside an already-verified private
/// directory, refusing a symlink and refusing another user's file.
///
/// The directory's `0700` mode is the primary barrier. The two checks here are
/// what covers the case it cannot: a directory that was loose *before*
/// [`ensure_private_dir`] tightened it. `O_NOFOLLOW` catches a planted symlink and
/// the owner check catches a planted regular file, which `O_NOFOLLOW` does not.
///
/// `truncate(false)`: callers want a descriptor to `flock`, not a fresh file, and
/// truncating would discard bytes some future version may keep there. A
/// consequence worth knowing: `.mode(0o600)` applies only at creation, so a
/// pre-existing file keeps whatever mode it has. That is safe given the owner
/// check above, since only we could have created it.
///
/// # Errors
/// `ELOOP` when the path is a symlink, [`io::ErrorKind::PermissionDenied`] when
/// the file belongs to another uid, or whatever else `open` returns.
pub(crate) fn open_private_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if meta.uid() == our_euid() {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} belongs to uid {}, not to us",
                path.display(),
                meta.uid()
            ),
        ))
    }
}

/// This process's effective uid.
fn our_euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Turn the two refusal-shaped `open` failures into the error kind the caller
/// documents, leaving every other failure's *kind* alone.
///
/// `O_NOFOLLOW` on a symlink reports `ELOOP` and `O_DIRECTORY` on a regular file
/// reports `ENOTDIR`; both mean "something other than our directory is sitting
/// here", which is a permission decision rather than the plumbing failure their
/// raw kinds suggest. POSIX mandates `ELOOP` here and both Linux and Darwin
/// comply, which is what lets the tests assert one kind on both lanes; some other
/// BSDs report `EMLINK` or `EFTYPE` instead, and neither is a Duja target.
///
/// Every other error keeps its kind but is re-wrapped, so its `raw_os_error` is
/// lost. Callers here only match on `kind`.
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
        // Idempotent: the second call takes the adopt path, not the create path.
        ensure_private_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Half the regression for the fail-open defect: a readable-but-not-writable
    /// directory of ours is **repaired**, where the old recursive builder left it
    /// exactly as it found it. `0755` is the one that matters — it is what
    /// `create_dir_all` produces under an ordinary umask.
    #[test]
    fn tightens_a_directory_of_ours_that_is_readable_but_not_writable() {
        // `2700`/`1700` are here because `SPECIAL_BITS` is otherwise pinned only by
        // the pure `verdict` test: a fresh `mkdir` under a setgid parent inherits
        // setgid on Linux, and `ensure_private_dir` returns straight from the
        // create arm without inspecting it, so the repair happens on the *second*
        // launch. `Mode::RWXU` is a full chmod rather than an OR, which is what
        // makes these come back as a flat 0700.
        for mode in [0o755, 0o750, 0o705, 0o701, 0o710, 0o500, 0o2700, 0o1700] {
            let dir = scratch(&format!("mode-{mode:o}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();

            // Confirm the setup took, or this case proves nothing: a platform that
            // silently drops `S_ISGID` would leave the directory at exactly 0700,
            // `verdict` would answer `Accept`, and the assertion below would pass
            // without any repair having happened. The special-bit cases are
            // *skipped* rather than failed when that happens, because whether a
            // non-privileged `chmod` preserves `S_ISGID` is a platform rule about
            // group membership, not anything Duja controls. The plain modes are
            // asserted, since every unix applies those verbatim.
            let applied = mode_of(&dir);
            if applied != mode {
                // The predicate is the *difference*, not the request. Testing
                // `mode & SPECIAL_BITS` instead — as the first version did — gets
                // both arms backwards: `0o2700` stored as `0o0700` would panic
                // (the case meant to be skipped) while `0o755` mangled to `0o750`
                // would skip silently (the case meant to fail, and the vacuous
                // pass this guard exists to prevent).
                assert_eq!(
                    (mode ^ applied) & !SPECIAL_BITS,
                    0,
                    "chmod {mode:04o} became {applied:04o}; only special bits may be refused"
                );
                eprintln!("skipping {mode:04o}: this platform stored it as {applied:04o}");
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            }

            ensure_private_dir(&dir)
                .unwrap_or_else(|e| panic!("mode {mode:04o} is ours and repairable: {e}"));
            assert_eq!(
                mode_of(&dir),
                0o700,
                "mode {mode:04o} must be tightened to 0700"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The other half, and the one the first draft of this module got wrong by
    /// repairing it: a directory another user can **write** to may already have
    /// had `ctl.sock` or a lock file planted in it, and a late `chmod` does not
    /// undo that.
    #[test]
    fn refuses_a_directory_another_user_can_write_to() {
        for mode in [0o777, 0o770, 0o707, 0o727, 0o702, 0o720] {
            let dir = scratch(&format!("writable-{mode:o}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();

            let err = ensure_private_dir(&dir)
                .expect_err(&format!("mode {mode:04o} is plantable and must be refused"));
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                mode_of(&dir),
                mode,
                "a refused directory must be left alone, not half-repaired"
            );

            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::remove_dir_all(&dir);
        }
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
    /// somebody else. Tested as the pure rule instead, over a chosen grid rather
    /// than exhaustively.
    ///
    /// What this pins is that ownership dominates mode in both directions — a
    /// foreign directory is refused even at the `0700` that looks perfect from
    /// outside, and our own is never refused for any reason except writability.
    #[test]
    fn foreign_owner_is_refused_whatever_the_mode() {
        for mode in [0o40700, 0o40755, 0o40777, 0o40000, 0o42700] {
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
        }
    }

    #[test]
    fn our_directory_is_accepted_only_when_exactly_private_and_refused_only_when_writable() {
        // `S_IFDIR` rides along in `st_mode` and must not be read as access.
        assert_eq!(verdict(1000, 0o40700, 1000), Verdict::Accept);

        // Readable or traversable, but nothing can be planted: repair.
        assert_eq!(verdict(1000, 0o40750, 1000), Verdict::Tighten, "group r-x");
        assert_eq!(verdict(1000, 0o40705, 1000), Verdict::Tighten, "other r-x");
        assert_eq!(verdict(1000, 0o40701, 1000), Verdict::Tighten, "other x");
        assert_eq!(
            verdict(1000, 0o40600, 1000),
            Verdict::Tighten,
            "no traverse"
        );
        assert_eq!(verdict(1000, 0o40000, 1000), Verdict::Tighten, "unusable");
        // Special bits are cleared by the same repair rather than left set.
        assert_eq!(verdict(1000, 0o42700, 1000), Verdict::Tighten, "setgid");
        assert_eq!(verdict(1000, 0o41700, 1000), Verdict::Tighten, "sticky");

        // Writable by anyone else: too late to repair.
        assert_eq!(verdict(1000, 0o40777, 1000), Verdict::Refuse, "world write");
        assert_eq!(verdict(1000, 0o40770, 1000), Verdict::Refuse, "group write");
        assert_eq!(verdict(1000, 0o40702, 1000), Verdict::Refuse, "other write");
        assert_eq!(
            verdict(1000, 0o40720, 1000),
            Verdict::Refuse,
            "group w only"
        );
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

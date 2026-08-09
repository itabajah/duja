//! Unix-domain-socket transport for the local IPC protocol (macOS now, Linux in
//! P7).
//!
//! This is the confined-`unsafe` half of the IPC story on unix, the exact peer
//! of [`win_pipe`](super) on Windows: [`duja_ipc`] stays pure protocol, and every
//! OS call that needs `unsafe` — the peer-credential check — lives here behind a
//! safe seam. Everything else (socket creation, permissions, timeouts) is served
//! by `std::os::unix`, so the `unsafe` surface is a single, small function.
//!
//! # Security posture (SECURITY.md §IPC, plan §6)
//!
//! - The socket path is per-user and lives inside a dedicated directory Duja
//!   owns: macOS `~/Library/Application Support/duja/ctl.sock`, Linux
//!   `$XDG_RUNTIME_DIR/duja/ctl.sock` (fallback `/tmp/duja-<uid>/ctl.sock`).
//!   That directory is **verified, not assumed**: it is created `0700`, or else
//!   checked to be a real directory (not a symlink) owned by our effective uid.
//!   One another local user owns, or that any other user can *write* to, is
//!   refused and the server does not start; one merely readable is tightened. See
//!   [`crate::unix_dir`], which holds the rule, the reason writability is the
//!   dividing line, and the argument for how durable the check is. This closed a
//!   real hole here, though not the one the debt row described: `prepare_socket_dir`
//!   used to `chmod` the directory, which fails `EPERM` on another uid's, so a
//!   squat made the server fail *closed*. What it did not survive was a **symlink**
//!   planted at the path, which redirected both that `chmod` and the `bind`.
//! - **The parent directory is the real access barrier**: at `0700`,
//!   no other user can even traverse into it to reach the socket inode. The
//!   socket itself is additionally `chmod 0600` right after `bind`, as
//!   defence-in-depth. `std`'s [`UnixListener::bind`] couples `bind` + `listen`
//!   in one call, so there is no seam to `chmod` *between* them without dropping
//!   to raw FFI; the `0700` directory closes that (already tiny) window because
//!   the inode is unreachable to any other principal regardless of its mode. A
//!   `umask` dance is deliberately avoided — `umask` is process-global and not
//!   thread-safe, and would race other threads binding sockets.
//! - **Every accepted peer is verified before any byte is read**: its effective
//!   uid must equal ours. The check is one seam
//!   ([`peer_euid`]) with two `cfg` arms — Linux reads `SO_PEERCRED`, every other
//!   unix (macOS) calls `getpeereid` — and the pure comparison
//!   ([`peer_allowed`]) is unit-tested (CI cannot switch uid, so the *decision*
//!   is tested, not the syscall).
//! - **Stale-socket handling** doubles as the single-instance answer: if `bind`
//!   fails with `AddrInUse`, we try to *connect*. What that answers **differs by
//!   platform**, and two versions of this bullet have now hidden it — first by
//!   saying a refusal means the socket is ours to delete, then by saying a full
//!   backlog always makes us refuse to start:
//!   - a **successful connect** means a live server owns the name, on both, and we
//!     refuse to start a second;
//!   - a **full backlog** answers `EAGAIN` on Linux, which only a listener can
//!     produce, so that is a refusal to start too. On the BSDs it answers
//!     `ECONNREFUSED`, and the next line is what happens then;
//!   - a **refusal** means nothing is listening — on Linux. On the BSDs it means
//!     that *or* a full backlog, with nothing to tell them apart, so a live
//!     server's socket can still be unlinked there. That is the half of
//!     `docs/debt.md`'s D-076 still open, and it is a property of the kernel's
//!     answer rather than of this code.
//!
//!   Where a refusal *is* believed, the unlink is gated on the inode being a
//!   socket and being ours ([`unlink_target_is_ours`]) — which is a different
//!   distinction from the one above, and is the one the BSDs do let anyone make.
//!   See [`takeover_bind`].
//!   (On Windows the equivalent split is `FILE_FLAG_FIRST_PIPE_INSTANCE` plus the
//!   named single-instance mutex; here the bound socket *is* the instance token.)
//!   Because that sequence is several steps against a shared name rather than one
//!   atomic call, the **whole** bind runs under a sibling `flock` — otherwise two
//!   instances starting together both unlink and both bind, leaving one of them
//!   listening on an unreachable inode while believing it is serving. The lock
//!   covers the ordinary `bind` too, not just the takeover, because
//!   [`UnixListener::bind`] is `socket`/`bind`/`listen` and an instance parked
//!   between the last two is indistinguishable from a stale inode. Shutdown's
//!   unlink is conditioned on the same lock plus an inode check, since a departing
//!   server would otherwise delete its successor's socket. See [`bind_listener`]
//!   and [`unlink_if_ours`].
//! - The same limits as the pipe: at most [`MAX_CONNECTIONS`]
//!   connections are in flight, [`MAX_HANDLER_THREADS`]
//!   serve at once, and the frame codec caps a single body at 64 KiB **before**
//!   allocating (enforced inside [`duja_ipc`], used here exactly as the pipe uses
//!   it).
//!
//! # Threads
//!
//! A [`PipeServer`] owns one **listener** thread and a pool of at most
//! [`MAX_HANDLER_THREADS`] **handler** threads. The
//! listener accepts connections and hands each to a handler over a bounded
//! channel; an atomic in-flight counter caps the total accepted at
//! [`MAX_CONNECTIONS`], and while at capacity the listener
//! stops accepting so excess connections wait in the kernel backlog rather than
//! growing the server (the unix analogue of the pipe's `nMaxInstances` ceiling).
//!
//! Unix has no clean connection-time `ERROR_PIPE_BUSY` analogue — a `connect` to
//! a live listener succeeds into the backlog — so the cap manifests as *bounded
//! concurrency* plus backlog backpressure, not a connect-time `Busy`. What is
//! preserved is the security-relevant property: a flood cannot exhaust the
//! server's threads or memory.
//!
//! # Bounded I/O and the exchange-wide read deadline
//!
//! The listener runs its `accept` **non-blocking**, re-checking the stop flag on
//! a short slice, so shutdown is prompt without the murky semantics of closing a
//! listening fd from another thread (undefined-ish on macOS) or a self-connect
//! nudge. This mirrors `win_pipe`'s sliced overlapped wait — same cost profile,
//! same promptness.
//!
//! Reads and writes on an accepted connection are bounded the same way, gated by
//! a sliced [`poll`](poll_ready) on the non-blocking socket: the wait blocks for
//! at most a slice, then the operation is retried until it completes, times out,
//! or the stop flag is observed. `poll` is used rather than
//! `SO_RCVTIMEO`/`SO_SNDTIMEO` because macOS rejects those socket options on
//! `AF_UNIX` (`EINVAL`); `poll` behaves identically on Linux and macOS.
//!
//! The server arms **one whole-exchange read deadline** (see
//! [`SockStream::with_read_deadline`]) computed from an [`Instant`]; each slice is
//! `min(WAIT_SLICE, remaining_budget)`, so the budget is spent across the whole
//! exchange, never renewed per read. This is the direct fix for P5 finding C1: a
//! naive per-read timeout gives every `read` a fresh budget, so a peer dribbling
//! one byte at a time renews it forever and pins a handler thread (the frame
//! never completes). Clients keep a per-read budget — they talk to a trusted,
//! prompt server.
//!
//! On the stop path a read/write returns [`io::ErrorKind::ConnectionAborted`],
//! **never** [`io::ErrorKind::Interrupted`]: the framing layer's `read_exact`
//! silently *retries* `Interrupted`, which would spin forever once the stop flag
//! latches (P5's second latent bug). `Interrupted` from the OS (`EINTR`) is
//! folded into the retry loop, which re-checks stop and the deadline first, so it
//! cannot spin either.

// RATIONALE (clippy::cast_possible_truncation): the only casts here widen
// `MAX_CONNECTIONS: u32` to `usize` (never narrowing on any target we build) or
// size a `socklen_t` from a compile-time `size_of` (a 12-byte struct, far below
// `u32::MAX`); neither loses information at runtime.
#![allow(clippy::cast_possible_truncation)]
// RATIONALE (clippy::borrow_as_ptr): passing `&mut out_param` to a libc call that
// wants a raw pointer is the idiomatic FFI shape; the borrow lives exactly for
// the synchronous call.
#![allow(clippy::borrow_as_ptr)]

use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};

use duja_ipc::{Request, Response};

use super::{IpcTransportError, MAX_CONNECTIONS, MAX_HANDLER_THREADS, READ_TIMEOUT};

/// The generic short backoff in this module: the listener's wait when it is at the
/// connection cap, its retry after a transient `accept` fault, the client's
/// connect retry, and [`BindLock`]'s poll. Short enough that none of them adds
/// perceptible latency, long enough that none of them spins.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The slice a non-blocking `accept` sleeps between polls, and the slice each
/// bounded read/write blocks before re-checking the stop flag and deadline.
/// Small enough that shutdown is prompt, large enough not to spin.
const WAIT_SLICE: Duration = Duration::from_millis(50);

/// The bound on a single write to an accepted connection.
///
/// Writes complete as soon as the bytes land in the socket's send buffer, which
/// is near-instant unless a peer connected and then refuses to drain a full
/// buffer. This deadline (plus the stop flag) caps how long such a peer can pin a
/// handler in `write`; it mirrors the read timeout.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`BindLock`] waits for a contended bind lock before giving up.
///
/// Not a latency budget: honest contention is a probe plus two binds and clears
/// in microseconds. It is the bound that stops a wedged or foreign lock holder
/// from hanging startup indefinitely, so it is sized to be unmistakably longer
/// than any legitimate hold.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// The default per-user socket path as a string.
///
/// macOS: `~/Library/Application Support/duja/ctl.sock`; Linux:
/// `$XDG_RUNTIME_DIR/duja/ctl.sock`, falling back to `/tmp/duja-<uid>/ctl.sock`
/// when the runtime dir is unset. The parent directory's `0700` mode — not the
/// path — is the access barrier.
#[must_use]
pub fn default_pipe_name() -> String {
    socket_path().to_string_lossy().into_owned()
}

/// Resolve the platform socket path from the live environment.
fn socket_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        macos_socket_path(home.as_deref(), current_uid())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        linux_socket_path(xdg.as_deref(), current_uid())
    }
}

/// Pure macOS path policy: under `~/Library/Application Support/duja`, or the
/// `/tmp` fallback if `HOME` is unset.
#[cfg(target_os = "macos")]
fn macos_socket_path(home: Option<&Path>, uid: u32) -> PathBuf {
    match home {
        Some(home) => home.join("Library/Application Support/duja/ctl.sock"),
        None => tmp_fallback(uid),
    }
}

/// Pure Linux path policy: under `$XDG_RUNTIME_DIR/duja`, or the `/tmp` fallback
/// if the runtime dir is unset.
#[cfg(not(target_os = "macos"))]
fn linux_socket_path(xdg: Option<&Path>, uid: u32) -> PathBuf {
    match xdg {
        Some(dir) => dir.join("duja/ctl.sock"),
        None => tmp_fallback(uid),
    }
}

/// The per-uid `/tmp` fallback path, used when the preferred runtime/home
/// directory is unavailable.
fn tmp_fallback(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/duja-{uid}/ctl.sock"))
}

// -- Peer credential seam (the only `unsafe` in this module) --------------

/// The effective uid of the connected peer on `fd`, or `None` if it cannot be
/// determined (which the caller treats as a refusal).
///
/// Two `cfg` arms behind one signature: Linux reads the `SO_PEERCRED` socket
/// option; every other unix (macOS) calls `getpeereid`.
#[cfg(target_os = "linux")]
fn peer_euid(fd: RawFd) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `fd` is a live, connected AF_UNIX socket this process owns; `cred`
    // is a valid, writable `ucred` and `len` its byte length, exactly the
    // out-parameters `getsockopt(SO_PEERCRED)` fills. The call only reads `fd`.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut cred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

/// The effective uid of the connected peer on `fd`, or `None` if it cannot be
/// determined (which the caller treats as a refusal).
#[cfg(not(target_os = "linux"))]
fn peer_euid(fd: RawFd) -> Option<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `fd` is a live, connected AF_UNIX socket this process owns; `uid`
    // and `gid` are valid, writable out-parameters `getpeereid` fills. The call
    // only reads `fd`.
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    (rc == 0).then_some(uid)
}

/// This process's effective uid.
fn our_euid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, reads only process-global state, and
    // cannot fail.
    unsafe { libc::geteuid() }
}

/// This process's real uid (used only to name the `/tmp` fallback directory).
fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, reads only process-global state, and
    // cannot fail.
    unsafe { libc::getuid() }
}

/// Whether a peer with effective uid `peer` (or `None` if unreadable) is allowed
/// — the pure decision, tested directly because CI cannot switch uid.
fn peer_allowed(peer: Option<u32>, ours: u32) -> bool {
    peer == Some(ours)
}

// -- The connected-stream adapter (Read + Write with poll-gated timeouts) --

/// A connected socket presented as a `Read + Write` byte stream.
///
/// The socket is non-blocking; each read/write first waits for readiness with a
/// sliced [`poll`](poll_ready) so an optional shutdown flag is honoured promptly.
/// Reads are bounded by an armed exchange deadline (server) or a fresh
/// [`READ_TIMEOUT`] per read (client); writes by [`WRITE_TIMEOUT`].
///
/// `poll` is used rather than `SO_RCVTIMEO`/`SO_SNDTIMEO` because macOS rejects
/// those options on `AF_UNIX` sockets (`EINVAL`); `poll` is uniform across Linux
/// and macOS.
struct SockStream {
    stream: UnixStream,
    fd: RawFd,
    stop: Option<Arc<AtomicBool>>,
    /// When set, the instant by which **all** reads of one request→response
    /// exchange must have completed. See [`SockStream::with_read_deadline`].
    read_deadline: Option<Instant>,
}

impl SockStream {
    /// A server-side stream, cancellable by `stop`.
    fn server(stream: UnixStream, stop: Arc<AtomicBool>) -> Self {
        Self::new(stream, Some(stop))
    }

    /// A client-side stream (no shutdown flag; per-read timeout budget).
    fn client(stream: UnixStream) -> Self {
        Self::new(stream, None)
    }

    fn new(stream: UnixStream, stop: Option<Arc<AtomicBool>>) -> Self {
        let fd = stream.as_raw_fd();
        // Non-blocking so a `poll`-reported readiness that races away surfaces as
        // `WouldBlock` (retried) rather than blocking past the deadline.
        let _ = stream.set_nonblocking(true);
        SockStream {
            stream,
            fd,
            stop,
            read_deadline: None,
        }
    }

    /// Arm a single deadline shared by every read of one exchange, starting now.
    ///
    /// Without this, each read would get a fresh budget, and because the framing
    /// layer drives reads with `read_exact` (looping until its buffer fills), a
    /// peer dribbling one byte at a time would renew the budget forever and pin a
    /// handler thread — P5 finding C1. Servers arm one whole-exchange deadline and
    /// compute the remaining budget before each `poll`; clients keep the per-read
    /// budget (they talk to a trusted, prompt server).
    fn with_read_deadline(mut self, budget: Duration) -> Self {
        self.read_deadline = Instant::now().checked_add(budget);
        self
    }

    /// The deadline this read must respect: the armed exchange one if present,
    /// else a fresh per-read [`READ_TIMEOUT`] budget starting now.
    fn read_deadline(&self) -> Option<Instant> {
        self.read_deadline
            .or_else(|| Instant::now().checked_add(READ_TIMEOUT))
    }

    /// Whether the shutdown flag is set.
    fn stop_set(&self) -> bool {
        self.stop
            .as_deref()
            .is_some_and(|s| s.load(Ordering::Acquire))
    }
}

/// The slice to wait for now: `min(WAIT_SLICE, remaining)`, or a terminal timeout
/// if the deadline has already passed. `None` deadline ⇒ a full slice.
fn slice_until(deadline: Option<Instant>) -> Result<Duration, ()> {
    match deadline {
        Some(dl) => match dl.checked_duration_since(Instant::now()) {
            Some(rem) if !rem.is_zero() => Ok(rem.min(WAIT_SLICE)),
            _ => Err(()),
        },
        None => Ok(WAIT_SLICE),
    }
}

/// Wait until `fd` is ready for `events` (`POLLIN`/`POLLOUT`) or `slice` elapses.
///
/// `Ok(true)` = ready, `Ok(false)` = timed out or interrupted (the caller
/// re-checks the stop flag and deadline, then retries), `Err` = a real failure.
fn poll_ready(fd: RawFd, events: libc::c_short, slice: Duration) -> io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // At least 1 ms so a sub-millisecond remainder does not become `poll(0)` (an
    // immediate return that would busy-spin until the deadline check trips).
    let ms = i32::try_from(slice.as_millis()).unwrap_or(i32::MAX).max(1);
    // SAFETY: `pfd` is a single valid `pollfd` living across the call; `poll`
    // reads `fd`/`events` and writes `revents`, touching nothing else.
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        // EINTR: report "not ready" so the caller re-checks stop and retries.
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(err);
    }
    Ok(rc > 0)
}

impl Read for SockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = self.read_deadline();
        loop {
            if self.stop_set() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "ipc server stopping",
                ));
            }
            let Ok(slice) = slice_until(deadline) else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "ipc read timeout"));
            };
            if !poll_ready(self.fd, libc::POLLIN, slice)? {
                continue; // timed out or EINTR: re-check stop / deadline
            }
            match self.stream.read(buf) {
                Ok(n) => return Ok(n),
                // Readiness raced away or the syscall was interrupted: retry (the
                // deadline bounds the loop, so this cannot spin).
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
    }
}

impl Write for SockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now().checked_add(WRITE_TIMEOUT);
        loop {
            if self.stop_set() {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "ipc server stopping",
                ));
            }
            let Ok(slice) = slice_until(deadline) else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "ipc write timeout"));
            };
            if !poll_ready(self.fd, libc::POLLOUT, slice)? {
                continue; // timed out or EINTR: re-check stop / deadline
            }
            match self.stream.write(buf) {
                Ok(n) => return Ok(n),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// -- The server -----------------------------------------------------------

/// A running unix-socket IPC server.
///
/// Holds the listener and handler threads and the bound socket path;
/// [`shutdown`](Self::shutdown) (also run on drop) stops the listener, joins
/// every thread, and unlinks the socket.
pub struct PipeServer {
    stop: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
    socket_path: PathBuf,
    /// `(dev, ino)` of the socket this server bound, or `None` if it could not be
    /// read. Compared before unlinking so a shutdown cannot delete a *successor's*
    /// socket — see [`unlink_if_ours`].
    socket_identity: Option<(u64, u64)>,
    /// A descriptor that pins the socket's **filesystem inode**, kept open until
    /// after the unlink so its number cannot be recycled underneath
    /// [`socket_identity`](Self::socket_identity). `None` where no pin is needed
    /// or none could be taken — see [`pin_inode`].
    _pin: Option<std::os::fd::OwnedFd>,
}

impl PipeServer {
    /// Start a server on the default per-user socket path.
    ///
    /// # Errors
    /// [`IpcTransportError::Io`] if the socket directory cannot be prepared, the
    /// socket cannot be bound (e.g. a live server already owns it), or a thread
    /// cannot be spawned.
    pub fn serve<H>(handler: H) -> Result<Self, IpcTransportError>
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        Self::serve_named(&default_pipe_name(), handler)
    }

    /// Start a server on an explicit socket path (test seam).
    ///
    /// # Errors
    /// As [`serve`](Self::serve).
    pub fn serve_named<H>(name: &str, handler: H) -> Result<Self, IpcTransportError>
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        let path = PathBuf::from(name);
        let BoundSocket {
            listener,
            identity,
            pin,
        } = bind_listener(&path)?;
        // Defence-in-depth beyond the 0700 parent dir (see module docs).
        set_mode(&path, 0o600).map_err(|e| {
            unlink_if_ours(&path, identity);
            IpcTransportError::Io(e.to_string())
        })?;
        listener.set_nonblocking(true).map_err(|e| {
            unlink_if_ours(&path, identity);
            IpcTransportError::Io(e.to_string())
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let handler: Arc<dyn Fn(Request) -> Response + Send + Sync> = Arc::new(handler);
        let (work_tx, work_rx) = bounded::<UnixStream>(MAX_CONNECTIONS as usize);

        let mut workers = Vec::with_capacity(MAX_HANDLER_THREADS);
        for i in 0..MAX_HANDLER_THREADS {
            let rx = work_rx.clone();
            let stop_c = stop.clone();
            let active_c = active.clone();
            let handler_c = handler.clone();
            match thread::Builder::new()
                .name(format!("duja-ipc-handler-{i}"))
                .spawn(move || worker_loop(&rx, &stop_c, &active_c, handler_c.as_ref()))
            {
                Ok(worker) => workers.push(worker),
                Err(e) => {
                    // Unwind: stop, drop the sender so already-spawned workers
                    // exit, join them, unlink the socket.
                    stop.store(true, Ordering::Release);
                    drop(work_tx);
                    drop(work_rx);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    unlink_if_ours(&path, identity);
                    return Err(IpcTransportError::Io(e.to_string()));
                }
            }
        }
        drop(work_rx);

        let listener_handle = {
            let stop_c = stop.clone();
            let active_c = active.clone();
            match thread::Builder::new()
                .name("duja-ipc-listener".to_owned())
                .spawn(move || listener_loop(&listener, &work_tx, &stop_c, &active_c))
            {
                Ok(handle) => handle,
                Err(e) => {
                    // The closure never ran, so its captured `work_tx` was dropped
                    // by the failed spawn; that ends the workers once the channel
                    // drains. Stop, join them, unlink.
                    stop.store(true, Ordering::Release);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    unlink_if_ours(&path, identity);
                    return Err(IpcTransportError::Io(e.to_string()));
                }
            }
        };

        Ok(PipeServer {
            stop,
            listener: Some(listener_handle),
            workers,
            socket_path: path,
            socket_identity: identity,
            _pin: pin,
        })
    }

    /// Stop the server: unblock the listener, join every thread, unlink the
    /// socket. Idempotent and also run on drop.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        if self.stop.swap(true, Ordering::AcqRel) {
            return; // already shut down
        }
        // The non-blocking accept loop re-checks the stop flag every `WAIT_SLICE`
        // and exits; joining it drops the work sender, which ends the workers.
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        unlink_if_ours(&self.socket_path, self.socket_identity);
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// The listener thread: accept a connection, hand it to a worker, repeat —
/// bounded by the in-flight cap.
fn listener_loop(
    listener: &UnixListener,
    work_tx: &Sender<UnixStream>,
    stop: &AtomicBool,
    active: &AtomicUsize,
) {
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if active.load(Ordering::Acquire) >= MAX_CONNECTIONS as usize {
            // At capacity: excess connections wait in the kernel backlog. Back off
            // briefly and re-check, re-evaluating the stop flag each pass.
            thread::sleep(POLL_INTERVAL);
            continue;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                if stop.load(Ordering::Acquire) {
                    break; // `stream` drops, closing it
                }
                // The handler wraps the accepted stream in a `SockStream`, which
                // sets its own non-blocking mode for the `poll`-gated I/O.
                active.fetch_add(1, Ordering::AcqRel);
                if work_tx.send(stream).is_err() {
                    active.fetch_sub(1, Ordering::AcqRel);
                    break; // workers gone
                }
            }
            // No pending connection: poll again after a slice, honouring stop.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(WAIT_SLICE),
            // A signal interrupted `accept`: retry immediately.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // A transient fault: back off briefly so we never busy-spin.
            Err(_) => thread::sleep(POLL_INTERVAL),
        }
    }
}

/// A handler thread: verify each accepted peer, serve exactly one exchange, then
/// release its in-flight slot.
fn worker_loop(
    rx: &Receiver<UnixStream>,
    stop: &Arc<AtomicBool>,
    active: &AtomicUsize,
    handler: &(dyn Fn(Request) -> Response + Send + Sync),
) {
    while let Ok(stream) = rx.recv() {
        if !stop.load(Ordering::Acquire) {
            serve_connection(stream, stop, handler);
        }
        // Whether we served it or dropped it on shutdown, the slot is now free.
        active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Verify the peer, then run one exchange; the stream closes on drop.
///
/// No flush follows the exchange: once the response `write` completes, the bytes
/// are in the socket's send buffer. A well-behaved client reads them before
/// closing; a client that connects and never reads simply loses the response
/// when the handle closes — acceptable within the same-user threat model (it only
/// harms itself and cannot pin the handler).
fn serve_connection(
    stream: UnixStream,
    stop: &Arc<AtomicBool>,
    handler: &(dyn Fn(Request) -> Response + Send + Sync),
) {
    if !peer_allowed(peer_euid(stream.as_raw_fd()), our_euid()) {
        // Refuse a foreign-uid peer silently; nothing trustworthy to answer.
        return; // `stream` drops, closing it
    }
    // One deadline for the whole request read so a dribbling peer cannot renew it
    // per syscall and pin this handler (see `with_read_deadline`).
    let mut stream = SockStream::server(stream, stop.clone()).with_read_deadline(READ_TIMEOUT);
    let _ = duja_ipc::serve_once(&mut stream, handler);
}

/// Bind a listener at `path`, preparing its `0700` parent directory and taking
/// over a stale socket if one is present — the whole of it under a lock.
///
/// # The race this closes
///
/// Probe → `remove_file` → `bind` is three steps against a shared name, and two
/// instances starting together interleave them. Both probe the stale inode and
/// see a refusal; both unlink; both bind. The second `bind` creates a *new* inode
/// at the same path, so the first instance is left holding a listener no client
/// can ever reach — it accepts nothing, reports no error, and looks healthy.
/// Duja's own single-instance guard does not prevent this: it degrades to "first"
/// whenever its lock cannot be taken, and `dujactl` reaches the socket by path,
/// not by process.
///
/// # Why the lock covers the *plain* bind too
///
/// An earlier version of this fix locked only the takeover, on the reasoning that
/// a plain `bind` is atomic and needs no help. It is not, and the residual window
/// is real. [`UnixListener::bind`] is `socket` → `bind` → `listen`, and an
/// instance that has completed `bind` but not yet `listen` is indistinguishable
/// from a stale inode: the path exists, and a probing `connect` gets
/// `ECONNREFUSED`. So a starter parked in that two-syscall gap could have its
/// fresh socket unlinked by a concurrent takeover, and both would end up
/// "bound" — the exact defect this exists to prevent, reached from one step
/// earlier. Locking the whole sequence makes "every instance takes the lock
/// first" true rather than nearly true.
///
/// The cost is one `open` and one `flock` per server start. The consequence is
/// that a filesystem without working `flock` fails the server closed rather than
/// starting it unserialised; every path this resolves to is tmpfs, `/tmp` or
/// APFS, where `flock` works.
fn bind_listener(path: &Path) -> Result<BoundSocket, IpcTransportError> {
    prepare_socket_dir(path).map_err(|e| IpcTransportError::Io(e.to_string()))?;
    let _guard = BindLock::acquire(path)?;
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => takeover_bind(path)?,
        Err(e) => return Err(IpcTransportError::Io(e.to_string())),
    };
    // Read the identity and take the pin here, still under the lock, so both name
    // the same inode by construction rather than by argument. An earlier version
    // read the identity in the caller and claimed it was still locked, which was
    // false — the guard is a local and dies with this function — and a later one
    // took the pin there too, which was safe only because a bound socket is
    // already listening and a concurrent probe would therefore refuse. Sound, but
    // one subtlety more than this needs.
    let identity = socket_identity(path);
    let pin = pin_inode(path);
    Ok(BoundSocket {
        listener,
        identity,
        pin,
    })
}

/// What [`bind_listener`] hands back: the listener, plus the two things that make
/// a later unlink safe, both established under the bind lock.
struct BoundSocket {
    listener: UnixListener,
    /// `(dev, ino)` of the socket as bound.
    identity: Option<(u64, u64)>,
    /// A descriptor keeping that inode from being freed. See [`pin_inode`].
    pin: Option<std::os::fd::OwnedFd>,
}

/// The `AddrInUse` path, called with [`BindLock`] held: probe for a live owner,
/// else unlink the stale inode and rebind.
///
/// Because the lock covers the caller's plain `bind` as well, the inode found
/// here was almost always either bound by a fully-started server or left behind by
/// a dead one. One further case exists and is worth naming rather than denying: a
/// **live** server whose listen backlog is full. Reaching it needs enough queued
/// connections from a process of the same uid to fill that backlog — std passes
/// `listen(fd, -1)`, so the depth is `somaxconn` on Linux (4096 by default since
/// 5.4) and 128 on Apple — which is inside the trust boundary and so is self-harm
/// rather than an attack, and the deliberate flood is still required: this
/// module's listener stopping at [`MAX_CONNECTIONS`] is four connections in
/// flight, not a full backlog, and 128 or 4096 more have to be queued on top of
/// it. What the cap changes is only that those queued connections are not being
/// drained *while* it holds, which is why the window is wider than the count
/// suggests — not that the state arrives on its own.
///
/// The two platforms then fail differently, and **only one of the two failures
/// closes here.**
///
/// - **Linux: fixed, and not merely bounded.** `unix_stream_connect` used to find
///   the receive queue full and wait, with no send timeout on a blocking socket,
///   for an `accept` that may never come — the one unbounded wait left in a module
///   that bounds everything else, and it ran under [`BindLock`], so it timed every
///   other starter out at [`LOCK_WAIT`] too. The probe is non-blocking now, and
///   the kernel answers `EAGAIN` immediately. That errno is *positive evidence of a
///   listener* rather than a timeout to wait out: `unix_recvq_full_lockless` is
///   reached only after `unix_find_other` has resolved a live peer socket. So the
///   wait is not shortened, it is gone, and the answer it produces is the right
///   one.
/// - **The BSDs: not fixed, and a retry budget cannot fix it here.** XNU's
///   `unp_connect` answers `ECONNREFUSED` when `so_qlen >= so_qlimit`, which is
///   byte-identical to the answer for a dead inode. A budget long enough to
///   outlast a live server's worst-case drain is the obvious repair and it does
///   not fit: draining takes up to [`MAX_CONNECTIONS`] exchanges through
///   [`MAX_HANDLER_THREADS`] threads, each bounded by [`READ_TIMEOUT`] and
///   [`WRITE_TIMEOUT`], which is longer than [`LOCK_WAIT`] — and this probe runs
///   **holding** [`BindLock`], so a budget that could work would time out every
///   other starter to buy one instance a better guess. A shorter budget is a
///   heuristic that reads as a fix. [`D-076`] carries what is left, narrowed to
///   this.
///
/// What the refusal arm *does* gain is [`unlink_target_is_ours`]: a refusal says
/// "nothing is listening", never "this is mine to delete", and those are different
/// claims. The row asked for an `fstat` owner check and that is not available —
/// there is no descriptor for the peer on a refusal, and one for our own probe
/// socket describes the wrong thing. The check that guards [`std::fs::remove_file`]
/// is a check on what `remove_file` resolves, which is an `lstat` of the path.
///
/// [`D-076`]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-076
fn takeover_bind(path: &Path) -> Result<UnixListener, IpcTransportError> {
    let bind_now = || UnixListener::bind(path).map_err(|e| IpcTransportError::Io(e.to_string()));
    match probe_liveness(path) {
        // Someone is listening, or something answered in a way that does not rule
        // it out. Either way this is not ours to take over.
        Liveness::Live => Err(IpcTransportError::Io(
            "another duja IPC server is already listening on this socket".to_owned(),
        )),
        Liveness::Undecidable => Err(IpcTransportError::Io(
            "could not determine whether the socket at this path is live; refusing \
             to unlink it"
                .to_owned(),
        )),
        // The inode went away between the caller's `AddrInUse` and this probe. The
        // unlink would fail `NotFound` and lose a bind that would have worked.
        Liveness::Vanished => bind_now(),
        Liveness::Stale => {
            unlink_target_is_ours(path)?;
            std::fs::remove_file(path).map_err(|e| IpcTransportError::Io(e.to_string()))?;
            bind_now()
        }
    }
}

/// How long [`probe_liveness`] may spend before it gives up and refuses to unlink.
///
/// **Not a latency budget, and none of the common paths spend it.** A connect to a
/// live listener, to a full backlog, or to a dead inode all answer on the first
/// syscall. This bounds one arm only — a connect the kernel reports as still in
/// flight — which for `AF_UNIX` is rare, since both kernels complete the handshake
/// inside `connect` when there is queue room.
///
/// Two things size it, and neither is a measurement: it runs while [`BindLock`] is
/// held, so it must stay far below [`LOCK_WAIT`] or a slow probe becomes every
/// concurrent starter's problem — the escalation that put this row in `debt.md`;
/// and it must outlast the readiness of a connect that has already been queued,
/// which is a wakeup rather than any I/O. 250 ms is a twentieth of `LOCK_WAIT` and
/// several orders of magnitude above that wakeup. It is a reasoned bound and it
/// says so rather than implying it was timed.
const PROBE_BUDGET: Duration = Duration::from_millis(250);

/// What one `connect` attempt says about the endpoint.
///
/// Split out from the verdict so the mapping from errno to meaning is a pure
/// function, testable on every lane rather than only where a socket can be made to
/// misbehave.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attempt {
    /// The connect completed. Only a live listener produces this.
    Connected,
    /// The listener's receive queue is full. **Also only a live listener**, which
    /// is the whole reason this is not folded in with `Refused`: on Linux it is
    /// the answer that used to be an unbounded wait.
    BacklogFull,
    /// The kernel took the connect but has not finished it. Poll, then ask the
    /// socket what happened.
    InFlight,
    /// The kernel refused. On Linux that means no listener; on the BSDs it means
    /// either no listener or a full backlog, and nothing here can tell which.
    Refused,
    /// Nothing at the path.
    Vanished,
    /// Anything else — a permission failure, a name too long, an unexpected errno.
    Undecidable,
}

/// The whole probe's answer, which is a decision about whether to destroy an inode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Liveness {
    /// Do not unlink: something is listening, or might be.
    Live,
    /// Unlink is permitted, subject to [`unlink_target_is_ours`].
    Stale,
    /// Nothing to unlink; bind straight away.
    Vanished,
    /// Do not unlink: the probe could not answer.
    Undecidable,
}

/// POSIX permits `EWOULDBLOCK` to differ from `EAGAIN`, and [`classify`] names
/// only the latter — naming both is an unreachable pattern the compiler rejects
/// on every target this builds for. On a target where they did differ, a full
/// backlog would fall through to [`Attempt::Undecidable`]: safe, since that
/// refuses to unlink, but the Linux arm would silently stop working and nothing
/// would say why. This is the line that notices.
const _: () = assert!(libc::EAGAIN == libc::EWOULDBLOCK);

/// Map a raw `connect` errno to what it says.
///
/// `EINTR` joins the in-flight arm rather than being retried: an interrupted
/// `connect` continues in the background, so the socket — not a second
/// `connect` — is what has the answer.
///
/// `ENOTSOCK` is not listed and lands in [`Attempt::Undecidable`] deliberately.
/// It is **XNU's** answer for a path that exists and is not a socket, where Linux
/// answers `ECONNREFUSED`; `Undecidable` refuses to unlink, which is the same
/// outcome the Linux arm reaches through [`unlink_target_is_ours`], by a different
/// route. See that function for why the two routes matter.
const fn classify(raw: i32) -> Attempt {
    match raw {
        libc::ECONNREFUSED => Attempt::Refused,
        libc::EAGAIN => Attempt::BacklogFull,
        libc::EINPROGRESS | libc::EALREADY | libc::EINTR => Attempt::InFlight,
        libc::ENOENT => Attempt::Vanished,
        _ => Attempt::Undecidable,
    }
}

/// What one attempt settles, or `None` if the probe should look again.
///
/// Every uncertain answer resolves toward [`Liveness::Live`] or
/// [`Liveness::Undecidable`], because the action on the other side is an unlink and
/// there is no undoing one. A wrong `Live` costs a start that reports "already
/// listening"; a wrong `Stale` costs a running server its endpoint.
///
/// The `match` is wildcard-free on purpose: it is the one line a new [`Attempt`]
/// variant fails to compile at. If you are here because of that, the test module's
/// `ALL_ATTEMPTS` needs the variant too — nothing forces it, and the property test
/// that says "nothing but a refusal authorises an unlink" silently narrows to a
/// sample if it is forgotten.
const fn settle(attempt: Attempt) -> Option<Liveness> {
    match attempt {
        Attempt::Connected | Attempt::BacklogFull => Some(Liveness::Live),
        Attempt::Refused => Some(Liveness::Stale),
        Attempt::Vanished => Some(Liveness::Vanished),
        Attempt::Undecidable => Some(Liveness::Undecidable),
        // The only arm that asks for another look.
        Attempt::InFlight => None,
    }
}

/// Is anything listening at `path`?
///
/// **One connect, not a retry loop**, and that is a correctness point rather than
/// an economy. Every answer either kernel gives to an `AF_UNIX` `connect` is
/// synchronous — connected, `EAGAIN`, `ECONNREFUSED`, `ENOENT` — so a second
/// attempt would learn nothing the first did not. An earlier version looped, and
/// the loop was wrong twice over: it built a **fresh socket each pass**, so an
/// in-flight connect could never resolve across iterations, and every pass would
/// have left an abandoned half-open connection on the target's backlog. The only
/// wait that can pay for itself is on the socket that already has the connect,
/// and that lives inside [`attempt_connect`].
///
/// [`Instant::checked_add`] overflowing gets a branch it will never take, because
/// the alternative is handing a `None` to [`slice_until`], where `None` means "no
/// deadline, take a full slice" — which would turn the bounded wait below into an
/// unbounded one. That is the defect this whole change removes, reintroduced
/// through a type the two functions share, and "unreachable" is what both of this
/// row's bugs were called too.
fn probe_liveness(path: &Path) -> Liveness {
    let Some(deadline) = Instant::now().checked_add(PROBE_BUDGET) else {
        return Liveness::Undecidable;
    };
    match attempt_connect(path, Some(deadline)) {
        // The only unsettled attempt is one still in flight, and "slow" is not
        // "absent": something took the connect, so this is not ours to unlink.
        Ok(attempt) => settle(attempt).unwrap_or(Liveness::Live),
        Err(()) => Liveness::Undecidable,
    }
}

/// One non-blocking `connect`, including the `poll` + `SO_ERROR` follow-up when
/// the kernel reports the connect as still in flight.
///
/// `Err(())` means the probe itself failed — no socket, an unrepresentable path, a
/// `poll` fault — which the caller turns into [`Liveness::Undecidable`] rather than
/// into permission to unlink.
///
/// # The in-flight arm is not reachable on either kernel this builds for
///
/// Stated plainly rather than hedged as "rare", which is what this said first.
/// Linux's `unix_stream_connect` has no `-EINPROGRESS` in its path and cannot
/// return `EINTR` with `timeo == 0` because it never sleeps; XNU's `connectit`
/// raises `EINPROGRESS` only when `SS_ISCONNECTING` survives `soconnect`, and
/// `unp_connect` calls `soisconnected` synchronously. So `PROBE_BUDGET`, the poll
/// below and [`Attempt::InFlight`] are all dead today.
///
/// They are here anyway because the alternative is not "less code" but "a wrong
/// answer if a kernel ever changes its mind": without them an `EINPROGRESS` falls
/// into [`Attempt::Undecidable`], and a probe that cannot decide refuses to start
/// the server. The loop polls **the same socket** the connect is pending on, which
/// is the whole difference from the version that looped over fresh ones.
fn attempt_connect(path: &Path, deadline: Option<Instant>) -> Result<Attempt, ()> {
    let sock = probe_socket()?;
    let addr = rustix::net::SocketAddrUnix::new(path).map_err(|_| ())?;
    match rustix::net::connect(&sock, &addr) {
        Ok(()) => return Ok(Attempt::Connected),
        Err(e) => {
            let attempt = classify(e.raw_os_error());
            if attempt != Attempt::InFlight {
                return Ok(attempt);
            }
        }
    }
    // In flight: wait for writability on this socket, then ask it for the real
    // outcome. `SO_ERROR` is the only thing that distinguishes "connected" from
    // "refused" once `connect` has returned `EINPROGRESS`. `slice_until` is what
    // ends this: it answers `Err` once the deadline has passed.
    loop {
        let Ok(slice) = slice_until(deadline) else {
            return Ok(Attempt::InFlight);
        };
        // `Ok(false)` is the slice elapsing or `poll` being interrupted: fall
        // out of the match, re-check the deadline at the top, and wait on the
        // same socket again.
        match poll_ready(sock.as_raw_fd(), libc::POLLOUT, slice) {
            Ok(true) => break,
            Ok(false) => {}
            Err(_) => return Err(()),
        }
    }
    match rustix::net::sockopt::socket_error(&sock) {
        Ok(Ok(())) => Ok(Attempt::Connected),
        Ok(Err(e)) => Ok(classify(e.raw_os_error())),
        Err(_) => Err(()),
    }
}

/// A close-on-exec, non-blocking `AF_UNIX` stream socket.
///
/// `SocketFlags::NONBLOCK`/`CLOEXEC` are not taken here because rustix gates both
/// out on Apple — they are `SOCK_*` type bits that only Linux and the newer BSDs
/// accept in `socket()`. Two `fcntl`s set the same properties on both targets, so
/// this stays one code path rather than a `cfg` split.
///
/// `CLOEXEC` is not decoration, for the reason [`pin_inode`] gives: `duja-app`'s
/// tray has a live `exec` path, since "Restart" spawns a detached
/// `duja --relaunch` while the outgoing server is still up.
///
/// Setting it by `fcntl` leaves a window between the two syscalls that
/// `SOCK_CLOEXEC` would not, and that is stated rather than glossed: a `fork` +
/// `exec` landing inside it hands the child an unconnected probe socket it then
/// holds for its life. One leaked descriptor in an unrelated process, not a
/// correctness fault - the probe never carries data. Closing the window would
/// mean a `cfg` split that is atomic on Linux and unchanged on Apple, which buys
/// half a guarantee for a second code path on the arm that decides whether to
/// delete a socket.
fn probe_socket() -> Result<std::os::fd::OwnedFd, ()> {
    let sock = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::empty(),
        None,
    )
    .map_err(|_| ())?;
    rustix::io::fcntl_setfd(&sock, rustix::io::FdFlags::CLOEXEC).map_err(|_| ())?;
    let flags = rustix::fs::fcntl_getfl(&sock).map_err(|_| ())?;
    rustix::fs::fcntl_setfl(&sock, flags | rustix::fs::OFlags::NONBLOCK).map_err(|_| ())?;
    Ok(sock)
}

/// The last check before [`std::fs::remove_file`]: the thing about to be unlinked
/// must be a socket, and it must be ours.
///
/// A refusal answers "nothing is listening at this name". It does not answer "this
/// name is mine to delete", and the two get conflated because the common case makes
/// them coincide. They come apart in two ways that matter, and both end in a
/// deletion that erases evidence:
///
/// - the path is **not a socket**. Linux's `unix_find_other` refuses a
///   non-`S_ISSOCK` inode with `ECONNREFUSED`, indistinguishably from a dead
///   socket, so a regular file dropped into the endpoint's directory would be
///   deleted and bound over. Nothing in Duja puts one there, which is the point:
///   its presence means something has gone wrong, and unlinking it destroys the
///   only trace.
/// - the path is **not ours**. The directory is `0700` and this process owns it, so
///   a foreign-owned inode inside it is not reachable by the threat model — but the
///   check costs one `lstat` and the alternative is trusting an argument about
///   permissions at the moment a deletion happens.
///
/// # Which of the two arms below is inert, and where
///
/// The plain non-socket case is **Linux-only**. XNU's `unp_connect` rejects a
/// non-socket earlier and differently — `if (vp->v_type != VSOCK) error =
/// ENOTSOCK` — which [`classify`] sends to [`Attempt::Undecidable`], so
/// `takeover_bind` refuses before reaching this function at all.
///
/// So the only macOS route here is `ECONNREFUSED`, and the **vnode `unp_connect`
/// resolved** is in every case a socket: XNU raises that errno for a
/// null `v_socket` (a stale socket), for a listener that is not accepting —
/// `SO_ACCEPTCONN` clear, or `sonewconn` returning null — and on the
/// lock-reacquire path where `so_pcb` has gone. The argument is that shape rather
/// than a list of sites: it cannot reach any of them without having resolved a
/// `VSOCK` vnode first.
///
/// **"The vnode it resolved" is not "the inode this function checks", and two
/// versions of this paragraph treated them as one.** `unp_connect` looks the path
/// up with `FOLLOW`; [`std::fs::symlink_metadata`] does not. So a symlink at the
/// endpoint pointing at a stale socket elsewhere gives `ECONNREFUSED` from a
/// `VSOCK` vnode that is somebody else's inode in some other directory, while the
/// `lstat` here sees the **symlink** and the socket-type arm fires. That arm is
/// live on macOS, and it is the arm that refuses a planted symlink — for exactly
/// the reason the `lstat`-not-`stat` note below gives, which is why the two
/// paragraphs contradicted each other until now.
///
/// The **uid** arm is the inert one, and it is inert on **both** lanes rather than
/// on macOS: reaching it means the `lstat` saw a socket at this path, and putting a
/// foreign-owned socket inside a `0700` directory this euid owns needs either us or
/// root. What is macOS-specific is narrower — the socket-type arm loses its
/// regular-file route there (that is the `ENOTSOCK` above) and keeps only the
/// symlink one.
///
/// (Earlier versions got that split wrong in three different ways - a two-site
/// kernel list called "the only" routes, then both arms called dead, then the uid
/// arm called macOS-specific - and the count of versions is deliberately not here,
/// because a tally in a comment is what the three commits before this branch point
/// were each about. The counterexamples are the part worth keeping.)
///
/// The *function* is not dead on either lane regardless: the `symlink_metadata`
/// above both arms can fail if the inode vanishes between the probe and the
/// `lstat`, which is the window the note below admits.
///
/// Worth saying because the alternative is the shape this project rates worst: a
/// guard that reads as covering two platforms, whose test passes on both, and
/// which on one of them would pass just as well if it were deleted.
///
/// `lstat` rather than `stat`, and that difference is load-bearing rather than
/// stylistic: `remove_file` unlinks **the symlink**, not its target, so following
/// one here would check the wrong inode's ownership. A symlink also fails the
/// socket test, so a planted one is refused on both counts. Note the asymmetry with
/// [`socket_identity`], which uses `std::fs::metadata` and does follow.
///
/// The `lstat` and the `remove_file` are not atomic, so this narrows the window
/// rather than closing it — it is still an argument about permissions, made a few
/// microseconds earlier. What makes that acceptable is the same thing that makes
/// the whole module's inode handling acceptable: a `0700` parent this process
/// owns, and [`BindLock`] held across all of it. Said plainly because the sentence
/// above contrasts this check with "trusting an argument", and it is one.
fn unlink_target_is_ours(path: &Path) -> Result<(), IpcTransportError> {
    use std::os::unix::fs::FileTypeExt as _;

    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| IpcTransportError::Io(format!("cannot inspect {}: {e}", path.display())))?;
    if !meta.file_type().is_socket() {
        // Name the remedy. This refusal is permanent and self-heals nowhere: on
        // macOS the endpoint lives in Application Support, which archivers and
        // Migration Assistant restore as a plain file, and `duja-app` reacts to a
        // dead IPC server by logging a warning and running without it - so a user
        // who is not told to remove this gets a silent, permanent loss of the
        // control API. Recorded in the D-076 row as a residual, not a fix.
        return Err(IpcTransportError::Io(format!(
            "{} exists and is not a socket; refusing to unlink it - remove it by \
             hand if it is left over",
            path.display()
        )));
    }
    if meta.uid() != our_euid() {
        return Err(IpcTransportError::Io(format!(
            "{} is owned by another user; refusing to unlink it",
            path.display()
        )));
    }
    Ok(())
}

/// Take a descriptor that keeps `path`'s inode from being freed, so its number
/// cannot be handed to somebody else's socket while we still hold it.
///
/// # Why `O_PATH`, and why only Linux
///
/// The first version of this dup'd the *listening socket* instead. That pins the
/// inode — a bound `AF_UNIX` socket holds a `dget`/`mntget` on its path, so the
/// final `iput` (and with it `ext4_free_inode`, which clears the inode bitmap bit
/// that makes the number reusable) waits for the socket to be released — but it
/// also keeps the socket **connectable**, and that turned out to cost more than it
/// bought:
///
/// - If both handler threads die, `listener_loop` exits, but the socket stays in
///   `LISTEN`. `dujactl` then *connects successfully*, blocks for the read timeout
///   and exits with a server error — where before it got `ECONNREFUSED`,
///   translated to `NotRunning`, and **fell back to driving the hardware
///   directly**. That fallback is a documented degradation path, and a fix for a
///   shutdown race had no business removing it.
/// - During shutdown the same window makes a concurrent start fail with "another
///   duja IPC server is already listening", and `duja-app`'s IPC bridge does not
///   retry: it logs a warning and runs without IPC for that whole process.
///
/// `O_PATH` gives a descriptor that references the inode and nothing else — no
/// socket semantics, no `LISTEN` state, no connectability — so the pin costs
/// exactly what it should. It is Linux-only, which is also exactly where it is
/// needed: **ext4 and XFS recycle inode numbers**, while tmpfs allocates from a
/// monotonic counter (`get_next_ino`) and macOS's APFS assigns object identifiers
/// monotonically. So `$XDG_RUNTIME_DIR` and macOS are safe without it, and the
/// `/tmp/duja-<uid>` fallback — the case `docs/debt.md` row 57 calls a routine
/// cron/ssh/container condition — is the one that needs it.
///
/// `None` on failure rather than an error: the caller still has the `(dev, ino)`
/// comparison, which without the pin is merely very likely to be right instead of
/// certain. That is the behaviour every release before this one shipped, so
/// degrading to it is not a regression — but it is a degradation, and it is named
/// here so nobody reads the guarantee as unconditional.
/// `CLOEXEC` is not decoration: `rustix::fs::open` does **not** add it (unlike
/// every `std` descriptor in this module, which does), and `duja-app`'s tray has a
/// live `exec` path — "Restart" spawns a detached `duja --relaunch` while the
/// outgoing server is still up. Without the flag that child inherits the pin and
/// holds an about-to-be-unlinked inode for its whole life, and each restart
/// generation inherits its predecessor's. Harmless to correctness — a pinned inode
/// only makes the comparison more conservative — but it is an fd leak into an
/// unrelated long-lived process.
///
/// `NOFOLLOW` here means the documented `O_PATH | O_NOFOLLOW` idiom: it pins the
/// **symlink itself** rather than refusing one, which is the opposite of what
/// `NOFOLLOW` means at this module's other two uses. It is still the right choice
/// — without it a planted symlink would redirect the pin to an arbitrary object —
/// but note that [`socket_identity`] uses `std::fs::metadata`, which *follows*.
/// So for a path that is somehow a symlink the two would name different inodes and
/// the pin would protect nothing, degrading to the pre-pin guarantee. Unreachable
/// in practice: the directory is `0700` and the bind lock is held across both, so
/// only a same-uid process could arrange it.
#[cfg(target_os = "linux")]
fn pin_inode(path: &Path) -> Option<std::os::fd::OwnedFd> {
    rustix::fs::open(
        path,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()
}

/// No pin needed: this target's filesystems do not recycle the numbers
/// [`socket_identity`] compares. See the Linux variant for the full argument.
#[cfg(not(target_os = "linux"))]
fn pin_inode(_path: &Path) -> Option<std::os::fd::OwnedFd> {
    None
}

/// `(dev, ino)` of whatever is at `path`, or `None` if it cannot be read.
fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// Unlink `path` only if it is still the inode this server bound.
///
/// # The unlink this refuses to do
///
/// Shutdown used to `remove_file` the socket path unconditionally, which is a
/// second route to the same "two servers, one unreachable" defect the bind lock
/// closes — reached without any takeover at all:
///
/// 1. `P` joins its listener thread, closing the listening fd. The inode is still
///    at the path, but nothing is accepting on it.
/// 2. `Q` starts. Its bind gets `AddrInUse`, its probe gets `ECONNREFUSED`, so it
///    correctly judges the inode stale, unlinks it and binds its own. `Q` serves.
/// 3. `P` reaches its `remove_file` — and deletes **`Q`'s** socket.
///
/// `Q` is then live, healthy and unreachable. Comparing `(dev, ino)` against what
/// this server bound makes step 3 a no-op, and the comparison runs under the same
/// [`BindLock`] so it cannot be raced between the `stat` and the `unlink`.
///
/// # Why an inode *number* is not enough on its own
///
/// `(dev, ino)` only identifies an object while that object exists, and **ext4 and
/// XFS recycle inode numbers**. In step 1 above `P`'s fd closes while the link
/// count is still 1; `Q`'s `remove_file` in step 2 drops it to 0, the final `iput`
/// reaches `ext4_free_inode`, and the number goes back in the block group's inode
/// bitmap. `ext4_new_inode` then allocates the lowest free bit in that bitmap, so
/// `Q`'s rebind **can** land on the number `P` recorded, and `P` would match on a
/// coincidence and delete `Q`'s socket after all. (Not *likely* — the bitmap spans
/// the whole block group, typically 8192 inodes, not just this directory. The
/// guard has to be exact regardless.) The [`BindLock`] does not help: it orders
/// steps 2 and 3, it does not make the number unique.
///
/// This matters precisely where `docs/debt.md` row 57 says it would. tmpfs
/// allocates from a monotonic counter and APFS assigns object ids monotonically,
/// so `$XDG_RUNTIME_DIR` and macOS are immune — but the `/tmp/duja-<uid>`
/// fallback, the one the row calls "a routine cron/ssh/container condition" on
/// Linux, is usually ext4.
///
/// So on Linux [`PipeServer`] holds an `O_PATH` descriptor on the socket's inode
/// until after this runs ([`pin_inode`]). A descriptor referencing an inode keeps
/// it from being evicted, and `ext4_free_inode` only runs from
/// `ext4_evict_inode` — so while the pin is held the number cannot return, `Q`'s
/// rebind is *guaranteed* a different one, and the comparison is exact rather than
/// probable. Without a pin (a non-Linux target, or an `open` that failed) the
/// comparison degrades to what shipped before this change: very probably right.
///
/// An unreadable identity (either at bind or now) means the socket is already
/// gone or unstattable, so there is nothing to unlink and nothing to risk.
///
/// # Why unlink at all
///
/// This is a lot of machinery for a tidy-up, and the alternative — never unlink,
/// let the next start take the stale socket over — would be simpler and equally
/// safe. It is kept because the post-shutdown state is load-bearing for the
/// client: with the socket gone, `dujactl` gets `NotRunning` and falls back to
/// driving the hardware directly, which is the behaviour
/// `connect_to_absent_socket_reports_not_running` pins. A leftover socket would
/// answer `ECONNREFUSED` and reach the same place today, but only by accident of
/// how `connect` reports it.
fn unlink_if_ours(path: &Path, ours: Option<(u64, u64)>) {
    let Some(ours) = ours else { return };
    let Ok(_guard) = BindLock::acquire(path) else {
        // Cannot serialise, so cannot prove the inode is still ours. Leaving a
        // stale socket behind is recoverable (the next start takes it over);
        // deleting a live one is not.
        return;
    };
    if socket_identity(path) == Some(ours) {
        let _ = std::fs::remove_file(path);
    }
}

/// The path of the lock guarding a socket's bind/unlink sequence.
fn bind_lock_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// An exclusive advisory lock serialising the whole bind/unlink sequence across
/// instances.
///
/// The lock file is a sibling of the socket inside the same `0700` directory, so
/// it inherits that directory's access barrier and needs no separate one. It is
/// deliberately **not** the single-instance lock file: that one is held for the
/// whole process lifetime, so reusing it would leave the first instance's own bind
/// waiting on a lock it already holds — which, since this poll is bounded, means
/// failing to start after [`LOCK_WAIT`] rather than hanging, but failing all the
/// same.
///
/// **The lock file is never unlinked**, including on shutdown, and that is load
/// bearing rather than laziness. `flock` locks an *inode*, not a path: if one
/// instance held the lock while another unlinked the file and recreated it, the
/// second would take an uncontended lock on a brand-new inode and both would
/// believe they had exclusive access — reintroducing the race from one layer
/// down. An empty `0600` file inside an already-private directory is the cheap
/// side of that trade.
///
/// # Bounded, not blocking
///
/// A holder does at most a probe, an unlink and two binds, so honest contention
/// clears in microseconds and [`LOCK_WAIT`] is enormous by comparison. The reason
/// it is bounded at all is that `flock` has no notion of *whose* lock it is
/// waiting on: an unbounded `LockExclusive` would hang startup forever on a lock
/// file another user planted and held. `unix_dir`'s writable-directory refusal
/// makes that unreachable, and this makes it survivable — the same
/// defence-in-depth every other wait in this module has.
struct BindLock {
    /// Held open for the lifetime of the guard; closing it releases the `flock`.
    _file: std::fs::File,
}

impl BindLock {
    /// Take the exclusive lock guarding `socket`'s bind sequence.
    fn acquire(socket: &Path) -> Result<Self, IpcTransportError> {
        let path = bind_lock_path(socket);
        let file = crate::unix_dir::open_private_file(&path)
            .map_err(|e| IpcTransportError::Io(format!("cannot open {}: {e}", path.display())))?;

        let deadline = Instant::now().checked_add(LOCK_WAIT);
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(BindLock { _file: file }),
                Err(e) if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::AGAIN => {}
                Err(e) => {
                    return Err(IpcTransportError::Io(format!(
                        "cannot lock {}: {e}",
                        path.display()
                    )));
                }
            }
            // `is_none_or`, deliberately the opposite of `connect_named`'s
            // `is_some_and` on the same shape: there a `None` deadline (an
            // `Instant` overflow) means "no timeout", here it means "give up now".
            // Fail-closed is right for a lock — the alternative is an unbounded
            // wait, which is the thing this loop exists to prevent. Unreachable
            // either way: `Instant` is boot-relative on both targets.
            if deadline.is_none_or(|d| Instant::now() >= d) {
                return Err(IpcTransportError::Io(format!(
                    "{} was held by another process for longer than {LOCK_WAIT:?}",
                    path.display()
                )));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Prepare `path`'s parent as a private `0700` directory Duja owns.
///
/// The parent is always a directory dedicated to Duja (`.../duja/` or
/// `/tmp/duja-<uid>/`), never a shared system directory, because every resolved
/// path nests the socket under such a component.
///
/// This used to create the directory recursively and `chmod` it, which accepted a
/// pre-existing directory belonging to anybody; it now refuses one that is not
/// ours and private. See [`crate::unix_dir`] for the rule and for how durable the
/// check is.
fn prepare_socket_dir(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) => crate::unix_dir::ensure_private_dir(parent),
        None => Ok(()),
    }
}

/// Set the permission bits of `path` to `mode`.
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

// -- The client -----------------------------------------------------------

/// A connected IPC client.
///
/// Holds the connected socket stream; [`request`](Self::request) performs one
/// exchange. The socket closes on drop.
pub struct PipeClient {
    stream: SockStream,
}

impl PipeClient {
    /// Connect to the running server on the default per-user socket path.
    ///
    /// # Errors
    /// [`IpcTransportError::NotRunning`] if no server is listening,
    /// [`IpcTransportError::Busy`] if the connection could not be established
    /// within the timeout, or [`IpcTransportError::Io`] on any other transport
    /// failure.
    pub fn connect(timeout: Duration) -> Result<Self, IpcTransportError> {
        Self::connect_named(&default_pipe_name(), timeout)
    }

    /// Connect to an explicit socket path (test seam).
    ///
    /// # Errors
    /// As [`connect`](Self::connect).
    pub fn connect_named(name: &str, timeout: Duration) -> Result<Self, IpcTransportError> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            match UnixStream::connect(name) {
                Ok(stream) => {
                    return Ok(PipeClient {
                        stream: SockStream::client(stream),
                    });
                }
                Err(e) => match e.kind() {
                    // No socket file, or a socket with no live listener: the app
                    // is not running. `dujactl` falls back to direct access.
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                        return Err(IpcTransportError::NotRunning);
                    }
                    // The backlog was momentarily full: retry within the budget.
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => {
                        if deadline.is_some_and(|dl| Instant::now() >= dl) {
                            return Err(IpcTransportError::Busy);
                        }
                        thread::sleep(POLL_INTERVAL);
                    }
                    _ => return Err(IpcTransportError::Io(e.to_string())),
                },
            }
        }
    }

    /// Send one request and read the server's response.
    ///
    /// # Errors
    /// [`IpcTransportError::Protocol`] on a framing/version/validation failure
    /// during the exchange (including a mid-exchange transport fault).
    pub fn request(&mut self, request: &Request) -> Result<Response, IpcTransportError> {
        Ok(duja_ipc::exchange(&mut self.stream, request)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_path_is_per_user_and_well_formed() {
        let name = default_pipe_name();
        assert!(name.contains("duja"), "name = {name}");
        assert!(name.ends_with("ctl.sock"), "name = {name}");
    }

    #[test]
    fn peer_allowed_only_for_matching_uid() {
        assert!(peer_allowed(Some(1000), 1000));
        assert!(!peer_allowed(Some(0), 1000), "root peer must be refused");
        assert!(!peer_allowed(Some(1001), 1000), "other user refused");
        assert!(!peer_allowed(None, 1000), "unreadable creds refused");
    }

    #[test]
    fn tmp_fallback_is_per_uid_and_dedicated() {
        let path = tmp_fallback(1000);
        assert_eq!(path, PathBuf::from("/tmp/duja-1000/ctl.sock"));
        // The parent is a Duja-owned directory, never a shared system dir.
        assert_eq!(path.parent().unwrap(), Path::new("/tmp/duja-1000"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_path_uses_application_support_then_tmp() {
        let with_home = macos_socket_path(Some(Path::new("/Users/alice")), 501);
        assert_eq!(
            with_home,
            PathBuf::from("/Users/alice/Library/Application Support/duja/ctl.sock")
        );
        assert_eq!(
            macos_socket_path(None, 501),
            PathBuf::from("/tmp/duja-501/ctl.sock")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_path_uses_xdg_runtime_then_tmp() {
        let with_xdg = linux_socket_path(Some(Path::new("/run/user/1000")), 1000);
        assert_eq!(with_xdg, PathBuf::from("/run/user/1000/duja/ctl.sock"));
        assert_eq!(
            linux_socket_path(None, 1000),
            PathBuf::from("/tmp/duja-1000/ctl.sock")
        );
    }

    // --- D-076: the liveness probe that decides whether to unlink ---

    /// Every attempt this probe can produce, so the property test below is a
    /// closed enumeration rather than a sample.
    ///
    /// **Closed by convention, not by the compiler**, and the difference is worth
    /// stating in a file about checks that cannot fail. An earlier version of this
    /// doc said a new `Attempt` variant "fails to compile, because the array's
    /// length is asserted" — nothing is asserted; `[Attempt; 6]` is a type
    /// annotation on a hand-written literal. What a seventh variant really breaks
    /// is [`super::settle`]'s wildcard-free `match`, and whoever adds an arm there
    /// has to remember this list too. `settle`'s own doc points back here for
    /// exactly that reason.
    const ALL_ATTEMPTS: [Attempt; 6] = [
        Attempt::Connected,
        Attempt::BacklogFull,
        Attempt::InFlight,
        Attempt::Refused,
        Attempt::Vanished,
        Attempt::Undecidable,
    ];

    /// Each errno maps to what it *proves*, not to what it looks like.
    ///
    /// The one worth reading is `EAGAIN`. It reads like "try again" and it is not:
    /// `unix_stream_connect` reaches that branch only after `unix_find_other` has
    /// resolved a peer *and* that peer has been checked to be in `TCP_LISTEN`, so
    /// it is positive evidence of a live server. Treating it as a retry is what
    /// made this an unbounded wait.
    #[test]
    fn a_connect_errno_classifies_by_what_it_proves() {
        assert_eq!(classify(libc::ECONNREFUSED), Attempt::Refused);
        assert_eq!(classify(libc::EAGAIN), Attempt::BacklogFull);
        assert_eq!(classify(libc::EINPROGRESS), Attempt::InFlight);
        assert_eq!(classify(libc::EALREADY), Attempt::InFlight);
        assert_eq!(classify(libc::EINTR), Attempt::InFlight);
        assert_eq!(classify(libc::ENOENT), Attempt::Vanished);
        // Not an exhaustive list of what cannot happen - a representative one.
        // Everything unrecognised must land somewhere that refuses to unlink.
        assert_eq!(classify(libc::EACCES), Attempt::Undecidable);
        assert_eq!(classify(libc::ENOTSOCK), Attempt::Undecidable);
        assert_eq!(classify(0), Attempt::Undecidable);
    }

    /// **A refusal is the only thing that may authorise an unlink.**
    ///
    /// Stated as a property over the closed set rather than as six equalities,
    /// because the failure this guards against is somebody adding a seventh
    /// attempt and reaching for `Stale` to make a case work. Six equalities would
    /// all still pass.
    #[test]
    fn nothing_but_a_refusal_authorises_an_unlink() {
        for attempt in ALL_ATTEMPTS {
            if attempt == Attempt::Refused {
                continue;
            }
            assert_ne!(
                settle(attempt),
                Some(Liveness::Stale),
                "{attempt:?} authorised an unlink; only a refusal may, because the \
                 action on the other side destroys a running server's endpoint and \
                 there is no undoing it"
            );
        }
        assert_eq!(settle(Attempt::Refused), Some(Liveness::Stale));
    }

    /// A full backlog is a live server, and settling on it is what removed the
    /// wait rather than shortening it.
    #[test]
    fn a_full_backlog_settles_as_live_without_waiting() {
        assert_eq!(settle(Attempt::BacklogFull), Some(Liveness::Live));
        assert_eq!(settle(Attempt::Connected), Some(Liveness::Live));
        // The one arm that asks for another look, and the only reason
        // `PROBE_BUDGET` exists at all.
        assert_eq!(settle(Attempt::InFlight), None);
    }

    /// The probe's bound has to be small against the lock it is held under, or
    /// bounding it solves nothing: `#114` widened this from "this instance stalls"
    /// to "every concurrent starter stalls", and that is the half a budget fixes.
    #[test]
    fn the_probe_budget_is_small_against_the_bind_lock() {
        assert!(
            PROBE_BUDGET.saturating_mul(4) < LOCK_WAIT,
            "PROBE_BUDGET {PROBE_BUDGET:?} is not comfortably inside LOCK_WAIT \
             {LOCK_WAIT:?}"
        );
    }

    /// How long a test gives the probe before calling it hung.
    ///
    /// Not a latency budget: the probe's own bound is `PROBE_BUDGET` and the
    /// paths these tests take do not spend it. This exists so the *historical*
    /// defect - a blocking `connect` into a full backlog, which waits for an
    /// `accept` that never comes - surfaces as a named failing test rather than
    /// as a hung suite.
    const PROBE_JOIN_DEADLINE: Duration = Duration::from_secs(3);

    /// Run `takeover_bind` on its own thread and refuse to wait forever for it.
    ///
    /// The thread is deliberately not joined on the timeout path: the historical
    /// defect leaves it parked in `connect` with no way to interrupt it, and a
    /// test that joined would inherit exactly the hang it is reporting. It is a
    /// leaked thread in a failing test process, which is the cheaper of the two.
    fn takeover_bind_bounded(path: &Path) -> Result<UnixListener, IpcTransportError> {
        let (tx, rx) = bounded(1);
        let probed = path.to_path_buf();
        thread::Builder::new()
            .name("d076-probe".to_owned())
            .spawn(move || {
                let _ = tx.send(takeover_bind(&probed));
            })
            .expect("the probe thread must spawn");
        match rx.recv_timeout(PROBE_JOIN_DEADLINE) {
            Ok(result) => result,
            Err(e) => panic!(
                "the liveness probe did not return within {PROBE_JOIN_DEADLINE:?} ({e}): \
                 it is waiting on a peer that never answers, which is the unbounded \
                 wait D-076 is about"
            ),
        }
    }

    /// A non-blocking `connect`, for filling a backlog without blocking on the
    /// connect that finds it full - which is the very hang under test.
    ///
    /// Gated with its only caller rather than left ungated: a helper used solely
    /// by a `cfg(target_os = "linux")` test is dead code on the other two lanes,
    /// and `clippy -D warnings` fails there while Linux stays green. That is
    /// [`D-045`](https://github.com/itabajah/duja/blob/main/docs/debt-archive.md#d-045)'s
    /// lesson arriving a second time, and it cost a cross-check round here too.
    #[cfg(target_os = "linux")]
    fn try_connect(path: &Path) -> Result<std::os::fd::OwnedFd, i32> {
        let sock = rustix::net::socket_with(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::empty(),
            None,
        )
        .expect("a unix socket must be creatable");
        let flags = rustix::fs::fcntl_getfl(&sock).expect("F_GETFL must work");
        rustix::fs::fcntl_setfl(&sock, flags | rustix::fs::OFlags::NONBLOCK)
            .expect("F_SETFL must work");
        let addr = rustix::net::SocketAddrUnix::new(path).expect("the path must fit in sun_path");
        match rustix::net::connect(&sock, &addr) {
            Ok(()) => Ok(sock),
            Err(e) => Err(e.raw_os_error()),
        }
    }

    /// A listener bound at `path` with the smallest backlog the kernel accepts,
    /// so filling it costs a handful of connects rather than `somaxconn` of them.
    ///
    /// `std`'s `UnixListener::bind` passes `listen(fd, -1)` on Linux, which the
    /// kernel clamps to `somaxconn` (4096 since 5.4); on Apple it passes the
    /// literal `128`. Neither is fillable in a test, and that is the whole reason
    /// this goes through `rustix` instead.
    #[cfg(target_os = "linux")]
    fn listener_with_backlog(path: &Path, backlog: i32) -> std::os::fd::OwnedFd {
        let sock = rustix::net::socket_with(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::empty(),
            None,
        )
        .expect("a unix socket must be creatable");
        let addr = rustix::net::SocketAddrUnix::new(path).expect("the path must fit in sun_path");
        rustix::net::bind(&sock, &addr).expect("the listener must bind");
        rustix::net::listen(&sock, backlog).expect("the listener must listen");
        sock
    }

    /// **A live server whose backlog is full must not be read as stale.**
    ///
    /// Linux only, and the gate is the mechanism rather than convenience: this
    /// pins the arm where a non-blocking `connect` answers `EAGAIN`, which only
    /// happens when a listener exists and its receive queue is full
    /// (`unix_stream_connect`'s `unix_recvq_full_lockless` branch). XNU's
    /// `unp_connect` answers `ECONNREFUSED` for the same condition, which is
    /// indistinguishable from a dead inode and is the half of this row that does
    /// **not** close - see the row for why no budget separates them here.
    /// (`unp_connectat` is FreeBSD's name for it and is what this said first;
    /// reading the wrong kernel is also what produced the `ECONNREFUSED`-on-both-
    /// lanes claim in the sibling test below.)
    ///
    /// Two failures, one test, matching the two the row names:
    ///
    /// - the probe never returning, which is the unbounded wait, reported by
    ///   `takeover_bind_bounded` rather than by a hung suite;
    /// - the socket being unlinked, which is a live server's endpoint destroyed.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_live_server_with_a_full_backlog_is_not_unlinked() {
        let dir = tempfile::tempdir().expect("a temp dir must be creatable");
        let path = dir.path().join("ctl.sock");
        let _listener = listener_with_backlog(&path, 1);

        // Nothing ever accepts here, which is not an artificial cruelty: the
        // listener stops accepting at `MAX_CONNECTIONS` by design, so a live
        // server with a full backlog and no `accept` in flight is a state this
        // module produces on purpose.
        let mut queued = Vec::new();
        let mut refusal = None;
        for _ in 0..64_u32 {
            match try_connect(&path) {
                Ok(sock) => queued.push(sock),
                Err(errno) => {
                    refusal = Some(errno);
                    break;
                }
            }
        }
        // Assert the *reason*, not merely that the loop stopped. Any errno would
        // satisfy "something failed", so this test would pass on an `ENOENT`
        // from a path that never existed - a check that cannot fail, which is
        // the shape this phase keeps finding.
        assert_eq!(
            refusal,
            Some(libc::EAGAIN),
            "the backlog did not fill in 64 connects, so this test is not \
             exercising the arm it names"
        );

        let outcome = takeover_bind_bounded(&path);

        assert!(
            outcome.is_err(),
            "a listener with a full backlog is live, and takeover_bind must \
             refuse rather than take the socket over"
        );
        assert!(
            path.exists(),
            "the live server's socket was unlinked: every client that reaches it \
             by path is now cut off, which is the defect this module exists to \
             prevent"
        );
    }

    /// **A symlink at the endpoint is refused, and this is the arm three review
    /// rounds argued about.**
    ///
    /// Both kernels resolve a `connect` path with `FOLLOW` - Linux through
    /// `unix_find_other`'s `kern_path`, XNU through `NDINIT(..., FOLLOW |
    /// LOCKLEAF, ...)` - while `symlink_metadata` does not. So a symlink pointing
    /// at a stale socket somewhere else answers `ECONNREFUSED` from a vnode that
    /// is a socket, and `unlink_target_is_ours` then inspects the **symlink** and
    /// refuses. Without it, `remove_file` would unlink the symlink and `bind` would
    /// put duja's endpoint wherever an attacker aimed it.
    ///
    /// It exists because a doc comment claimed this arm was dead on macOS and a
    /// review had to read two kernels to show otherwise. An argument that needs a
    /// kernel source read to check is one a test should be making, and this is
    /// cheap enough that there was no excuse.
    #[test]
    fn a_symlink_at_the_endpoint_is_not_followed_and_not_unlinked() {
        let dir = tempfile::tempdir().expect("a temp dir must be creatable");
        let elsewhere = dir.path().join("elsewhere.sock");
        let endpoint = dir.path().join("ctl.sock");

        // A real socket inode with nothing listening on it: `std` does not unlink
        // on drop, so the file outlives the listener and a connect to it is
        // refused - the shape a stale socket has.
        drop(UnixListener::bind(&elsewhere).expect("the decoy listener must bind"));
        std::os::unix::fs::symlink(&elsewhere, &endpoint).expect("the symlink must be creatable");

        let error = takeover_bind_bounded(&endpoint).map_or_else(
            |e| e.to_string(),
            |_| "Ok(_): the guard did not refuse at all".to_owned(),
        );

        // Pin the *route*, not just the refusal. Both verdicts that refuse produce
        // an error, so asserting `is_err` alone would pass on `Undecidable` - which
        // an `EACCES` from the decoy's mode reaches without ever calling
        // `unlink_target_is_ours`. Its sibling above pins its errno for the same
        // reason.
        assert!(
            error.contains("is not a socket"),
            "expected the socket-type guard to refuse, got: {error}"
        );
        assert!(
            endpoint
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink()),
            "the symlink was replaced, so duja's endpoint is now wherever it pointed"
        );
        // There is deliberately no "the target survived" assertion. `remove_file`
        // is `unlink(2)`, which never follows the final symlink, so no path through
        // `takeover_bind` can reach `elsewhere` - an assertion on it could not fire
        // under any mutation, which is the defect this test exists to argue about.
    }

    /// **A refusal is not enough on its own: the thing about to be unlinked has
    /// to be our socket.**
    ///
    /// A regular file at the socket path is refused on both lanes, and **by two
    /// different mechanisms** - which the first version of this doc got wrong, and
    /// which matters for what the macOS lane's green is worth:
    ///
    /// - **Linux** answers `ECONNREFUSED` (`unix_find_other` rejects a
    ///   non-`S_ISSOCK` inode with exactly that), indistinguishable from a dead
    ///   socket, so the old probe deleted the file and bound over it. Here the
    ///   refusal reaches `unlink_target_is_ours`, which is what stops it.
    /// - **macOS** answers `ENOTSOCK` (`unp_connect`'s `vp->v_type != VSOCK`),
    ///   which `classify` sends to `Undecidable`, so `takeover_bind` refuses
    ///   before `unlink_target_is_ours` is reached at all.
    ///
    /// So this test passes on macOS **with the guard deleted**. It is a behaviour
    /// test on both lanes and a test of the guard on one, and saying so is the
    /// point: a check whose test cannot fail on a platform is not protecting that
    /// platform, whatever the green says.
    ///
    /// Nothing in Duja puts a regular file there - which is the point. The path is
    /// inside a `0700` directory this process owns, so a file appearing there means
    /// something has gone wrong that unlinking would erase.
    #[test]
    fn a_refusal_from_something_that_is_not_a_socket_is_not_believed() {
        let dir = tempfile::tempdir().expect("a temp dir must be creatable");
        let path = dir.path().join("ctl.sock");
        std::fs::write(&path, b"not a socket").expect("the file must be writable");

        let outcome = takeover_bind_bounded(&path);

        assert!(
            outcome.is_err(),
            "a non-socket at the endpoint path is not a stale socket, and \
             takeover_bind must refuse rather than delete it"
        );
        assert!(
            path.exists(),
            "the file was unlinked on the strength of a refusal that says only \
             'nothing is listening', not 'this is mine to delete'"
        );
        assert_eq!(
            std::fs::read(&path).expect("the file must still be readable"),
            b"not a socket",
            "the file survived by name but not by content"
        );
    }

    #[test]
    fn slice_until_caps_and_expires() {
        // No deadline ⇒ a full slice.
        assert_eq!(slice_until(None), Ok(WAIT_SLICE));
        // A far deadline ⇒ capped at a slice.
        let far = Instant::now().checked_add(Duration::from_secs(30));
        assert_eq!(slice_until(far), Ok(WAIT_SLICE));
        // A passed deadline ⇒ terminal timeout.
        let past = Instant::now().checked_sub(Duration::from_secs(1));
        assert_eq!(slice_until(past), Err(()));
    }
}

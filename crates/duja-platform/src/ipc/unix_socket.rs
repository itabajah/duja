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
//! - **The parent directory is the real access barrier**: it is created `0700`,
//!   so no other user can even traverse into it to reach the socket inode. The
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
//!   fails with `AddrInUse`, we try to *connect* — a refusal means the previous
//!   owner is gone (a stale inode), so we unlink and rebind; a successful connect
//!   means a live server already owns the name and we refuse to start a second.
//!   (On Windows the equivalent split is `FILE_FLAG_FIRST_PIPE_INSTANCE` plus the
//!   named single-instance mutex; here the bound socket *is* the instance token.)
//! - The same limits as the pipe: at most [`MAX_CONNECTIONS`](super::MAX_CONNECTIONS)
//!   connections are in flight, [`MAX_HANDLER_THREADS`](super::MAX_HANDLER_THREADS)
//!   serve at once, and the frame codec caps a single body at 64 KiB **before**
//!   allocating (enforced inside [`duja_ipc`], used here exactly as the pipe uses
//!   it).
//!
//! # Threads
//!
//! A [`PipeServer`] owns one **listener** thread and a pool of at most
//! [`MAX_HANDLER_THREADS`](super::MAX_HANDLER_THREADS) **handler** threads. The
//! listener accepts connections and hands each to a handler over a bounded
//! channel; an atomic in-flight counter caps the total accepted at
//! [`MAX_CONNECTIONS`](super::MAX_CONNECTIONS), and while at capacity the listener
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
//! Reads and writes on an accepted connection are bounded the same way: the
//! socket's `SO_RCVTIMEO`/`SO_SNDTIMEO` is set to a short slice and the operation
//! is retried until it completes, times out, or the stop flag is observed. The
//! server arms **one whole-exchange read deadline** (see
//! [`SockStream::with_read_deadline`]) computed from an [`Instant`] and enforced
//! by setting each slice to `min(slice, remaining_budget)`. This is the direct
//! fix for P5 finding C1: a naive `set_read_timeout(READ_TIMEOUT)` renews
//! `SO_RCVTIMEO` on **every** syscall, so a peer dribbling one byte at a time
//! resets the budget forever and pins a handler thread (the frame never
//! completes). Clients keep a per-read budget — they talk to a trusted, prompt
//! server.
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
use std::os::unix::fs::PermissionsExt;
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

/// How long the listener backs off before retrying when it is at the connection
/// cap (no free slot to accept the next one).
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

// -- The connected-stream adapter (Read + Write with sliced timeouts) -----

/// A connected socket presented as a `Read + Write` byte stream.
///
/// Reads are bounded by an armed exchange deadline (server) or a fresh
/// [`READ_TIMEOUT`] per read (client); writes by [`WRITE_TIMEOUT`]. Each blocking
/// wait is sliced so an optional shutdown flag is honoured promptly.
struct SockStream {
    stream: UnixStream,
    stop: Option<Arc<AtomicBool>>,
    /// When set, the instant by which **all** reads of one request→response
    /// exchange must have completed. See [`SockStream::with_read_deadline`].
    read_deadline: Option<Instant>,
}

impl SockStream {
    /// A server-side stream, cancellable by `stop`.
    fn server(stream: UnixStream, stop: Arc<AtomicBool>) -> Self {
        SockStream {
            stream,
            stop: Some(stop),
            read_deadline: None,
        }
    }

    /// A client-side stream (no shutdown flag; per-read timeout budget).
    fn client(stream: UnixStream) -> Self {
        SockStream {
            stream,
            stop: None,
            read_deadline: None,
        }
    }

    /// Arm a single deadline shared by every read of one exchange, starting now.
    ///
    /// Without this, each read would renew its `SO_RCVTIMEO` budget, and because
    /// the framing layer drives reads with `read_exact` (looping until its buffer
    /// fills), a peer dribbling one byte at a time would renew the budget forever
    /// and pin a handler thread — P5 finding C1. Servers arm one whole-exchange
    /// deadline; clients keep the per-read budget.
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

/// The slice to block for now: `min(WAIT_SLICE, remaining)`, or a terminal
/// timeout if the deadline has already passed. `None` deadline ⇒ a full slice.
fn slice_until(deadline: Option<Instant>) -> Result<Duration, ()> {
    match deadline {
        Some(dl) => match dl.checked_duration_since(Instant::now()) {
            Some(rem) if !rem.is_zero() => Ok(rem.min(WAIT_SLICE)),
            _ => Err(()),
        },
        None => Ok(WAIT_SLICE),
    }
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
            self.stream.set_read_timeout(Some(slice))?;
            match self.stream.read(buf) {
                Ok(n) => return Ok(n),
                // The slice expired (`WouldBlock`/`TimedOut`) or the syscall was
                // interrupted (`EINTR`): re-check the stop flag and deadline, then
                // retry. The deadline bounds the loop, so `EINTR` cannot spin.
                Err(e) => match e.kind() {
                    io::ErrorKind::WouldBlock
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::Interrupted => {}
                    _ => return Err(e),
                },
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
            self.stream.set_write_timeout(Some(slice))?;
            match self.stream.write(buf) {
                Ok(n) => return Ok(n),
                Err(e) => match e.kind() {
                    io::ErrorKind::WouldBlock
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::Interrupted => {}
                    _ => return Err(e),
                },
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
        let listener = bind_listener(&path)?;
        // Defence-in-depth beyond the 0700 parent dir (see module docs).
        set_mode(&path, 0o600).map_err(|e| IpcTransportError::Io(e.to_string()))?;
        listener.set_nonblocking(true).map_err(|e| {
            let _ = std::fs::remove_file(&path);
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
                    let _ = std::fs::remove_file(&path);
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
                    let _ = std::fs::remove_file(&path);
                    return Err(IpcTransportError::Io(e.to_string()));
                }
            }
        };

        Ok(PipeServer {
            stop,
            listener: Some(listener_handle),
            workers,
            socket_path: path,
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
        let _ = std::fs::remove_file(&self.socket_path);
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
                // Accepted sockets do not inherit the listener's non-blocking
                // flag portably; force blocking so `SO_RCVTIMEO`/`SO_SNDTIMEO`
                // (which only affect blocking mode) govern the sliced timeouts.
                if stream.set_nonblocking(false).is_err() {
                    continue; // drop and try the next
                }
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
/// over a stale socket if one is present.
fn bind_listener(path: &Path) -> Result<UnixListener, IpcTransportError> {
    prepare_socket_dir(path).map_err(|e| IpcTransportError::Io(e.to_string()))?;
    match UnixListener::bind(path) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => takeover_bind(path),
        Err(e) => Err(IpcTransportError::Io(e.to_string())),
    }
}

/// The `AddrInUse` path: probe for a live owner, else unlink the stale inode and
/// rebind.
fn takeover_bind(path: &Path) -> Result<UnixListener, IpcTransportError> {
    if UnixStream::connect(path).is_ok() {
        // A live server accepted the probe: we are the second instance.
        return Err(IpcTransportError::Io(
            "another duja IPC server is already listening on this socket".to_owned(),
        ));
    }
    // Refused (or otherwise unconnectable): the inode is stale. Unlink and rebind.
    std::fs::remove_file(path).map_err(|e| IpcTransportError::Io(e.to_string()))?;
    UnixListener::bind(path).map_err(|e| IpcTransportError::Io(e.to_string()))
}

/// Create `path`'s parent directory (recursively) and set it `0700`.
///
/// The parent is always a directory Duja owns (`.../duja/`), never a shared
/// system directory, because every resolved path nests the socket under a
/// dedicated `duja` (or `duja-<uid>`) component.
fn prepare_socket_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;
    }
    Ok(())
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
                    stream
                        .set_nonblocking(false)
                        .map_err(|e| IpcTransportError::Io(e.to_string()))?;
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

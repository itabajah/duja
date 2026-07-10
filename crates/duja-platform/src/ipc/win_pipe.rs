//! Windows named-pipe transport for the local IPC protocol.
//!
//! This is the confined-`unsafe` half of the IPC story: [`duja_ipc`] stays pure
//! protocol, and every Win32 call — pipe creation with an explicit DACL, the
//! anti-squat flag, remote-client rejection, the client PID/session check, and
//! the polled read timeout — lives here behind safe wrappers.
//!
//! # Threads
//!
//! A [`PipeServer`] owns one **listener** thread and a pool of at most
//! [`MAX_HANDLER_THREADS`](super::MAX_HANDLER_THREADS) **handler** threads. The
//! listener creates pipe instances one at a time and blocks in
//! `ConnectNamedPipe`; on a connection it hands the connected instance to a
//! handler over a bounded channel and creates the next instance. Because at most
//! [`MAX_CONNECTIONS`](super::MAX_CONNECTIONS) instances exist at once (the
//! `nMaxInstances` ceiling), a flood past that is refused by the OS with
//! `ERROR_PIPE_BUSY` rather than growing the server.
//!
//! # Read timeout without overlapped I/O
//!
//! Reads use a `PeekNamedPipe` poll against a deadline rather than overlapped
//! I/O, which keeps the handles synchronous (so the listener's blocking
//! `ConnectNamedPipe` stays simple) while still bounding how long a slow writer
//! can pin a handler thread. The same poll checks the shutdown flag, so a
//! blocked read unblocks promptly on teardown.

// RATIONALE (clippy::cast_possible_truncation): the only integer casts here
// widen a pipe's `u32` byte count to `usize` (never truncating on the 32/64-bit
// targets we support) or narrow a compile-time `size_of` to `u32` via
// `try_from(..).unwrap_or(0)`; neither loses information at runtime.
#![allow(clippy::cast_possible_truncation)]
// RATIONALE (clippy::borrow_as_ptr): passing `&mut out_param` to a Win32 call
// that wants a raw pointer is the idiomatic FFI shape; the borrow lives exactly
// for the synchronous call.
#![allow(clippy::borrow_as_ptr)]

use core::ffi::c_void;
use std::io::{self, Read, Write};
use std::iter;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, HLOCAL, LocalFree,
    WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE,
    FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, NAMED_PIPE_MODE,
    PIPE_REJECT_REMOTE_CLIENTS, PeekNamedPipe, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

use duja_ipc::{MAX_FRAME_LEN, Request, Response};

use super::{IpcTransportError, MAX_CONNECTIONS, MAX_HANDLER_THREADS, READ_TIMEOUT};

/// How often the read poll checks for data / shutdown while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The default per-user pipe name: `\\.\pipe\duja-<user-SID>`.
///
/// Falls back to a constant suffix if the user's SID cannot be read (rare); the
/// DACL still constrains access, so the name is only a namespace disambiguator.
#[must_use]
pub fn default_pipe_name() -> String {
    let sid = current_user_sid_string().unwrap_or_else(|| "anon".to_owned());
    format!(r"\\.\pipe\duja-{sid}")
}

// -- Send-safe handle wrapper ---------------------------------------------

/// A pipe instance handle moved from the listener thread to a handler thread.
///
/// A raw `HANDLE` is not `Send`; a kernel handle is nonetheless safe to use from
/// another thread once ownership is transferred (never aliased), which this
/// wrapper asserts.
struct SendHandle(HANDLE);

// SAFETY: the wrapped value is a kernel pipe-instance handle whose sole owner is
// transferred to the receiving thread; it is never used concurrently from two
// threads.
unsafe impl Send for SendHandle {}

// -- Security descriptor (explicit user-only DACL) ------------------------

/// An owned self-relative security descriptor built from an SDDL string.
///
/// Holds the `LocalAlloc`-backed blob returned by
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW`; freed on drop.
struct SecurityDescriptor {
    psd: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is a heap blob solely owned by this value; moving that
// ownership to the listener thread is sound (it is only read, by CreateNamedPipe,
// on that one thread).
unsafe impl Send for SecurityDescriptor {}

impl SecurityDescriptor {
    /// Build a protected DACL granting only `sid` full access (no inherited
    /// ACEs, no `Everyone`).
    fn user_only(sid: &str) -> Option<Self> {
        let sddl = format!("D:P(A;;GA;;;{sid})");
        let wide: Vec<u16> = sddl.encode_utf16().chain(iter::once(0)).collect();
        let mut psd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is a NUL-terminated wide string living across the call;
        // `psd` receives an owned descriptor we free in `Drop`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
        }
        .ok()?;
        if psd.0.is_null() {
            return None;
        }
        Some(SecurityDescriptor { psd })
    }

    /// A `SECURITY_ATTRIBUTES` referencing this descriptor (valid while `self`
    /// is alive).
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: self.psd.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.psd.0.is_null() {
            // SAFETY: `psd` came from ConvertStringSecurityDescriptor... and is
            // owned solely by this value; freeing it once is correct.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.psd.0)));
            }
        }
    }
}

// -- The connected-stream adapter (Read + Write with a polled timeout) ----

/// A connected pipe instance presented as a `Read + Write` byte stream.
///
/// Reads poll `PeekNamedPipe` against a [`READ_TIMEOUT`] deadline (and an
/// optional shutdown flag); writes go straight through `WriteFile`. Closes the
/// handle on drop.
struct PipeStream {
    handle: HANDLE,
    stop: Option<Arc<AtomicBool>>,
}

impl PipeStream {
    fn new(handle: HANDLE, stop: Option<Arc<AtomicBool>>) -> Self {
        PipeStream { handle, stop }
    }

    /// Block until the peer has drained everything we wrote (so a following
    /// close cannot discard the response).
    fn flush_buffers(&self) {
        // SAFETY: `handle` is a live pipe instance we own.
        unsafe {
            let _ = FlushFileBuffers(self.handle);
        }
    }

    fn stopping(&self) -> bool {
        self.stop
            .as_ref()
            .is_some_and(|s| s.load(Ordering::Acquire))
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now().checked_add(READ_TIMEOUT);
        loop {
            if self.stopping() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "ipc server stopping",
                ));
            }
            match peek_available(self.handle) {
                PeekResult::Available(0) => {}
                PeekResult::Available(avail) => {
                    let want = (avail as usize).min(buf.len());
                    if let Some(slice) = buf.get_mut(..want) {
                        let mut read_n = 0u32;
                        // SAFETY: `slice` is valid for `want` bytes; `handle` is
                        // a live pipe instance; synchronous read (no overlapped).
                        unsafe { ReadFile(self.handle, Some(slice), Some(&mut read_n), None) }
                            .map_err(|e| io::Error::other(e.to_string()))?;
                        return Ok(read_n as usize);
                    }
                    return Ok(0);
                }
                PeekResult::Disconnected => return Ok(0),
                PeekResult::Io(e) => return Err(e),
            }
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "ipc read timeout"));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut written = 0u32;
        // SAFETY: `buf` is valid for its length; `handle` is a live pipe
        // instance; synchronous write (no overlapped).
        unsafe { WriteFile(self.handle, Some(buf), Some(&mut written), None) }
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeStream {
    fn drop(&mut self) {
        close_handle(self.handle);
    }
}

/// The outcome of one `PeekNamedPipe` probe.
enum PeekResult {
    Available(u32),
    Disconnected,
    Io(io::Error),
}

/// Query how many bytes are currently readable without blocking.
fn peek_available(handle: HANDLE) -> PeekResult {
    let mut avail = 0u32;
    // SAFETY: `handle` is a live pipe instance; all output pointers are optional
    // and only `avail` is requested.
    match unsafe { PeekNamedPipe(handle, None, 0, None, Some(&mut avail), None) } {
        Ok(()) => PeekResult::Available(avail),
        Err(e) => {
            // SAFETY: reads the last-error for the failed peek above.
            let code = unsafe { GetLastError() };
            if code == ERROR_BROKEN_PIPE || code == ERROR_PIPE_NOT_CONNECTED {
                PeekResult::Disconnected
            } else {
                PeekResult::Io(io::Error::other(e.to_string()))
            }
        }
    }
}

// -- The server -----------------------------------------------------------

/// A running named-pipe IPC server.
///
/// Holds the listener and handler threads; [`shutdown`](Self::shutdown) (also
/// run on drop) stops the listener, drains and joins every thread.
pub struct PipeServer {
    stop: Arc<AtomicBool>,
    name_wide: Vec<u16>,
    listener: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
}

impl PipeServer {
    /// Start a server on the default per-user pipe name.
    ///
    /// # Errors
    /// [`IpcTransportError::Io`] if the user SID or DACL cannot be resolved, or
    /// if the first pipe instance cannot be created (e.g. a squatter already
    /// owns the name — [`FILE_FLAG_FIRST_PIPE_INSTANCE`] makes that fail).
    pub fn serve<H>(handler: H) -> Result<Self, IpcTransportError>
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        Self::serve_named(&default_pipe_name(), handler)
    }

    /// Start a server on an explicit pipe name (test seam).
    ///
    /// # Errors
    /// As [`serve`](Self::serve).
    pub fn serve_named<H>(name: &str, handler: H) -> Result<Self, IpcTransportError>
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        let sid = current_user_sid_string().ok_or_else(|| {
            IpcTransportError::Io("could not read the current user SID".to_owned())
        })?;
        let descriptor = SecurityDescriptor::user_only(&sid)
            .ok_or_else(|| IpcTransportError::Io("could not build the pipe DACL".to_owned()))?;

        let name_wide: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();

        // Create the FIRST instance synchronously so an anti-squat failure (or a
        // second server) surfaces here rather than on a background thread.
        let attributes = descriptor.attributes();
        let first = create_instance(&name_wide, true, &attributes).map_err(|code| {
            IpcTransportError::Io(format!(
                "could not create the pipe (win32 error {})",
                code.0
            ))
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<dyn Fn(Request) -> Response + Send + Sync> = Arc::new(handler);
        let (work_tx, work_rx) = bounded::<SendHandle>(MAX_CONNECTIONS as usize);

        let mut workers = Vec::with_capacity(MAX_HANDLER_THREADS);
        for i in 0..MAX_HANDLER_THREADS {
            let rx = work_rx.clone();
            let stop = stop.clone();
            let handler = handler.clone();
            let worker = std::thread::Builder::new()
                .name(format!("duja-ipc-handler-{i}"))
                .spawn(move || worker_loop(&rx, &stop, handler.as_ref()))
                .map_err(|e| IpcTransportError::Io(e.to_string()))?;
            workers.push(worker);
        }
        drop(work_rx);

        let listener = {
            let stop = stop.clone();
            let name_wide = name_wide.clone();
            let first = SendHandle(first);
            std::thread::Builder::new()
                .name("duja-ipc-listener".to_owned())
                .spawn(move || listener_loop(&name_wide, &descriptor, first, &work_tx, &stop))
                .map_err(|e| IpcTransportError::Io(e.to_string()))?
        };

        Ok(PipeServer {
            stop,
            name_wide,
            listener: Some(listener),
            workers,
        })
    }

    /// Stop the server: unblock the listener, drain and join every thread.
    ///
    /// Idempotent and also run on drop.
    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        if self.stop.swap(true, Ordering::AcqRel) {
            return; // already shut down
        }
        // Unblock a listener parked in ConnectNamedPipe by connecting to it
        // ourselves; it observes the stop flag and returns.
        self.nudge_listener();
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }

    /// Best-effort self-connect that wakes a listener blocked in
    /// `ConnectNamedPipe`. Any failure is fine: if all instances are busy the
    /// listener is in its retry loop and observes the stop flag directly.
    fn nudge_listener(&self) {
        // SAFETY: `name_wide` is a NUL-terminated wide string; the handle (if
        // any) is closed immediately.
        unsafe {
            if let Ok(handle) = CreateFileW(
                PCWSTR(self.name_wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            ) {
                let _ = CloseHandle(handle);
            }
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// The listener thread: create an instance, block for a client, hand it to a
/// worker, repeat — bounded by the OS `nMaxInstances` ceiling.
// RATIONALE (clippy::needless_pass_by_value): `first` is taken by value as a
// whole `SendHandle` so the spawning closure captures the `Send` wrapper (not
// its non-`Send` `HANDLE` field, which edition-2021 disjoint captures would
// otherwise pick); only its Copy field is then read.
#[allow(clippy::needless_pass_by_value)]
fn listener_loop(
    name_wide: &[u16],
    descriptor: &SecurityDescriptor,
    first: SendHandle,
    work_tx: &Sender<SendHandle>,
    stop: &AtomicBool,
) {
    let attributes = descriptor.attributes();
    let mut pending = Some(first.0);
    loop {
        if stop.load(Ordering::Acquire) {
            if let Some(handle) = pending {
                close_handle(handle);
            }
            break;
        }
        let instance = if let Some(handle) = pending.take() {
            handle
        } else if let Ok(handle) = create_instance(name_wide, false, &attributes) {
            handle
        } else {
            // All instances busy (or a transient failure): wait briefly and
            // retry, re-checking the stop flag each pass.
            std::thread::sleep(POLL_INTERVAL);
            continue;
        };

        if connect_instance(instance).is_ok() {
            if stop.load(Ordering::Acquire) {
                close_handle(instance);
                break;
            }
            if work_tx.send(SendHandle(instance)).is_err() {
                close_handle(instance);
                break; // workers gone
            }
        } else {
            close_handle(instance);
            if stop.load(Ordering::Acquire) {
                break;
            }
        }
    }
    // Dropping the sender ends the workers once the channel drains.
}

/// A handler thread: verify each accepted client, then serve exactly one
/// request→response exchange over it.
fn worker_loop(
    rx: &Receiver<SendHandle>,
    stop: &Arc<AtomicBool>,
    handler: &(dyn Fn(Request) -> Response + Send + Sync),
) {
    while let Ok(SendHandle(handle)) = rx.recv() {
        if stop.load(Ordering::Acquire) {
            close_handle(handle);
            continue;
        }
        serve_connection(handle, stop, handler);
    }
}

/// Verify the peer, then run one exchange; the stream closes the handle on drop.
fn serve_connection(
    handle: HANDLE,
    stop: &Arc<AtomicBool>,
    handler: &(dyn Fn(Request) -> Response + Send + Sync),
) {
    if !client_in_same_session(handle) {
        // Refuse a cross-session peer silently; nothing trustworthy to answer.
        close_handle(handle);
        return;
    }
    let mut stream = PipeStream::new(handle, Some(stop.clone()));
    let _ = duja_ipc::serve_once(&mut stream, handler);
    stream.flush_buffers();
    // `stream` drops here, closing the handle (freeing the instance slot).
}

/// Create one pipe instance. `first` sets `FILE_FLAG_FIRST_PIPE_INSTANCE` (the
/// anti-squat guard for the very first instance).
fn create_instance(
    name_wide: &[u16],
    first: bool,
    attributes: &SECURITY_ATTRIBUTES,
) -> Result<HANDLE, WIN32_ERROR> {
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let pipe_mode = NAMED_PIPE_MODE(PIPE_REJECT_REMOTE_CLIENTS.0);
    let buf = u32::try_from(MAX_FRAME_LEN).unwrap_or(64 * 1024);
    // SAFETY: `name_wide` is a NUL-terminated wide string; `attributes` points at
    // a live SECURITY_ATTRIBUTES + descriptor; all scalar arguments are valid.
    // Returns INVALID_HANDLE_VALUE on failure (no `Result`).
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name_wide.as_ptr()),
            open_mode,
            pipe_mode,
            MAX_CONNECTIONS,
            buf,
            buf,
            0,
            Some(attributes),
        )
    };
    if handle.is_invalid() {
        // SAFETY: reads the last-error for the failed create above.
        Err(unsafe { GetLastError() })
    } else {
        Ok(handle)
    }
}

/// Block until a client connects to `instance` (or it is already connected).
fn connect_instance(instance: HANDLE) -> Result<(), ()> {
    // SAFETY: `instance` is a freshly created, unconnected pipe instance; a
    // synchronous (non-overlapped) connect is requested.
    if unsafe { ConnectNamedPipe(instance, None) }.is_ok() {
        return Ok(());
    }
    // A client that connected between create and connect surfaces as
    // ERROR_PIPE_CONNECTED, which is success.
    // SAFETY: reads the last-error for the failed connect above.
    if unsafe { GetLastError() } == ERROR_PIPE_CONNECTED {
        Ok(())
    } else {
        Err(())
    }
}

/// Whether the pipe's connected client belongs to the same session as us.
fn client_in_same_session(handle: HANDLE) -> bool {
    let mut client_pid = 0u32;
    // SAFETY: `handle` is a connected pipe instance we own.
    if unsafe { GetNamedPipeClientProcessId(handle, &mut client_pid) }.is_err() {
        return false;
    }
    let Some(client_session) = session_of(client_pid) else {
        return false;
    };
    // SAFETY: GetCurrentProcessId takes no arguments and cannot fail.
    let our_pid = unsafe { GetCurrentProcessId() };
    let Some(our_session) = session_of(our_pid) else {
        return false;
    };
    client_session == our_session
}

/// The Terminal-Services session id of a process, or `None` on failure.
fn session_of(pid: u32) -> Option<u32> {
    let mut session = 0u32;
    // SAFETY: `session` is a valid out pointer; the call reads only `pid`.
    unsafe { ProcessIdToSessionId(pid, &mut session) }.ok()?;
    Some(session)
}

/// Close a kernel handle, ignoring the (best-effort) result.
fn close_handle(handle: HANDLE) {
    if !handle.is_invalid() {
        // SAFETY: `handle` was returned by CreateNamedPipeW/CreateFileW and is
        // owned here; closing it once is correct.
        unsafe {
            let _ = CloseHandle(handle);
        }
    }
}

// -- The client -----------------------------------------------------------

/// A connected IPC client.
///
/// Holds the connected pipe stream; [`request`](Self::request) performs one
/// exchange. The handle closes on drop.
pub struct PipeClient {
    stream: PipeStream,
}

impl PipeClient {
    /// Connect to the running server on the default per-user pipe name.
    ///
    /// # Errors
    /// [`IpcTransportError::NotRunning`] if no server is listening,
    /// [`IpcTransportError::Busy`] if every instance is in use for the whole
    /// timeout, [`IpcTransportError::Timeout`] if the deadline elapses, or
    /// [`IpcTransportError::Io`] on any other transport failure.
    pub fn connect(timeout: Duration) -> Result<Self, IpcTransportError> {
        Self::connect_named(&default_pipe_name(), timeout)
    }

    /// Connect to an explicit pipe name (test seam).
    ///
    /// # Errors
    /// As [`connect`](Self::connect).
    pub fn connect_named(name: &str, timeout: Duration) -> Result<Self, IpcTransportError> {
        let name_wide: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();
        let deadline = Instant::now().checked_add(timeout);
        loop {
            // SAFETY: `name_wide` is a NUL-terminated wide string; a failed open
            // returns an error we classify via GetLastError below.
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(name_wide.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };
            match opened {
                Ok(handle) if !handle.is_invalid() => {
                    return Ok(PipeClient {
                        stream: PipeStream::new(handle, None),
                    });
                }
                _ => {
                    // SAFETY: reads the last-error for the failed open above.
                    let code = unsafe { GetLastError() };
                    if code == ERROR_FILE_NOT_FOUND {
                        return Err(IpcTransportError::NotRunning);
                    }
                    if code != ERROR_PIPE_BUSY {
                        return Err(IpcTransportError::Io(format!("win32 error {}", code.0)));
                    }
                    // Busy: wait for a free instance within the remaining budget.
                    let remaining = deadline.map_or(Duration::ZERO, |dl| {
                        dl.saturating_duration_since(Instant::now())
                    });
                    if remaining.is_zero() {
                        return Err(IpcTransportError::Busy);
                    }
                    let ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
                    // SAFETY: `name_wide` is a NUL-terminated wide string; a
                    // false return just means we retry / time out below.
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(name_wide.as_ptr()), ms) };
                    if deadline.is_some_and(|dl| Instant::now() >= dl) {
                        return Err(IpcTransportError::Timeout);
                    }
                }
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

// -- Current-user SID -----------------------------------------------------

/// The current process token's user SID as an `S-1-…` string, or `None`.
fn current_user_sid_string() -> Option<String> {
    // SAFETY: `GetCurrentProcess` is a pseudo-handle needing no close;
    // `OpenProcessToken` writes an owned token handle we close below.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let sid = read_token_sid_string(token);
        let _ = CloseHandle(token);
        sid
    }
}

/// Read the `TokenUser` SID for an opened token and stringify it.
///
/// # Safety
/// `token` must be a valid token handle opened with `TOKEN_QUERY`.
unsafe fn read_token_sid_string(token: HANDLE) -> Option<String> {
    let mut len = 0u32;
    // SAFETY: sizing call — a null buffer with length 0 returns the needed size.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    if len == 0 {
        return None;
    }
    // Over-aligned buffer so the TOKEN_USER view never raises the alignment.
    let words = (len as usize).div_ceil(8);
    let mut buf = vec![0u64; words];
    // SAFETY: `buf` is at least `len` bytes; the call fills it with a TOKEN_USER
    // whose embedded PSID points inside `buf`.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .ok()?;
    }
    // SAFETY: `buf` now holds a well-formed, 8-aligned TOKEN_USER.
    let sid: PSID = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    if sid.is_invalid() {
        return None;
    }
    sid_to_string(sid)
}

/// Convert a `PSID` to its `S-1-…` string form.
fn sid_to_string(sid: PSID) -> Option<String> {
    let mut raw = PWSTR::null();
    // SAFETY: `sid` is a valid PSID; on success `raw` receives a LocalAlloc'd
    // wide string we copy and free below.
    unsafe { ConvertSidToStringSidW(sid, &mut raw) }.ok()?;
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a NUL-terminated wide string owned by us until LocalFree.
    let string = unsafe { raw.to_string() }.ok();
    // SAFETY: `raw` came from ConvertSidToStringSidW (LocalAlloc); free it once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(raw.0.cast::<c_void>())));
    }
    string
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pipe_name_is_per_user_and_well_formed() {
        let name = default_pipe_name();
        assert!(name.starts_with(r"\\.\pipe\duja-"), "name = {name}");
    }

    #[test]
    fn current_user_sid_is_readable() {
        // In a normal (even disconnected) session the process token has a user
        // SID; assert we can read and stringify it.
        let sid = current_user_sid_string();
        assert!(
            sid.as_deref().is_some_and(|s| s.starts_with("S-1-")),
            "sid = {sid:?}"
        );
    }

    #[test]
    fn security_descriptor_builds_from_sddl() {
        let sid = current_user_sid_string().expect("user sid");
        let descriptor = SecurityDescriptor::user_only(&sid).expect("descriptor");
        let attributes = descriptor.attributes();
        assert!(!attributes.lpSecurityDescriptor.is_null());
    }
}

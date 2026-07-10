//! Windows named-pipe transport for the local IPC protocol.
//!
//! This is the confined-`unsafe` half of the IPC story: [`duja_ipc`] stays pure
//! protocol, and every Win32 call — pipe creation with an explicit DACL, the
//! anti-squat flag, remote-client rejection, the client PID/session check, and
//! the bounded overlapped I/O — lives here behind safe wrappers.
//!
//! # Threads
//!
//! A [`PipeServer`] owns one **listener** thread and a pool of at most
//! [`MAX_HANDLER_THREADS`](super::MAX_HANDLER_THREADS) **handler** threads. The
//! listener creates pipe instances one at a time and waits for a client in an
//! *overlapped* `ConnectNamedPipe`; on a connection it hands the connected
//! instance to a handler over a bounded channel and creates the next instance.
//! Because at most [`MAX_CONNECTIONS`](super::MAX_CONNECTIONS) instances exist at
//! once (the `nMaxInstances` ceiling), a flood past that is refused by the OS
//! with `ERROR_PIPE_BUSY` rather than growing the server. The overlapped connect
//! wait re-checks the shutdown flag on a short slice, so the listener tears down
//! promptly without needing a self-connect nudge.
//!
//! # Bounded I/O with overlapped operations
//!
//! Every blocking wait — `ConnectNamedPipe`, `ReadFile`, `WriteFile` — runs on a
//! handle opened `FILE_FLAG_OVERLAPPED` and is driven against a real deadline and
//! the shutdown flag: the call is started, then we wait on its manual-reset event
//! in short slices, and on timeout or stop we `CancelIoEx` the operation and reap
//! it with a blocking `GetOverlappedResult` before returning.
//!
//! This replaced an earlier `PeekNamedPipe`-poll timeout that assumed peek never
//! blocks. That assumption is false: a client that connects with `GENERIC_READ`
//! only and writes nothing (exactly what a security-inspection open does) wedges
//! the handler thread *inside a single `PeekNamedPipe` call* — it never returns,
//! so neither the deadline nor the stop flag is ever re-evaluated, and shutdown
//! then hangs forever joining that handler. Any silent client could pin a handler
//! that way, defeating the 5 s read-timeout guarantee. Overlapped I/O with
//! `CancelIoEx` is the structural fix: the wait itself is bounded and cancellable,
//! never parked in an OS call that ignores our deadline.
//!
//! ## OVERLAPPED lifetime (safety-critical)
//!
//! An `OVERLAPPED` handed to a Win32 call that returns `ERROR_IO_PENDING` is
//! borrowed by the kernel until the operation finishes; freeing it while the
//! kernel still references it is undefined behavior. Every code path here holds
//! both the `OVERLAPPED` and its event alive until the operation is *reaped* —
//! either it completed naturally, or we called `CancelIoEx` followed by a
//! blocking `GetOverlappedResult` (see [`reap_cancelled`]). No overlapped
//! initiate ever returns to its caller with the op still in flight.

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
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, NAMED_PIPE_MODE,
    PIPE_REJECT_REMOTE_CLIENTS, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, OpenProcessToken, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use duja_ipc::{MAX_FRAME_LEN, Request, Response};

use super::{IpcTransportError, MAX_CONNECTIONS, MAX_HANDLER_THREADS, READ_TIMEOUT};

/// How long the listener backs off before retrying when every pipe instance is
/// busy (no free slot to create the next one).
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The slice each overlapped wait blocks for before re-checking the stop flag
/// and the operation's deadline. Small enough that shutdown is prompt, large
/// enough not to spin.
const WAIT_SLICE: Duration = Duration::from_millis(50);

/// The bound on a single overlapped `WriteFile`.
///
/// Writes complete as soon as the bytes land in the pipe's kernel buffer, which
/// is near-instant unless a client has connected and then refuses to drain a
/// full buffer. This deadline (plus the stop flag) caps how long such a client
/// can pin a handler in `WriteFile`; it mirrors the read timeout.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// ACEs, no `Everyone`), with `sid` as the explicit owner.
    ///
    /// The owner is set explicitly because an **elevated** token's default
    /// owner for new objects is the `Administrators` group, not the user —
    /// relying on the default would make the pipe's ownership depend on how
    /// the process was launched (and fails the owner check on elevated CI
    /// runners).
    fn user_only(sid: &str) -> Option<Self> {
        let sddl = format!("O:{sid}D:P(A;;GA;;;{sid})");
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

// -- Overlapped-I/O primitives --------------------------------------------

/// A manual-reset event owned for the lifetime of one overlapped operation.
///
/// `ReadFile`/`WriteFile`/`ConnectNamedPipe` reset the event to non-signaled
/// when they start the op and signal it on completion; we wait on it in slices.
/// Closed on drop.
struct Event(HANDLE);

impl Event {
    fn new() -> io::Result<Self> {
        // SAFETY: a manual-reset (`true`), initially-non-signaled (`false`) event
        // with no name/attributes; returns an owned handle we close in `Drop`.
        let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Event(handle))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        close_handle(self.0);
    }
}

/// The terminal outcome of one overlapped operation.
enum IoOutcome {
    /// The op completed transferring this many bytes.
    Bytes(u32),
    /// The peer closed (broken/disconnected pipe) — end of stream.
    Eof,
    /// The deadline elapsed; the op was cancelled and reaped.
    TimedOut,
    /// The stop flag was observed; the op was cancelled and reaped.
    Interrupted,
    /// The op failed for another reason.
    Failed(io::Error),
}

/// Whether a Win32 error means the pipe peer is gone (treated as EOF for reads).
fn is_eof(code: WIN32_ERROR) -> bool {
    code == ERROR_BROKEN_PIPE || code == ERROR_PIPE_NOT_CONNECTED
}

/// Capture the last-error of an immediately-preceding failed Win32 call.
///
/// Must be invoked before any other Win32 call clobbers the thread's last error.
fn last_win32(result: windows::core::Result<()>) -> Result<(), WIN32_ERROR> {
    // SAFETY: `map_err` runs only on the failure path, immediately after the
    // just-returned failed call, so `GetLastError` still reflects it.
    result.map_err(|_| unsafe { GetLastError() })
}

/// Reap a *completed* overlapped op for its transferred byte count.
///
/// # Safety
/// `handle` and `*ov` must reference the same finished (signaled) operation.
unsafe fn reap_bytes(handle: HANDLE, ov: &mut OVERLAPPED) -> IoOutcome {
    let mut bytes = 0u32;
    // SAFETY: the op is signaled complete; `bwait = true` returns at once.
    if unsafe { GetOverlappedResult(handle, ov, &mut bytes, true) }.is_ok() {
        return IoOutcome::Bytes(bytes);
    }
    // SAFETY: reads the last-error for the failed reap above.
    let code = unsafe { GetLastError() };
    if is_eof(code) {
        IoOutcome::Eof
    } else {
        IoOutcome::Failed(io::Error::other(format!("win32 error {}", code.0)))
    }
}

/// Cancel a still-pending overlapped op and block until the kernel releases
/// `*ov`, so it can be dropped safely.
///
/// This is the safety-critical reaping step: after an initiate returned
/// `ERROR_IO_PENDING`, the kernel holds a pointer into `*ov`. `CancelIoEx`
/// requests cancellation but does not itself guarantee the kernel is finished;
/// the following blocking `GetOverlappedResult` does. Only after it returns is
/// `*ov` no longer referenced and free to drop.
///
/// # Safety
/// `handle` must own the pending operation described by `*ov`.
unsafe fn reap_cancelled(handle: HANDLE, ov: &mut OVERLAPPED) {
    // SAFETY: `handle` owns the pending op referencing `*ov`; requesting its
    // cancellation is sound (a no-op if it already completed).
    unsafe {
        let _ = CancelIoEx(handle, Some(std::ptr::from_ref(&*ov)));
    }
    let mut bytes = 0u32;
    // SAFETY: `bwait = true` blocks until the (cancelled or raced-to-complete)
    // op is fully reaped; afterwards the kernel no longer references `*ov`. The
    // result — typically `ERROR_OPERATION_ABORTED` — is intentionally ignored.
    unsafe {
        let _ = GetOverlappedResult(handle, ov, &mut bytes, true);
    }
}

/// Wait for a *pending* overlapped op against a deadline and stop flag, slicing
/// the wait so both are re-checked. On timeout or stop the op is cancelled and
/// reaped before returning; on completion its byte count is reaped.
///
/// # Safety
/// The initiate call for `*ov` returned `ERROR_IO_PENDING`, `event` is `*ov`'s
/// `hEvent`, and both `*ov` and `event` outlive this call. This function never
/// returns with the op still in flight (see [`reap_cancelled`]).
unsafe fn wait_pending(
    handle: HANDLE,
    ov: &mut OVERLAPPED,
    event: HANDLE,
    deadline: Option<Instant>,
    stop: Option<&AtomicBool>,
) -> IoOutcome {
    let slice_ms = u32::try_from(WAIT_SLICE.as_millis()).unwrap_or(u32::MAX);
    loop {
        if stop.is_some_and(|s| s.load(Ordering::Acquire)) {
            // SAFETY: op still pending; reap it before returning.
            unsafe { reap_cancelled(handle, ov) };
            return IoOutcome::Interrupted;
        }
        // SAFETY: `event` is the live manual-reset event of this pending op.
        let waited = unsafe { WaitForSingleObject(event, slice_ms) };
        if waited == WAIT_OBJECT_0 {
            // SAFETY: signaled ⇒ the op is complete and safe to reap.
            return unsafe { reap_bytes(handle, ov) };
        }
        if waited == WAIT_TIMEOUT {
            if deadline.is_some_and(|dl| Instant::now() >= dl) {
                // SAFETY: op still pending; reap it before returning.
                unsafe { reap_cancelled(handle, ov) };
                return IoOutcome::TimedOut;
            }
            continue; // re-check stop / deadline
        }
        // WAIT_FAILED (or WAIT_ABANDONED): give up, but still reap the op.
        // SAFETY: op still pending; reap it before returning.
        unsafe { reap_cancelled(handle, ov) };
        return IoOutcome::Failed(io::Error::other("WaitForSingleObject failed"));
    }
}

// -- The connected-stream adapter (Read + Write with overlapped timeouts) --

/// A connected pipe instance presented as a `Read + Write` byte stream.
///
/// The handle is opened `FILE_FLAG_OVERLAPPED`; reads are bounded by a
/// [`READ_TIMEOUT`] deadline and writes by [`WRITE_TIMEOUT`], each also
/// cancellable via an optional shutdown flag. Closes the handle on drop.
struct PipeStream {
    handle: HANDLE,
    stop: Option<Arc<AtomicBool>>,
}

impl PipeStream {
    fn new(handle: HANDLE, stop: Option<Arc<AtomicBool>>) -> Self {
        PipeStream { handle, stop }
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now().checked_add(READ_TIMEOUT);
        let event = Event::new()?;
        let mut ov = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: `buf` is valid for its length and, crucially, is NOT touched
        // again until the op is reaped by `drive_overlapped` below — the kernel
        // writes into it while the op is in flight. `ov`/`event` live on this
        // stack frame across the whole overlapped lifetime.
        let initiate = last_win32(unsafe { ReadFile(self.handle, Some(buf), None, Some(&mut ov)) });
        // SAFETY: `ov`/`event` outlive this call and `event` is `ov`'s `hEvent`;
        // the driver reaps any still-pending op before it returns.
        let outcome = unsafe {
            drive_overlapped(
                self.handle,
                &mut ov,
                event.0,
                initiate,
                deadline,
                self.stop_ref(),
            )
        };
        match outcome {
            IoOutcome::Bytes(n) => Ok(n as usize),
            IoOutcome::Eof => Ok(0),
            IoOutcome::TimedOut => Err(io::Error::new(io::ErrorKind::TimedOut, "ipc read timeout")),
            // NB: not `ErrorKind::Interrupted` — `Read::read_exact` (used by the
            // framing layer) silently *retries* on `Interrupted`, which would spin
            // forever once the stop flag is latched. `ConnectionAborted`
            // propagates and ends the exchange.
            IoOutcome::Interrupted => Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "ipc server stopping",
            )),
            IoOutcome::Failed(e) => Err(e),
        }
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now().checked_add(WRITE_TIMEOUT);
        let event = Event::new()?;
        let mut ov = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        // SAFETY: `buf` is valid for its length and is not read again by us until
        // the op is reaped by `drive_overlapped` below; `ov`/`event` live on this
        // stack frame across the whole overlapped lifetime.
        let initiate =
            last_win32(unsafe { WriteFile(self.handle, Some(buf), None, Some(&mut ov)) });
        // SAFETY: `ov`/`event` outlive this call and `event` is `ov`'s `hEvent`;
        // the driver reaps any still-pending op before it returns.
        let outcome = unsafe {
            drive_overlapped(
                self.handle,
                &mut ov,
                event.0,
                initiate,
                deadline,
                self.stop_ref(),
            )
        };
        match outcome {
            IoOutcome::Bytes(n) => Ok(n as usize),
            // A broken pipe mid-write means the peer went away: surface it as a
            // write error rather than a silent success.
            IoOutcome::Eof => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
            IoOutcome::TimedOut => {
                Err(io::Error::new(io::ErrorKind::TimedOut, "ipc write timeout"))
            }
            // See the read path: `ConnectionAborted`, not `Interrupted`, so a
            // retrying writer (e.g. `write_all`) cannot spin on the stop flag.
            IoOutcome::Interrupted => Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "ipc server stopping",
            )),
            IoOutcome::Failed(e) => Err(e),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl PipeStream {
    fn stop_ref(&self) -> Option<&AtomicBool> {
        self.stop.as_deref()
    }
}

impl Drop for PipeStream {
    fn drop(&mut self) {
        close_handle(self.handle);
    }
}

/// Drive an overlapped op from its initiate result to a terminal outcome.
///
/// `initiate` is the last-error classification of the `ReadFile`/`WriteFile`
/// initiate call; on `ERROR_IO_PENDING` we wait, otherwise the op already
/// finished (synchronously, as EOF, or as an error).
///
/// # Safety
/// `*ov`/`event` outlive this call, `event` is `*ov`'s `hEvent`, and the
/// initiate call that produced `initiate` targeted `*ov`. Any pending op is
/// reaped before return.
unsafe fn drive_overlapped(
    handle: HANDLE,
    ov: &mut OVERLAPPED,
    event: HANDLE,
    initiate: Result<(), WIN32_ERROR>,
    deadline: Option<Instant>,
    stop: Option<&AtomicBool>,
) -> IoOutcome {
    match initiate {
        // Completed synchronously; reap the byte count (the event is signaled).
        // SAFETY: `handle`/`*ov` name the same just-finished op (see fn Safety).
        Ok(()) => unsafe { reap_bytes(handle, ov) },
        // SAFETY: initiate returned ERROR_IO_PENDING, so the op is in flight on
        // `handle` with `*ov`/`event`; `wait_pending` reaps it before returning.
        Err(code) if code == ERROR_IO_PENDING => unsafe {
            wait_pending(handle, ov, event, deadline, stop)
        },
        Err(code) if is_eof(code) => IoOutcome::Eof,
        Err(code) => IoOutcome::Failed(io::Error::other(format!("win32 error {}", code.0))),
    }
}

// -- The server -----------------------------------------------------------

/// A running named-pipe IPC server.
///
/// Holds the listener and handler threads; [`shutdown`](Self::shutdown) (also
/// run on drop) stops the listener, drains and joins every thread.
pub struct PipeServer {
    stop: Arc<AtomicBool>,
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
            let first = SendHandle(first);
            std::thread::Builder::new()
                .name("duja-ipc-listener".to_owned())
                .spawn(move || listener_loop(&name_wide, &descriptor, first, &work_tx, &stop))
                .map_err(|e| IpcTransportError::Io(e.to_string()))?
        };

        Ok(PipeServer {
            stop,
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
        // The listener's overlapped `ConnectNamedPipe` wait re-checks the stop
        // flag every `WAIT_SLICE` and cancels itself, so no self-connect nudge is
        // needed to unblock it.
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
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

        match connect_instance(instance, stop) {
            ConnectOutcome::Connected => {
                if stop.load(Ordering::Acquire) {
                    close_handle(instance);
                    break;
                }
                if work_tx.send(SendHandle(instance)).is_err() {
                    close_handle(instance);
                    break; // workers gone
                }
            }
            ConnectOutcome::Stopped => {
                close_handle(instance);
                break;
            }
            ConnectOutcome::Failed => {
                close_handle(instance);
                if stop.load(Ordering::Acquire) {
                    break;
                }
                // A transient connect failure: back off briefly before retrying
                // so we never busy-spin creating instances.
                std::thread::sleep(POLL_INTERVAL);
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
///
/// No explicit `FlushFileBuffers` follows the exchange: on an overlapped handle
/// a flush blocks indefinitely waiting for the client to drain, which is exactly
/// the pin we are eliminating. The bounded overlapped [`Write`](PipeStream) gives
/// the durability guarantee instead — once the response write completes, the
/// bytes are in the pipe's kernel buffer. A well-behaved client reads them before
/// closing; a client that connects and never reads simply loses the response when
/// the handle closes, which is acceptable within the same-user threat model (it
/// only harms itself and cannot pin the handler).
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
    // `stream` drops here, closing the handle (freeing the instance slot).
}

/// Create one pipe instance. `first` sets `FILE_FLAG_FIRST_PIPE_INSTANCE` (the
/// anti-squat guard for the very first instance).
fn create_instance(
    name_wide: &[u16],
    first: bool,
    attributes: &SECURITY_ATTRIBUTES,
) -> Result<HANDLE, WIN32_ERROR> {
    // FILE_FLAG_OVERLAPPED so every wait on this instance (connect, read, write)
    // is issued asynchronously and can be bounded + cancelled; see the module
    // docs on why the previous synchronous/peek-poll design could hang.
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
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

/// The outcome of waiting for a client on a pipe instance.
enum ConnectOutcome {
    /// A client connected (or was already connected).
    Connected,
    /// The stop flag was observed while waiting; the listener should exit.
    Stopped,
    /// The connect failed transiently; the instance should be discarded.
    Failed,
}

/// Wait (overlapped, cancellable) for a client to connect to `instance`, bailing
/// out promptly when `stop` is set.
///
/// The overlapped `ConnectNamedPipe` returns `ERROR_IO_PENDING`; we wait on its
/// event in [`WAIT_SLICE`] slices so the stop flag is honoured without any
/// self-connect nudge. There is no time deadline — an idle server legitimately
/// waits forever for the next client, but only until shutdown.
fn connect_instance(instance: HANDLE, stop: &AtomicBool) -> ConnectOutcome {
    let Ok(event) = Event::new() else {
        return ConnectOutcome::Failed;
    };
    let mut ov = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    // SAFETY: `instance` is a freshly created, unconnected overlapped pipe
    // instance; `ov`/`event` outlive the op (any pending op is reaped below via
    // `wait_pending` before this frame returns).
    let initiate = last_win32(unsafe { ConnectNamedPipe(instance, Some(&mut ov)) });
    match initiate {
        // Rare, but an overlapped connect can complete synchronously.
        Ok(()) => ConnectOutcome::Connected,
        // A client that connected between create and connect surfaces as
        // ERROR_PIPE_CONNECTED, which is success.
        Err(code) if code == ERROR_PIPE_CONNECTED => ConnectOutcome::Connected,
        Err(code) if code == ERROR_IO_PENDING => {
            // SAFETY: initiate returned ERROR_IO_PENDING; `ov`/`event` are live
            // and `event` is `ov`'s `hEvent`. `wait_pending` reaps before return.
            match unsafe { wait_pending(instance, &mut ov, event.0, None, Some(stop)) } {
                // A client connected. `Eof` means it connected then closed
                // immediately; treat it as connected — the ensuing read observes
                // the disconnect and the exchange ends cleanly.
                IoOutcome::Bytes(_) | IoOutcome::Eof => ConnectOutcome::Connected,
                IoOutcome::Interrupted => ConnectOutcome::Stopped,
                // No deadline was supplied, so `TimedOut` cannot occur; fold it
                // in with other faults defensively.
                IoOutcome::TimedOut | IoOutcome::Failed(_) => ConnectOutcome::Failed,
            }
        }
        Err(_) => ConnectOutcome::Failed,
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
            // returns an error we classify via GetLastError below. The handle is
            // opened FILE_FLAG_OVERLAPPED so the client's own reads/writes go
            // through the same bounded [`PipeStream`] path — a stuck server
            // cannot hang `dujactl` past the read timeout.
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(name_wide.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
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

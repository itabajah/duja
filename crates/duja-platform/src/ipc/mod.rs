//! Local IPC transport: a hardened Windows named pipe carrying the pure
//! [`duja_ipc`] protocol.
//!
//! The protocol itself ([`duja_ipc`]) is transport-agnostic and
//! `#![forbid(unsafe_code)]`; all the OS `unsafe` for the pipe — creation with an
//! explicit DACL, the anti-squat flag, remote-client rejection, the client
//! PID/session check, and the read timeout — is confined here in
//! `duja-platform`, which already carries other Win32 FFI.
//!
//! # Security posture (SECURITY.md §IPC, plan §6)
//!
//! - The pipe name is per-user: `\\.\pipe\duja-<user-SID>`.
//! - An explicit DACL grants the current user only; there is no `Everyone` ACE.
//! - `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance defeats a squatter
//!   that pre-creates the name.
//! - `PIPE_REJECT_REMOTE_CLIENTS` refuses connections arriving over the network.
//! - Every accepted client is verified: its PID must resolve to a process in the
//!   **same session** as the server before any request is read.
//! - Reads are bounded by a 5 s timeout; at most [`MAX_CONNECTIONS`] pipe
//!   instances exist at once, so a flood is refused by the OS rather than
//!   exhausting the server.
//!
//! On non-Windows targets the transport is a no-op stub: the server accepts
//! nothing and the client always reports [`IpcTransportError::NotRunning`]
//! (the unix-socket transport lands with the macOS/Linux ports in P6/P7).

use std::time::Duration;

use duja_ipc::IpcError;

#[cfg(windows)]
mod win_pipe;

#[cfg(not(windows))]
mod noop;

#[cfg(windows)]
pub use win_pipe::{PipeClient, PipeServer, default_pipe_name};

#[cfg(not(windows))]
pub use noop::{PipeClient, PipeServer, default_pipe_name};

/// The maximum number of concurrent pipe instances the server keeps alive.
///
/// This is the OS-enforced ceiling passed as `nMaxInstances`: once this many
/// clients are connected, a further `CreateFile` from a client fails with
/// `ERROR_PIPE_BUSY` — a polite refusal, not a crash. Handling is done by a
/// small bounded pool (see [`MAX_HANDLER_THREADS`]).
pub const MAX_CONNECTIONS: u32 = 4;

/// The size of the handler thread pool that serves accepted connections.
///
/// Accepting (one listener thread) is decoupled from serving (this many worker
/// threads) so a slow client cannot stall the accept loop; up to
/// [`MAX_CONNECTIONS`] connections can be *accepted* while at most this many are
/// *being served* at once.
pub const MAX_HANDLER_THREADS: usize = 2;

/// The per-read timeout enforced on an accepted connection.
///
/// A client that connects but does not deliver a full request frame within this
/// window has its connection dropped, so a slow-loris writer cannot pin a
/// handler thread indefinitely.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// A failure from the local IPC transport.
///
/// The variants are cross-platform so `dujactl` and the app can branch on them
/// without `cfg`. The connection-establishment failures ([`NotRunning`],
/// [`Busy`], [`Timeout`]) are the signal for `dujactl` to fall back to direct
/// hardware access.
///
/// [`NotRunning`]: IpcTransportError::NotRunning
/// [`Busy`]: IpcTransportError::Busy
/// [`Timeout`]: IpcTransportError::Timeout
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IpcTransportError {
    /// No server is listening on the pipe (the app is not running).
    #[error("the Duja IPC server is not running")]
    NotRunning,
    /// The server is up but every instance is busy (too many connections).
    #[error("the Duja IPC server is busy: all {MAX_CONNECTIONS} instances are in use")]
    Busy,
    /// The connect attempt exceeded the caller's timeout.
    #[error("timed out connecting to the Duja IPC server")]
    Timeout,
    /// The peer failed the PID/session identity check and was refused.
    #[error("the IPC peer failed the session identity check")]
    Forbidden,
    /// A protocol-level failure (framing, version, or field validation) during
    /// the exchange.
    #[error("ipc protocol error: {0}")]
    Protocol(#[from] IpcError),
    /// An OS transport I/O failure, described in text (kept string-typed so the
    /// public surface is identical on every target).
    #[error("ipc transport error: {0}")]
    Io(String),
    /// This platform has no IPC transport yet (non-Windows).
    #[error("local IPC is not supported on this platform")]
    Unsupported,
}

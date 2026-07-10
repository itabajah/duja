//! Non-Windows no-op IPC transport.
//!
//! There is no local IPC on macOS/Linux yet (a unix-socket transport lands in
//! P6/P7). The server accepts nothing; the client always reports
//! [`IpcTransportError::NotRunning`] so `dujactl` transparently falls back to
//! direct hardware access.

use std::time::Duration;

use duja_ipc::{Request, Response};

use super::IpcTransportError;

/// The pipe name that *would* be used — a stable placeholder off-Windows so
/// diagnostics still have something to print.
#[must_use]
pub fn default_pipe_name() -> String {
    "duja-ipc-unsupported".to_owned()
}

/// A no-op server handle: constructing one starts nothing, dropping it stops
/// nothing.
#[derive(Debug)]
pub struct PipeServer {
    _private: (),
}

impl PipeServer {
    /// Accepts a handler and immediately returns a no-op server (nothing is
    /// ever dispatched to it).
    ///
    /// # Errors
    /// Never; the signature mirrors the Windows transport for a `cfg`-free
    /// caller.
    pub fn serve<H>(handler: H) -> Result<Self, IpcTransportError>
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        let _ = handler;
        Ok(PipeServer { _private: () })
    }

    /// As [`serve`](Self::serve), ignoring the requested name.
    ///
    /// # Errors
    /// Never.
    pub fn serve_named<H>(name: &str, handler: H) -> Result<Self, IpcTransportError>
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        let _ = name;
        Self::serve(handler)
    }

    /// No-op teardown.
    pub fn shutdown(self) {}
}

/// A client that can never connect on this platform.
#[derive(Debug)]
pub struct PipeClient {
    _private: (),
}

impl PipeClient {
    /// Always fails with [`IpcTransportError::NotRunning`].
    ///
    /// # Errors
    /// Always [`IpcTransportError::NotRunning`].
    pub fn connect(timeout: Duration) -> Result<Self, IpcTransportError> {
        let _ = timeout;
        Err(IpcTransportError::NotRunning)
    }

    /// Always fails with [`IpcTransportError::NotRunning`].
    ///
    /// # Errors
    /// Always [`IpcTransportError::NotRunning`].
    pub fn connect_named(name: &str, timeout: Duration) -> Result<Self, IpcTransportError> {
        let _ = (name, timeout);
        Err(IpcTransportError::NotRunning)
    }

    /// Unreachable (no client can be constructed).
    ///
    /// # Errors
    /// Always [`IpcTransportError::Unsupported`].
    pub fn request(&mut self, request: &Request) -> Result<Response, IpcTransportError> {
        let _ = request;
        Err(IpcTransportError::Unsupported)
    }
}

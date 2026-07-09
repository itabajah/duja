//! The low-level VCP transport seam.
//!
//! [`VcpTransport`] is the *only* thing the [`crate::controller::DdcController`]
//! knows about the wire: three primitive operations — read a VCP feature, write
//! a VCP feature, read the capability string — plus a small, classified error
//! type. Every policy decision (pacing, retry, verify-by-readback, quirks) lives
//! above this line in the controller, so the transport stays a thin, swappable
//! shim: the real dxva2 implementation on Windows, a scriptable fake in tests.

// RATIONALE: the module's public vocabulary (`VcpTransport`, `TransportError`)
// deliberately shares the `transport` stem; the qualified names read best at
// call sites and the surface is small and frozen.
#![allow(clippy::module_name_repetitions)]

use std::error::Error;

/// A single VCP feature reading: the current value and the maximum the display
/// reports for it.
///
/// Note that `max` is *untrusted* for non-continuous features (e.g. input
/// source, VCP `0x60`): real monitors report nonsense there (the P1 spike saw
/// `current = 15`, `max = 3`). Only the controller, armed with the capability
/// string and quirks, decides what a value means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcpReading {
    /// The current raw value the display returned.
    pub current: u16,
    /// The maximum raw value the display reported.
    pub max: u16,
}

/// A classified low-level transport failure.
///
/// The controller maps these onto [`duja_core::controller::ControlError`]:
/// [`Disconnected`](TransportError::Disconnected) is terminal (the monitor is
/// gone), [`Timeout`](TransportError::Timeout) is the common transient DDC
/// no-reply that retries recover from, and
/// [`Backend`](TransportError::Backend) carries anything else.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The display's handle is no longer valid — unplugged, powered off, or the
    /// graphics device was removed. Terminal; do not treat as retryable success.
    #[error("the display is disconnected or its handle is no longer valid")]
    Disconnected,
    /// The DDC/CI exchange did not complete: no reply, or a rejected request.
    /// The common transient failure; the controller retries it.
    #[error("the DDC/CI operation did not complete in time")]
    Timeout,
    /// Any other backend-specific failure.
    #[error("transport backend error: {0}")]
    Backend(#[source] Box<dyn Error + Send + Sync>),
}

impl TransportError {
    /// Wrap any `Send + Sync` error as a [`TransportError::Backend`].
    pub fn backend<E>(err: E) -> Self
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        TransportError::Backend(err.into())
    }
}

/// The minimal per-display wire interface the controller drives.
///
/// `&mut self` mirrors the controller's own exclusive ownership: a transport is
/// never shared, so implementations need no interior locking. `Send` lets the
/// owning controller move onto a per-monitor worker thread; `Debug` lets it be
/// logged.
pub trait VcpTransport: Send + std::fmt::Debug {
    /// Read VCP feature `code`, returning its current value and maximum.
    ///
    /// # Errors
    /// A [`TransportError`] if the exchange fails; the controller decides
    /// whether to retry (transient) or surface it (disconnected).
    fn read_vcp(&mut self, code: u8) -> Result<VcpReading, TransportError>;

    /// Write `value` to VCP feature `code`.
    ///
    /// # Errors
    /// A [`TransportError`] if the write is not acknowledged.
    fn write_vcp(&mut self, code: u8, value: u16) -> Result<(), TransportError>;

    /// Read the display's raw MCCS capability string.
    ///
    /// # Errors
    /// A [`TransportError`]; capability reads are slow and flaky, so the
    /// controller retries and, on persistent failure, falls back to probing
    /// individual VCP codes.
    fn read_capabilities(&mut self) -> Result<String, TransportError>;
}

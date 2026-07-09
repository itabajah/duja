//! The [`PanelTransport`] abstraction: the minimal panel-brightness operations
//! a platform backend must provide, decoupled from the
//! [`duja_core::controller::BrightnessController`] adapter that consumes them.
//!
//! Splitting the transport out lets the OS-specific, `unsafe`-carrying WMI code
//! ([`crate::wmi`], Windows only) and a deterministic in-memory fake share one
//! [`crate::controller::PanelController`] implementation, so the whole control
//! adapter is exercised by the cross-platform contract suite.

use std::fmt::Debug;

use crate::error::PanelError;

/// A panel's current brightness and the discrete levels it accepts.
///
/// All values are **percentages** (`0..=100`): Windows `WmiMonitorBrightness`
/// is percent-based, so the backend has no raw hardware range to expose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelBrightness {
    /// The current brightness, `0..=100`.
    pub current: u8,
    /// The discrete brightness levels the panel accepts, ascending. Informative
    /// only — the controller reports a continuous `0..=100` range and lets the
    /// platform snap to the nearest supported level.
    pub levels: Vec<u8>,
}

/// The minimal set of operations the panel control adapter needs from a
/// platform backend.
///
/// `Send + Debug` mirror the [`BrightnessController`](duja_core::controller::BrightnessController)
/// bounds so a [`crate::controller::PanelController`] wrapping any transport can
/// be moved onto a per-display worker thread and logged.
///
/// # Threading
/// A transport is constructed on, and used from, a **single** worker thread
/// (the Windows implementation initializes a COM apartment on that thread). It
/// is `Send` — it may be moved to its worker — but is not required to be `Sync`.
pub trait PanelTransport: Send + Debug {
    /// Read the panel's current brightness and its supported levels.
    ///
    /// # Errors
    /// [`PanelError`] if the panel cannot be reached or the query fails.
    fn query(&mut self) -> Result<PanelBrightness, PanelError>;

    /// Set the panel's brightness to `percent` (`0..=100`, already clamped by
    /// the caller).
    ///
    /// # Errors
    /// [`PanelError`] if the panel cannot be reached or the write fails.
    fn set_brightness(&mut self, percent: u8) -> Result<(), PanelError>;
}

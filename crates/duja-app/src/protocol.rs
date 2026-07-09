//! The internal wire protocol between the engine actor and its per-monitor
//! workers, plus the engine's in-flight watchdog key.
//!
//! These types never cross the crate boundary; the public surface is in
//! [`crate`]'s root.

use duja_core::controller::ControlError;
use duja_core::id::StableDisplayId;
use duja_core::model::{Feature, FeatureRange};

/// A command from the engine to one worker.
#[derive(Debug)]
pub(crate) enum WorkerCommand {
    /// Write `raw` to `feature`; the worker coalesces latest-wins per feature
    /// and enforces the engine's min-gap before performing it.
    Set {
        /// Which VCP feature to write.
        feature: Feature,
        /// The already-scaled raw value.
        raw: u16,
        /// Engine-assigned sequence number for watchdog matching.
        seq: u64,
    },
    /// Read `feature`'s current value/max (used once on add to learn the
    /// hardware level). Not coalesced.
    Get {
        /// Which VCP feature to read.
        feature: Feature,
        /// Engine-assigned sequence number for watchdog matching.
        seq: u64,
    },
    /// Finish any in-flight op, then exit the worker loop.
    Shutdown,
}

/// Identifies one outstanding operation for the watchdog, per display.
///
/// Only the *latest* operation of each kind per display is watched: coalesced
/// (superseded) writes are replaced in the in-flight map before they are ever
/// acked, so they cannot trip the watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum InflightKey {
    /// A pending [`WorkerCommand::Set`] for a feature.
    Set(Feature),
    /// A pending [`WorkerCommand::Get`] for a feature.
    Get(Feature),
}

/// The result of a worker operation, reported back to the engine.
#[derive(Debug)]
pub(crate) enum AckOutcome {
    /// A `Set` completed. The engine acts on writes only via the watchdog and
    /// panic paths, so a per-write backend result is deliberately not carried —
    /// a genuinely gone display is caught by the next enumeration's `Removed`.
    Set {
        /// The feature written.
        feature: Feature,
        /// The sequence number of the performed write.
        seq: u64,
    },
    /// A `Get` completed.
    Get {
        /// The feature read.
        feature: Feature,
        /// The sequence number of the performed read.
        seq: u64,
        /// The backend result.
        result: Result<FeatureRange, ControlError>,
    },
    /// The controller panicked during an operation; the worker is exiting.
    Panicked {
        /// The operation that was in progress when the panic occurred.
        key: InflightKey,
        /// The sequence number of that operation.
        seq: u64,
    },
}

/// A worker's reply to the engine, tagged with the worker's display id.
#[derive(Debug)]
pub(crate) struct WorkerAck {
    /// The display this ack came from.
    pub(crate) id: StableDisplayId,
    /// What happened.
    pub(crate) outcome: AckOutcome,
}

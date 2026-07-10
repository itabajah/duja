//! [`DdcController`]: the transport-agnostic policy layer.
//!
//! A [`DdcController`] wraps a [`VcpTransport`] and turns the three raw wire
//! operations into a robust [`BrightnessController`]. It owns *all* the
//! behaviour that makes real DDC/CI monitors usable, none of which belongs in a
//! transport:
//!
//! - **Pacing.** A minimum gap is enforced between *every* DDC operation (not
//!   just writes): the P1 spike measured 60–70% of unpaced back-to-back reads
//!   failing. The gap defaults to [`DEFAULT_MIN_GAP`] and is overridable per
//!   monitor by the `min_write_gap_ms` quirk.
//! - **Retry with back-off.** Reads and writes retry a bounded number of times
//!   with exponential back-off; capability reads retry `caps_retry` times.
//! - **Verify-by-readback.** When the `verify_writes` quirk is set, every write
//!   is read back and re-issued on mismatch.
//! - **Quirks.** `max_brightness` overrides a bogus reported maximum,
//!   `caps_unreliable` / a failed capability read fall back to probing VCP
//!   `0x10`/`0x12` directly, `ddc_broken` forces an empty (software-only)
//!   capability set, `no_input_switch` drops a broken input-source feature, and
//!   `input_source_allowed` gates input-source writes.
//!
//! Time is injected through a [`Clock`] so pacing and back-off are
//! deterministically testable without ever sleeping a real thread.

// RATIONALE: `DdcController` shares the `controller` module stem; the qualified
// name reads best at call sites and the type name is fixed by the plan.
#![allow(clippy::module_name_repetitions)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use duja_core::caps::ParsedCaps;
use duja_core::controller::{BrightnessController, ControlError};
use duja_core::id::StableDisplayId;
use duja_core::model::{Capabilities, Feature, FeatureRange};
use duja_core::quirks::{QuirkDb, ResolvedQuirks};

use crate::clock::{Clock, SystemClock};
use crate::transport::{TransportError, VcpReading, VcpTransport};

/// The default minimum gap between DDC operations when no quirk overrides it.
///
/// Conservative on purpose: the ADR-0002 evidence is that unpaced DDC traffic
/// is unreliable, and 100 ms is comfortably above the ~40–50 ms floor the spike
/// measured while staying imperceptible for interactive brightness changes.
pub const DEFAULT_MIN_GAP: Duration = Duration::from_millis(100);

/// Total attempts (initial try plus retries) for a single VCP read or write.
const OP_ATTEMPTS: u32 = 3;

/// Total attempts for a capability-string read when no `caps_retry` quirk sets
/// one. Capability reads are the slowest and flakiest operation.
const DEFAULT_CAPS_ATTEMPTS: u32 = 3;

/// How many times a verified write is re-issued when the readback disagrees.
const VERIFY_ATTEMPTS: u32 = 3;

/// The tolerance, in raw VCP units, allowed between a written value and its
/// readback before a verified write is considered to have failed. Monitors
/// occasionally round to their own step size.
const VERIFY_TOLERANCE: u16 = 2;

/// The base back-off between retries; doubled each attempt up to [`BACKOFF_MAX`].
const BACKOFF_BASE: Duration = Duration::from_millis(20);

/// The ceiling on a single retry back-off.
const BACKOFF_MAX: Duration = Duration::from_millis(320);

/// A DDC/CI [`BrightnessController`] over an injectable [`VcpTransport`] clock.
///
/// See the [module documentation](self) for the policy this type implements.
/// Construct one for a real display with [`DdcController::new`] (system clock,
/// quirks resolved from the embedded database), or with explicit parts via
/// [`DdcController::with_parts`] for tests.
pub struct DdcController<T: VcpTransport, C: Clock = SystemClock> {
    transport: T,
    clock: C,
    quirks: ResolvedQuirks,
    min_gap: Duration,
    caps: Option<Capabilities>,
    cached_max: BTreeMap<Feature, u16>,
    last_op: Option<Instant>,
}

impl<T: VcpTransport, C: Clock> fmt::Debug for DdcController<T, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DdcController")
            .field("transport", &self.transport)
            .field("min_gap", &self.min_gap)
            .field("quirks", &self.quirks)
            .field("probed", &self.caps.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: VcpTransport> DdcController<T, SystemClock> {
    /// Build a controller for the display identified by `id`, resolving its
    /// quirks from the embedded [`QuirkDb`] and driving real wall-clock time.
    #[must_use]
    pub fn new(transport: T, id: &StableDisplayId) -> Self {
        let quirks = QuirkDb::embedded().resolve(id);
        Self::with_parts(transport, quirks, SystemClock)
    }
}

impl<T: VcpTransport, C: Clock> DdcController<T, C> {
    /// Build a controller from explicit parts: a transport, an already-resolved
    /// [`ResolvedQuirks`], and a [`Clock`].
    ///
    /// This is the seam tests use to inject a fake transport and a virtual
    /// clock, and to exercise quirks that are not in the embedded database.
    #[must_use]
    pub fn with_parts(transport: T, quirks: ResolvedQuirks, clock: C) -> Self {
        let min_gap = quirks.min_write_gap(DEFAULT_MIN_GAP);
        DdcController {
            transport,
            clock,
            quirks,
            min_gap,
            caps: None,
            cached_max: BTreeMap::new(),
            last_op: None,
        }
    }

    /// Borrow the underlying transport (for inspection in tests).
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Borrow the injected clock (for inspection in tests).
    #[must_use]
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Borrow the resolved quirks in effect for this display.
    #[must_use]
    pub fn quirks(&self) -> &ResolvedQuirks {
        &self.quirks
    }

    /// The effective minimum inter-operation gap for this display.
    #[must_use]
    pub fn min_gap(&self) -> Duration {
        self.min_gap
    }

    // --- pacing + back-off ------------------------------------------------

    /// Enforce the minimum gap since the previous DDC operation, then record
    /// this operation's start time. Called immediately before every wire op.
    fn pace(&mut self) {
        if let Some(last) = self.last_op {
            let elapsed = self.clock.now().saturating_duration_since(last);
            if elapsed < self.min_gap {
                self.clock.sleep(self.min_gap.saturating_sub(elapsed));
            }
        }
        self.last_op = Some(self.clock.now());
    }

    /// Sleep the exponential back-off for a zero-based retry `attempt`.
    fn backoff(&mut self, attempt: u32) {
        let shifted = BACKOFF_BASE
            .checked_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
            .unwrap_or(BACKOFF_MAX);
        self.clock.sleep(shifted.min(BACKOFF_MAX));
    }

    // --- retrying wire primitives -----------------------------------------

    /// Read a VCP feature, pacing and retrying up to [`OP_ATTEMPTS`] times.
    fn read_retry(&mut self, code: u8) -> Result<VcpReading, TransportError> {
        let mut last = TransportError::Timeout;
        for attempt in 0..OP_ATTEMPTS {
            self.pace();
            match self.transport.read_vcp(code) {
                Ok(reading) => return Ok(reading),
                Err(err) => last = err,
            }
            if attempt.saturating_add(1) < OP_ATTEMPTS {
                self.backoff(attempt);
            }
        }
        Err(last)
    }

    /// Write a VCP feature, pacing and retrying up to [`OP_ATTEMPTS`] times.
    fn write_retry(&mut self, code: u8, value: u16) -> Result<(), TransportError> {
        let mut last = TransportError::Timeout;
        for attempt in 0..OP_ATTEMPTS {
            self.pace();
            match self.transport.write_vcp(code, value) {
                Ok(()) => return Ok(()),
                Err(err) => last = err,
            }
            if attempt.saturating_add(1) < OP_ATTEMPTS {
                self.backoff(attempt);
            }
        }
        Err(last)
    }

    /// Read the capability string, retrying `caps_retry` (or the default) times.
    fn caps_retry(&mut self) -> Result<String, TransportError> {
        let attempts = self
            .quirks
            .caps_retry
            .unwrap_or(DEFAULT_CAPS_ATTEMPTS)
            .max(1);
        let mut last = TransportError::Timeout;
        for attempt in 0..attempts {
            self.pace();
            match self.transport.read_capabilities() {
                Ok(caps) => return Ok(caps),
                Err(err) => last = err,
            }
            if attempt.saturating_add(1) < attempts {
                self.backoff(attempt);
            }
        }
        Err(last)
    }

    // --- probing ----------------------------------------------------------

    /// Probe capabilities, applying quirks and caching the result.
    fn probe_inner(&mut self) -> Result<Capabilities, TransportError> {
        if self.quirks.ddc_broken {
            // Forced software-only: no hardware range, no features, no wire I/O.
            return Ok(Capabilities {
                features: BTreeSet::new(),
                hardware_range: false,
                raw_capabilities: None,
                allowed_inputs: Vec::new(),
            });
        }

        let mut caps = if self.quirks.caps_unreliable {
            self.probe_by_reads()?
        } else {
            match self.caps_retry() {
                Ok(raw) => {
                    let parsed = ParsedCaps::parse(&raw).unwrap_or_default();
                    let mut caps = intersect_features(&parsed);
                    caps.allowed_inputs = self.resolve_allowed_inputs(caps.allowed_inputs);
                    caps.raw_capabilities = Some(raw);
                    caps
                }
                // A disconnected display is terminal; anything else falls back
                // to probing individual VCP codes (caps reads are the flakiest).
                Err(TransportError::Disconnected) => return Err(TransportError::Disconnected),
                Err(_) => self.probe_by_reads()?,
            }
        };

        if self.quirks.no_input_switch {
            caps.features.remove(&Feature::InputSource);
            caps.allowed_inputs.clear();
        }
        Ok(caps)
    }

    /// Fallback discovery: attempt a direct read of brightness and contrast; a
    /// successful read means the feature is present. A disconnected error is
    /// propagated; any other read failure means the feature is simply absent.
    fn probe_by_reads(&mut self) -> Result<Capabilities, TransportError> {
        let mut features = BTreeSet::new();
        for feature in [Feature::Brightness, Feature::Contrast] {
            match self.read_retry(feature.vcp_code()) {
                Ok(_) => {
                    features.insert(feature);
                }
                Err(TransportError::Disconnected) => return Err(TransportError::Disconnected),
                Err(_) => {}
            }
        }
        let hardware_range = features.contains(&Feature::Brightness);
        // No capability string here, so the only input-source knowledge is a
        // quirk override (empty when there is none or switching is disabled).
        let allowed_inputs = self.resolve_allowed_inputs(Vec::new());
        Ok(Capabilities {
            features,
            hardware_range,
            raw_capabilities: None,
            allowed_inputs,
        })
    }

    /// Resolve the effective allowed input-source set for this display:
    /// intersect the capability-string value list (`from_caps`) with the
    /// `input_source_allowed` quirk when present, or fall back to the quirk when
    /// the caps string carried no list. A `no_input_switch` display is cleared by
    /// the caller.
    fn resolve_allowed_inputs(&self, from_caps: Vec<u8>) -> Vec<u8> {
        match self.quirks.input_source_allowed.as_deref() {
            None => from_caps,
            Some(quirk) if from_caps.is_empty() => quirk.to_vec(),
            Some(quirk) => from_caps
                .into_iter()
                .filter(|code| quirk.contains(code))
                .collect(),
        }
    }

    /// Whether a prior probe positively established that `feature` is
    /// unsupported. Absent a probe we do not claim to know.
    fn known_unsupported(&self, feature: Feature) -> bool {
        self.caps
            .as_ref()
            .is_some_and(|caps| !caps.supports(feature))
    }

    /// Apply the `max_brightness` override (brightness only) to a raw reading.
    fn with_max_override(&self, feature: Feature, reading: VcpReading) -> FeatureRange {
        if feature == Feature::Brightness
            && let Some(max) = self.quirks.max_brightness
        {
            return FeatureRange {
                current: reading.current.min(max),
                max,
            };
        }
        FeatureRange {
            current: reading.current,
            max: reading.max,
        }
    }

    /// The clamp ceiling for a continuous feature: the `max_brightness` quirk if
    /// set, else a cached max, else discovered by a read.
    fn effective_max(&mut self, feature: Feature) -> Result<u16, TransportError> {
        if feature == Feature::Brightness
            && let Some(max) = self.quirks.max_brightness
        {
            return Ok(max);
        }
        if let Some(&max) = self.cached_max.get(&feature) {
            return Ok(max);
        }
        let reading = self.read_retry(feature.vcp_code())?;
        let max = self.with_max_override(feature, reading).max;
        self.cached_max.insert(feature, max);
        Ok(max)
    }

    // --- feature writes ---------------------------------------------------

    /// Write a continuous feature (brightness/contrast): clamp to the effective
    /// maximum, then optionally verify by readback and re-issue on mismatch.
    fn set_continuous(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
        let max = self.effective_max(feature).map_err(control_error)?;
        let clamped = value.min(max);

        if !self.quirks.verify_writes {
            self.write_retry(feature.vcp_code(), clamped)
                .map_err(control_error)?;
            self.cached_max.insert(feature, max);
            return Ok(());
        }

        let mut last = ControlError::Timeout;
        for attempt in 0..VERIFY_ATTEMPTS {
            self.write_retry(feature.vcp_code(), clamped)
                .map_err(control_error)?;
            match self.read_retry(feature.vcp_code()) {
                Ok(reading) if reading.current.abs_diff(clamped) <= VERIFY_TOLERANCE => {
                    self.cached_max.insert(feature, max);
                    return Ok(());
                }
                Ok(reading) => {
                    last = ControlError::backend(VerifyMismatch {
                        wrote: clamped,
                        read: reading.current,
                    });
                }
                Err(err) => last = control_error(err),
            }
            if attempt.saturating_add(1) < VERIFY_ATTEMPTS {
                self.backoff(attempt);
            }
        }
        Err(last)
    }

    /// Write the input source (VCP `0x60`), gating on `input_source_allowed`.
    /// The value is never verified by readback — input-source metadata is
    /// unreliable (ADR-0002) — and never clamped against a reported maximum.
    fn set_input_source(&mut self, value: u16) -> Result<(), ControlError> {
        if let Some(allowed) = self.quirks.input_source_allowed.as_deref() {
            let permitted = u8::try_from(value).is_ok_and(|byte| allowed.contains(&byte));
            if !permitted {
                return Err(ControlError::backend(DisallowedInputSource { value }));
            }
        }
        self.write_retry(Feature::InputSource.vcp_code(), value)
            .map_err(control_error)
    }
}

impl<T: VcpTransport, C: Clock> BrightnessController for DdcController<T, C> {
    fn probe(&mut self) -> Result<Capabilities, ControlError> {
        let caps = self.probe_inner().map_err(control_error)?;
        self.caps = Some(caps.clone());
        Ok(caps)
    }

    fn get(&mut self, feature: Feature) -> Result<FeatureRange, ControlError> {
        if self.known_unsupported(feature) {
            return Err(ControlError::Unsupported);
        }
        let reading = self.read_retry(feature.vcp_code()).map_err(control_error)?;
        let range = self.with_max_override(feature, reading);
        // Cache the continuous maxima only; input-source "max" is untrusted.
        if feature != Feature::InputSource {
            self.cached_max.insert(feature, range.max);
        }
        Ok(range)
    }

    fn set(&mut self, feature: Feature, value: u16) -> Result<(), ControlError> {
        if self.known_unsupported(feature) {
            return Err(ControlError::Unsupported);
        }
        match feature {
            Feature::InputSource => self.set_input_source(value),
            Feature::Brightness | Feature::Contrast => self.set_continuous(feature, value),
        }
    }
}

/// Map a low-level [`TransportError`] onto the trait's [`ControlError`].
fn control_error(err: TransportError) -> ControlError {
    match err {
        TransportError::Disconnected => ControlError::Disconnected,
        TransportError::Timeout => ControlError::Timeout,
        TransportError::Backend(inner) => ControlError::Backend(inner),
    }
}

/// Build a [`Capabilities`] from a parsed capability string, keeping only the
/// VCP codes Duja controls (`0x10`/`0x12`/`0x60`) and the `0x60` value list.
fn intersect_features(parsed: &ParsedCaps) -> Capabilities {
    let mut features = BTreeSet::new();
    for feature in Feature::ALL {
        if parsed.supports(feature.vcp_code()) {
            features.insert(feature);
        }
    }
    let hardware_range = features.contains(&Feature::Brightness);
    let allowed_inputs = parsed
        .allowed_values(Feature::InputSource.vcp_code())
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    Capabilities {
        features,
        hardware_range,
        raw_capabilities: None,
        allowed_inputs,
    }
}

/// A verified write whose readback did not match what was written.
#[derive(Debug)]
struct VerifyMismatch {
    wrote: u16,
    read: u16,
}

impl fmt::Display for VerifyMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "write verification failed: wrote {}, read back {}",
            self.wrote, self.read
        )
    }
}

impl std::error::Error for VerifyMismatch {}

/// An input-source write whose value is not in the display's allowed set.
#[derive(Debug)]
struct DisallowedInputSource {
    value: u16,
}

impl fmt::Display for DisallowedInputSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "input-source value {:#04x} is not in the display's allowed set",
            self.value
        )
    }
}

impl std::error::Error for DisallowedInputSource {}

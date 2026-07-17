//! Deterministic test doubles: a scriptable [`FakeTransport`] and a virtual
//! [`TestClock`] that records its sleeps. Compiled only under `cfg(test)`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use duja_core::testing::FakeClock;

use crate::clock::Clock;
use crate::ddcci::{DdcWire, I2cBus, encode_caps_reply, encode_get_vcp_reply};
use crate::transport::{TransportError, VcpReading, VcpTransport};

/// Which error a [`FakeTransport`] injects while its failure budget is not
/// exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectKind {
    /// Yield [`TransportError::Timeout`] (a retryable transient failure).
    Timeout,
    /// Yield [`TransportError::Disconnected`] (terminal).
    Disconnected,
}

/// A scriptable [`VcpTransport`] for deterministic controller tests.
///
/// Every operation is logged. Failure budgets (`fail_reads`, `fail_writes`,
/// `fail_caps`) are consumed one per *attempt*, so a budget of `N` fails the
/// first `N` attempts the controller makes and then behaves normally — which is
/// exactly how the controller's own retry count is exercised.
#[derive(Debug, Clone)]
pub struct FakeTransport {
    connected: bool,
    caps_string: Option<String>,
    values: BTreeMap<u8, VcpReading>,
    inject: InjectKind,
    fail_reads: usize,
    fail_writes: usize,
    fail_caps: usize,
    ignore_writes: bool,
    read_back_drift: u16,
    pub reads: Vec<u8>,
    pub writes: Vec<(u8, u16)>,
    pub caps_calls: usize,
}

impl FakeTransport {
    /// A healthy monitor: reports brightness (`0x10`) and contrast (`0x12`),
    /// each seeded at current 50 / max 100, with a matching capability string.
    #[must_use]
    pub fn nominal() -> Self {
        let mut values = BTreeMap::new();
        values.insert(
            0x10,
            VcpReading {
                current: 50,
                max: 100,
            },
        );
        values.insert(
            0x12,
            VcpReading {
                current: 50,
                max: 100,
            },
        );
        FakeTransport {
            connected: true,
            caps_string: Some("(vcp(10 12))".to_owned()),
            values,
            inject: InjectKind::Timeout,
            fail_reads: 0,
            fail_writes: 0,
            fail_caps: 0,
            ignore_writes: false,
            read_back_drift: 0,
            reads: Vec::new(),
            writes: Vec::new(),
            caps_calls: 0,
        }
    }

    /// A monitor that answers every operation with `Disconnected`.
    #[must_use]
    pub fn disconnected() -> Self {
        let mut t = Self::nominal();
        t.connected = false;
        t
    }

    /// Replace the capability string (`None` makes capability reads fail).
    #[must_use]
    pub fn with_caps(mut self, caps: Option<&str>) -> Self {
        self.caps_string = caps.map(str::to_owned);
        self
    }

    /// Seed or overwrite a VCP code's reading.
    #[must_use]
    pub fn with_value(mut self, code: u8, current: u16, max: u16) -> Self {
        self.values.insert(code, VcpReading { current, max });
        self
    }

    /// Remove a VCP code so reads of it fail (feature absent).
    #[must_use]
    pub fn without_value(mut self, code: u8) -> Self {
        self.values.remove(&code);
        self
    }

    /// Set the injected error kind and the per-operation failure budgets.
    #[must_use]
    pub fn failing(mut self, kind: InjectKind, reads: usize, writes: usize, caps: usize) -> Self {
        self.inject = kind;
        self.fail_reads = reads;
        self.fail_writes = writes;
        self.fail_caps = caps;
        self
    }

    /// Make writes no-ops so verified writes see a stale readback (mismatch).
    #[must_use]
    pub fn ignoring_writes(mut self) -> Self {
        self.ignore_writes = true;
        self
    }

    /// Make every stored write read back `delta` raw units above what was
    /// written, modelling a monitor that quantises a written value to its own
    /// coarser step. The default fake stores writes exactly (`delta` 0); this
    /// opt-in seam drives the controller's verify-by-readback tolerance window,
    /// so a verified write of `v` reads back `v + delta` and the controller sees
    /// a drift of exactly `delta` (the offset is added saturating, then clamped
    /// to the feature's max like any stored value).
    #[must_use]
    pub fn drifting(mut self, delta: u16) -> Self {
        self.read_back_drift = delta;
        self
    }

    fn inject_err(&self) -> TransportError {
        match self.inject {
            InjectKind::Timeout => TransportError::Timeout,
            InjectKind::Disconnected => TransportError::Disconnected,
        }
    }
}

impl VcpTransport for FakeTransport {
    fn read_vcp(&mut self, code: u8) -> Result<VcpReading, TransportError> {
        self.reads.push(code);
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        if self.fail_reads > 0 {
            self.fail_reads = self.fail_reads.saturating_sub(1);
            return Err(self.inject_err());
        }
        self.values
            .get(&code)
            .copied()
            .ok_or(TransportError::Timeout)
    }

    fn write_vcp(&mut self, code: u8, value: u16) -> Result<(), TransportError> {
        self.writes.push((code, value));
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        if self.fail_writes > 0 {
            self.fail_writes = self.fail_writes.saturating_sub(1);
            return Err(self.inject_err());
        }
        if !self.ignore_writes {
            let max = self.values.get(&code).map_or(100, |r| r.max);
            self.values.insert(
                code,
                VcpReading {
                    current: value.saturating_add(self.read_back_drift).min(max),
                    max,
                },
            );
        }
        Ok(())
    }

    fn read_capabilities(&mut self) -> Result<String, TransportError> {
        self.caps_calls = self.caps_calls.saturating_add(1);
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        if self.fail_caps > 0 {
            self.fail_caps = self.fail_caps.saturating_sub(1);
            return Err(self.inject_err());
        }
        self.caps_string.clone().ok_or(TransportError::Timeout)
    }
}

/// The DDC/CI op-code a request body starts with, used by [`FakeI2cBus`] to
/// route a written packet. These mirror the private constants in `crate::ddcci`.
const OP_GET_VCP: u8 = 0x01;
const OP_SET_VCP: u8 = 0x03;
const OP_CAPS_REQUEST: u8 = 0xF3;

/// The most payload bytes a display returns in one capabilities-reply fragment.
const CAPS_FRAGMENT: usize = 32;

/// A scriptable [`I2cBus`] that simulates a DDC/CI monitor at the **byte** level:
/// it parses the framed request the [`DdcCiTransport`](crate::ddcci::DdcCiTransport)
/// writes and answers with a properly framed reply. Wiring it under the real
/// transport binds the macOS controller logic — packet framing, checksum, reply
/// parsing, retry, pacing, verify — into the cross-platform contract suite, so
/// it is exercised on every OS in CI even though no Mac hardware exists.
///
/// Failure budgets (`fail_reads`, `fail_writes`, `fail_caps`) are consumed one
/// per *attempt*, exactly like [`FakeTransport`], so the controller's retry
/// counts are driven identically through both fakes.
#[derive(Debug, Clone)]
pub struct FakeI2cBus {
    wire: DdcWire,
    connected: bool,
    caps_string: Option<String>,
    values: BTreeMap<u8, VcpReading>,
    inject: InjectKind,
    fail_reads: usize,
    fail_writes: usize,
    fail_caps: usize,
    ignore_writes: bool,
    /// The reply the next [`read`](I2cBus::read) will serve.
    pending: Option<Vec<u8>>,
    pub writes: Vec<(u8, u16)>,
    pub reads: Vec<u8>,
    pub caps_calls: usize,
}

impl FakeI2cBus {
    /// A healthy monitor: brightness (`0x10`) and contrast (`0x12`) seeded at
    /// current 50 / max 100, with a matching capability string. Defaults to the
    /// Apple Silicon framing (the harder arm); the framing does not affect the
    /// simulated monitor's behaviour, only the request bytes.
    #[must_use]
    pub fn nominal() -> Self {
        let mut values = BTreeMap::new();
        values.insert(
            0x10,
            VcpReading {
                current: 50,
                max: 100,
            },
        );
        values.insert(
            0x12,
            VcpReading {
                current: 50,
                max: 100,
            },
        );
        FakeI2cBus {
            wire: DdcWire::AppleSilicon,
            connected: true,
            caps_string: Some("(vcp(10 12))".to_owned()),
            values,
            inject: InjectKind::Timeout,
            fail_reads: 0,
            fail_writes: 0,
            fail_caps: 0,
            ignore_writes: false,
            pending: None,
            writes: Vec::new(),
            reads: Vec::new(),
            caps_calls: 0,
        }
    }

    /// A monitor that answers every operation with `Disconnected`.
    #[must_use]
    pub fn disconnected() -> Self {
        let mut b = Self::nominal();
        b.connected = false;
        b
    }

    /// Select the request framing the simulated bus expects.
    #[must_use]
    pub fn with_wire(mut self, wire: DdcWire) -> Self {
        self.wire = wire;
        self
    }

    /// Set the injected error kind and per-operation failure budgets.
    #[must_use]
    pub fn failing(mut self, kind: InjectKind, reads: usize, writes: usize, caps: usize) -> Self {
        self.inject = kind;
        self.fail_reads = reads;
        self.fail_writes = writes;
        self.fail_caps = caps;
        self
    }

    /// Make writes no-ops so a verified write sees a stale readback (mismatch).
    #[must_use]
    pub fn ignoring_writes(mut self) -> Self {
        self.ignore_writes = true;
        self
    }

    fn inject_err(&self) -> TransportError {
        match self.inject {
            InjectKind::Timeout => TransportError::Timeout,
            InjectKind::Disconnected => TransportError::Disconnected,
        }
    }

    /// Extract the DDC/CI request body (op-code and args) from a framed packet.
    /// The body always starts at index 2 and its length lives in the low 7 bits
    /// of index 1 — true for both the Intel and Apple Silicon framings.
    fn request_body(data: &[u8]) -> Option<&[u8]> {
        let len = usize::from(data.get(1).copied()? & 0x7F);
        let end = len.saturating_add(2);
        data.get(2..end)
    }

    /// Build the capabilities-reply fragment for `offset` from the caps string.
    fn caps_fragment(&self, offset: u16) -> Vec<u8> {
        let caps = self.caps_string.as_deref().unwrap_or("");
        let start = usize::from(offset).min(caps.len());
        let end = start.saturating_add(CAPS_FRAGMENT).min(caps.len());
        let slice = caps.as_bytes().get(start..end).unwrap_or(&[]);
        encode_caps_reply(offset, slice)
    }
}

impl I2cBus for FakeI2cBus {
    fn wire(&self) -> DdcWire {
        self.wire
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        let Some(body) = Self::request_body(data) else {
            return Err(TransportError::Timeout);
        };
        match body.first().copied() {
            Some(OP_GET_VCP) => {
                let code = body.get(1).copied().unwrap_or(0);
                self.reads.push(code);
                if self.fail_reads > 0 {
                    self.fail_reads = self.fail_reads.saturating_sub(1);
                    return Err(self.inject_err());
                }
                // Result 0x01 (unsupported) for a code the monitor does not seed.
                self.pending = Some(match self.values.get(&code).copied() {
                    Some(reading) => encode_get_vcp_reply(code, reading, 0x00),
                    None => encode_get_vcp_reply(code, VcpReading { current: 0, max: 0 }, 0x01),
                });
                Ok(())
            }
            Some(OP_SET_VCP) => {
                let code = body.get(1).copied().unwrap_or(0);
                let hi = body.get(2).copied().unwrap_or(0);
                let lo = body.get(3).copied().unwrap_or(0);
                let value = u16::from_be_bytes([hi, lo]);
                self.writes.push((code, value));
                if self.fail_writes > 0 {
                    self.fail_writes = self.fail_writes.saturating_sub(1);
                    return Err(self.inject_err());
                }
                if !self.ignore_writes {
                    let max = self.values.get(&code).map_or(100, |r| r.max);
                    self.values.insert(
                        code,
                        VcpReading {
                            current: value.min(max),
                            max,
                        },
                    );
                }
                Ok(())
            }
            Some(OP_CAPS_REQUEST) => {
                let hi = body.get(1).copied().unwrap_or(0);
                let lo = body.get(2).copied().unwrap_or(0);
                let offset = u16::from_be_bytes([hi, lo]);
                if offset == 0 {
                    self.caps_calls = self.caps_calls.saturating_add(1);
                }
                if self.fail_caps > 0 {
                    self.fail_caps = self.fail_caps.saturating_sub(1);
                    return Err(self.inject_err());
                }
                if self.caps_string.is_none() {
                    // No capability string: the read fails, exercising the
                    // controller's probe-by-reads fallback.
                    return Err(TransportError::Timeout);
                }
                self.pending = Some(self.caps_fragment(offset));
                Ok(())
            }
            _ => Err(TransportError::Timeout),
        }
    }

    fn read(&mut self, _len: usize) -> Result<Vec<u8>, TransportError> {
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        self.pending.take().ok_or(TransportError::Timeout)
    }
}

/// A virtual [`Clock`] whose time advances only when the controller sleeps, and
/// which records every sleep duration for assertions.
#[derive(Debug, Clone)]
pub struct TestClock {
    clock: FakeClock,
    pub sleeps: Vec<Duration>,
}

impl TestClock {
    /// A fresh clock anchored at "now" with no recorded sleeps.
    #[must_use]
    pub fn new() -> Self {
        TestClock {
            clock: FakeClock::new(),
            sleeps: Vec::new(),
        }
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        self.clock.now()
    }

    fn sleep(&mut self, dur: Duration) {
        self.sleeps.push(dur);
        self.clock.advance(dur);
    }
}

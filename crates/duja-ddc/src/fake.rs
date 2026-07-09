//! Deterministic test doubles: a scriptable [`FakeTransport`] and a virtual
//! [`TestClock`] that records its sleeps. Compiled only under `cfg(test)`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use duja_core::testing::FakeClock;

use crate::clock::Clock;
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
                    current: value.min(max),
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

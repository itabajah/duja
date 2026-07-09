//! Real-hardware tests for the Windows dxva2 backend.
//!
//! These are **double-gated**: each is marked `#[ignore]` (so `cargo test`
//! never runs them by default) *and* returns early unless the environment
//! variable `DUJA_HW_TESTS=1` is set. Run them, with the MSI MP273QP attached,
//! via:
//!
//! ```powershell
//! $env:DUJA_HW_TESTS = "1"
//! cargo nextest run -p duja-ddc --run-ignored all
//! ```
//!
//! Safety: the tests only ever write **brightness** (VCP `0x10`) — never the
//! input source (`0x60`), which would switch the monitor's input and blank the
//! user's screen — and every brightness change is restored by a drop guard that
//! runs even if an assertion panics.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use duja_core::controller::BrightnessController;
use duja_core::model::Feature;
use duja_ddc::{DdcController, Dxva2Transport, SystemClock, enumerate};

/// The round-trip drift, in raw VCP units, tolerated on real hardware.
const TOLERANCE: u16 = 1;

/// Whether the operator opted into real-hardware tests.
fn hw_enabled() -> bool {
    std::env::var("DUJA_HW_TESTS").as_deref() == Ok("1")
}

/// Restores a display's brightness to a captured value on drop — even during a
/// panic unwind — so a failing assertion never leaves the monitor dimmed.
struct BrightnessGuard {
    controller: DdcController<Dxva2Transport, SystemClock>,
    restore_to: u16,
}

impl BrightnessGuard {
    fn controller(&mut self) -> &mut DdcController<Dxva2Transport, SystemClock> {
        &mut self.controller
    }
}

impl Drop for BrightnessGuard {
    fn drop(&mut self) {
        let restore_to = self.restore_to;
        if let Err(err) = self.controller.set(Feature::Brightness, restore_to) {
            eprintln!("warning: failed to restore brightness to {restore_to}: {err}");
        }
    }
}

/// The hint shown when no DDC display is visible: almost always a detached
/// desktop session (dxva2/CCD need the console session that owns the monitor).
const NO_DISPLAY_HINT: &str = "no DDC display visible — the desktop session must \
be connected to the console that owns the monitor (a disconnected/RDP session \
sees only the 'WinDisc' pseudo-display); run `qwinsta` to check";

/// Find the attached MSI MP273QP (id prefix `MSI-30B6`) and open a controller.
fn open_msi() -> DdcController<Dxva2Transport, SystemClock> {
    let displays = enumerate().expect("enumerate must succeed");
    assert!(!displays.is_empty(), "{NO_DISPLAY_HINT}");
    let display = displays
        .into_iter()
        .find(|d| d.id.as_str().starts_with("MSI-30B6"))
        .expect("the MSI MP273QP (id prefix MSI-30B6) must be attached");
    display.into_controller()
}

#[test]
#[ignore = "hardware: requires DUJA_HW_TESTS=1 and the MSI MP273QP attached"]
fn hw_enumerates_msi_monitor() {
    if !hw_enabled() {
        eprintln!("skipping hw_enumerates_msi_monitor: set DUJA_HW_TESTS=1 to run");
        return;
    }
    let displays = enumerate().expect("enumerate must succeed");
    assert!(!displays.is_empty(), "{NO_DISPLAY_HINT}");
    for d in &displays {
        eprintln!(
            "display: id={} name={:?} edid_len={}",
            d.id.as_str(),
            d.name,
            d.edid.len()
        );
    }
    assert!(
        displays
            .iter()
            .any(|d| d.id.as_str().starts_with("MSI-30B6")),
        "no display with the MSI-30B6 id prefix was found"
    );
}

#[test]
#[ignore = "hardware: requires DUJA_HW_TESTS=1 and the MSI MP273QP attached"]
fn hw_contract_suite_real_monitor() {
    if !hw_enabled() {
        eprintln!("skipping hw_contract_suite_real_monitor: set DUJA_HW_TESTS=1 to run");
        return;
    }

    let mut controller = open_msi();

    // Probe is idempotent and reports a real hardware brightness range.
    let caps_a = controller.probe().expect("first probe must succeed");
    let caps_b = controller.probe().expect("second probe must succeed");
    assert_eq!(caps_a, caps_b, "probe must be idempotent");
    assert!(
        caps_a.supports(Feature::Brightness),
        "MSI must report brightness"
    );
    assert!(caps_a.hardware_range, "MSI must have a hardware range");
    eprintln!(
        "caps: features={:?} raw_len={:?}",
        caps_a.features,
        caps_a.raw_capabilities.as_deref().map(str::len)
    );

    // Read initial brightness, timing the read, and arm the restore guard.
    let started = Instant::now();
    let initial = controller
        .get(Feature::Brightness)
        .expect("initial brightness read must succeed");
    eprintln!(
        "brightness read: current={} max={} in {:?}",
        initial.current,
        initial.max,
        started.elapsed()
    );
    assert!(initial.max > 0, "brightness max must be positive");
    assert!(
        initial.current <= initial.max,
        "current must not exceed max"
    );

    let mut guard = BrightnessGuard {
        controller,
        restore_to: initial.current,
    };
    let c = guard.controller();

    // Round-trip a set/get within tolerance (nominal-safe contract case). Pick a
    // gentle in-range target that differs from the current value.
    let target = if initial.current > 20 {
        initial.current.saturating_sub(10)
    } else {
        initial.current.saturating_add(10).min(initial.max)
    };
    c.set(Feature::Brightness, target)
        .expect("in-range brightness set must succeed");
    let after = c
        .get(Feature::Brightness)
        .expect("brightness read after set must succeed");
    let drift = after.current.abs_diff(target);
    eprintln!(
        "round-trip: target={target} read_back={} drift={drift}",
        after.current
    );
    assert!(
        drift <= TOLERANCE,
        "round-trip drift {drift} exceeds tolerance {TOLERANCE}"
    );

    // Out-of-range set must clamp within the reported max (never overshoot).
    c.set(Feature::Brightness, u16::MAX)
        .expect("out-of-range brightness set must clamp, not fail");
    let clamped = c
        .get(Feature::Brightness)
        .expect("brightness read after clamp must succeed");
    assert!(
        clamped.current <= clamped.max,
        "clamped value {} exceeds max {}",
        clamped.current,
        clamped.max
    );

    // `guard` drops here, restoring the original brightness.
}

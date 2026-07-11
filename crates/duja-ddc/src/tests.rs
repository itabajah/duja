//! Cross-platform, deterministic tests for the transport-agnostic controller:
//! the reusable core contract bound to `DdcController<FakeTransport>`, plus
//! focused unit tests for pacing, retry, verify, quirks and fallback.

use std::time::Duration;

use duja_core::controller::{BrightnessController, ControlError};
use duja_core::id::StableDisplayId;
use duja_core::model::Feature;
use duja_core::quirks::{QuirkDb, ResolvedQuirks};
use duja_core::testing::{Scenario, run_controller_contract};

use crate::controller::{DEFAULT_MIN_GAP, DdcController};
use crate::ddcci::{DdcCiTransport, DdcWire, I2cBus};
use crate::fake::{FakeI2cBus, FakeTransport, InjectKind, TestClock};

/// A checksum-valid EDID for an MSI display with product code `0x30B6` and no
/// serial, so its id begins `MSI-30B6` and picks up the embedded MSI quirks.
fn msi_edid() -> Vec<u8> {
    let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    e.push(0x36); // "MSI" high byte
    e.push(0x69); // "MSI" low byte
    e.push(0xB6); // product 0x30B6, little-endian low
    e.push(0x30); // product 0x30B6, little-endian high
    e.resize(127, 0x00);
    let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
    e.push(sum.wrapping_neg());
    e
}

fn msi_id() -> StableDisplayId {
    StableDisplayId::from_edid(&msi_edid()).expect("valid MSI edid")
}

fn msi_quirks() -> ResolvedQuirks {
    QuirkDb::embedded().resolve(&msi_id())
}

/// Build a controller with a fake transport, a virtual clock, and given quirks.
fn controller(
    transport: FakeTransport,
    quirks: ResolvedQuirks,
) -> DdcController<FakeTransport, TestClock> {
    DdcController::with_parts(transport, quirks, TestClock::new())
}

// --- the reusable core contract ------------------------------------------

fn contract_factory(scenario: Scenario) -> DdcController<FakeTransport, TestClock> {
    let transport = match scenario {
        Scenario::Nominal => FakeTransport::nominal(),
        Scenario::Disconnected => FakeTransport::disconnected(),
        // Fail exactly enough read attempts that the *first* controller-level
        // read exhausts its retries and surfaces the error, then recovers.
        Scenario::ErrorThenOk => FakeTransport::nominal().failing(InjectKind::Timeout, 3, 0, 0),
    };
    controller(transport, ResolvedQuirks::default())
}

#[test]
fn satisfies_core_controller_contract() {
    run_controller_contract(contract_factory, 0);
}

// --- the same contract, bound to the macOS DDC/CI transport --------------

/// Build the macOS controller (`DdcController` over `DdcCiTransport`) for a
/// contract scenario, driven by the scriptable [`FakeI2cBus`]. This proves the
/// full mac stack — packet framing, checksum, reply parsing, retry, pacing,
/// verify — satisfies the same core contract as the Windows backend, on every
/// OS in CI, without any Mac hardware.
fn mac_contract_factory(
    scenario: Scenario,
) -> DdcController<DdcCiTransport<FakeI2cBus>, TestClock> {
    let bus = match scenario {
        Scenario::Nominal => FakeI2cBus::nominal(),
        Scenario::Disconnected => FakeI2cBus::disconnected(),
        // Fail exactly enough read attempts that the first controller-level read
        // exhausts its retries and surfaces the error, then recovers.
        Scenario::ErrorThenOk => FakeI2cBus::nominal().failing(InjectKind::Timeout, 3, 0, 0),
    };
    DdcController::with_parts(
        DdcCiTransport::new(bus),
        ResolvedQuirks::default(),
        TestClock::new(),
    )
}

#[test]
fn mac_transport_satisfies_core_controller_contract() {
    run_controller_contract(mac_contract_factory, 0);
}

#[test]
fn mac_transport_round_trips_a_brightness_write() {
    // An end-to-end check through the real codec: set 42, read back 42.
    let mut c = DdcController::with_parts(
        DdcCiTransport::new(FakeI2cBus::nominal()),
        ResolvedQuirks::default(),
        TestClock::new(),
    );
    c.set(Feature::Brightness, 42).unwrap();
    assert_eq!(c.get(Feature::Brightness).unwrap().current, 42);
}

#[test]
fn mac_transport_round_trips_under_intel_framing() {
    // The same logic must hold when the bus reports the Intel wire framing.
    let bus = FakeI2cBus::nominal().with_wire(DdcWire::Intel);
    let mut c = DdcController::with_parts(
        DdcCiTransport::new(bus),
        ResolvedQuirks::default(),
        TestClock::new(),
    );
    c.set(Feature::Brightness, 33).unwrap();
    assert_eq!(c.get(Feature::Brightness).unwrap().current, 33);
    assert_eq!(c.transport().bus().wire(), DdcWire::Intel);
}

#[test]
fn mac_transport_verified_write_detects_mismatch() {
    // A bus that ignores writes leaves the readback stale, so a verified write
    // must surface a backend error after exhausting its attempts.
    let quirks = ResolvedQuirks {
        verify_writes: true,
        ..ResolvedQuirks::default()
    };
    let bus = FakeI2cBus::nominal().ignoring_writes();
    let mut c = DdcController::with_parts(DdcCiTransport::new(bus), quirks, TestClock::new());
    assert!(matches!(
        c.set(Feature::Brightness, 42),
        Err(ControlError::Backend(_))
    ));
}

#[test]
fn mac_transport_probes_capabilities_over_i2c() {
    let mut c = DdcController::with_parts(
        DdcCiTransport::new(FakeI2cBus::nominal()),
        ResolvedQuirks::default(),
        TestClock::new(),
    );
    let caps = c.probe().unwrap();
    assert!(caps.supports(Feature::Brightness));
    assert!(caps.supports(Feature::Contrast));
    assert!(!caps.supports(Feature::InputSource));
    assert_eq!(caps.raw_capabilities.as_deref(), Some("(vcp(10 12))"));
}

// --- pacing --------------------------------------------------------------

#[test]
fn operations_are_paced_by_the_default_min_gap() {
    let mut c = controller(FakeTransport::nominal(), ResolvedQuirks::default());
    // Three successful single-read gets: the first paces without sleeping, each
    // later one sleeps the full gap (virtual time only advances on sleep).
    c.get(Feature::Brightness).unwrap();
    c.get(Feature::Brightness).unwrap();
    c.get(Feature::Brightness).unwrap();
    assert_eq!(c.clock().sleeps, vec![DEFAULT_MIN_GAP, DEFAULT_MIN_GAP]);
    assert_eq!(c.min_gap(), DEFAULT_MIN_GAP);
}

#[test]
fn msi_quirk_tightens_the_pacing_gap() {
    let c = controller(FakeTransport::nominal(), msi_quirks());
    // The embedded MSI-30B6 row sets a 50 ms gap.
    assert_eq!(c.min_gap(), Duration::from_millis(50));
}

#[test]
fn first_operation_does_not_sleep() {
    let mut c = controller(FakeTransport::nominal(), ResolvedQuirks::default());
    c.get(Feature::Brightness).unwrap();
    assert!(c.clock().sleeps.is_empty());
}

// --- retry ---------------------------------------------------------------

#[test]
fn read_retries_then_succeeds() {
    // Fail the first two read attempts, succeed on the third.
    let t = FakeTransport::nominal().failing(InjectKind::Timeout, 2, 0, 0);
    let mut c = controller(t, ResolvedQuirks::default());
    let range = c.get(Feature::Brightness).unwrap();
    assert_eq!(range.current, 50);
    assert_eq!(c.transport().reads.len(), 3);
}

#[test]
fn read_retry_exhaustion_maps_to_timeout() {
    let t = FakeTransport::nominal().failing(InjectKind::Timeout, 99, 0, 0);
    let mut c = controller(t, ResolvedQuirks::default());
    assert!(matches!(
        c.get(Feature::Brightness),
        Err(ControlError::Timeout)
    ));
    assert_eq!(c.transport().reads.len(), 3);
}

#[test]
fn persistent_disconnect_maps_to_disconnected() {
    let t = FakeTransport::nominal().failing(InjectKind::Disconnected, 99, 0, 0);
    let mut c = controller(t, ResolvedQuirks::default());
    assert!(matches!(
        c.get(Feature::Brightness),
        Err(ControlError::Disconnected)
    ));
}

// --- verify-by-readback --------------------------------------------------

#[test]
fn verified_write_succeeds_when_readback_matches() {
    let quirks = ResolvedQuirks {
        verify_writes: true,
        ..ResolvedQuirks::default()
    };
    let mut c = controller(FakeTransport::nominal(), quirks);
    c.set(Feature::Brightness, 42).unwrap();
    assert_eq!(c.get(Feature::Brightness).unwrap().current, 42);
}

#[test]
fn verified_write_retries_then_errors_on_persistent_mismatch() {
    let quirks = ResolvedQuirks {
        verify_writes: true,
        ..ResolvedQuirks::default()
    };
    // Writes are ignored, so every readback stays at the seeded 50 != 42.
    let t = FakeTransport::nominal().ignoring_writes();
    let mut c = controller(t, quirks);
    let err = c.set(Feature::Brightness, 42);
    assert!(matches!(err, Err(ControlError::Backend(_))));
    // Three verify attempts, each issuing one write.
    assert_eq!(c.transport().writes.len(), 3);
}

// --- clamping ------------------------------------------------------------

#[test]
fn out_of_range_set_clamps_to_max() {
    let mut c = controller(FakeTransport::nominal(), ResolvedQuirks::default());
    c.set(Feature::Brightness, u16::MAX).unwrap();
    // The last write must be the clamped value (max 100), not u16::MAX.
    assert_eq!(c.transport().writes.last(), Some(&(0x10u8, 100u16)));
}

// --- capability fallback -------------------------------------------------

#[test]
fn caps_read_failure_falls_back_to_direct_probes() {
    // No capability string: the caps read fails, so the controller probes
    // 0x10/0x12 by direct read instead.
    let t = FakeTransport::nominal().with_caps(None);
    let mut c = controller(t, ResolvedQuirks::default());
    let caps = c.probe().unwrap();
    assert!(caps.supports(Feature::Brightness));
    assert!(caps.supports(Feature::Contrast));
    assert!(caps.hardware_range);
    assert_eq!(caps.raw_capabilities, None);
}

#[test]
fn caps_unreliable_quirk_ignores_the_capability_string() {
    let quirks = ResolvedQuirks {
        caps_unreliable: true,
        ..ResolvedQuirks::default()
    };
    // The caps string advertises input source (0x60), but with caps_unreliable
    // the controller probes 0x10/0x12 only and never trusts 0x60.
    let t = FakeTransport::nominal().with_caps(Some("(vcp(10 12 60))"));
    let mut c = controller(t, quirks);
    let caps = c.probe().unwrap();
    assert!(caps.supports(Feature::Brightness));
    assert!(caps.supports(Feature::Contrast));
    assert!(!caps.supports(Feature::InputSource));
    assert_eq!(caps.raw_capabilities, None);
}

#[test]
fn fallback_omits_a_feature_whose_direct_read_fails() {
    // No caps string and contrast (0x12) absent: only brightness is discovered.
    let t = FakeTransport::nominal().with_caps(None).without_value(0x12);
    let mut c = controller(t, ResolvedQuirks::default());
    let caps = c.probe().unwrap();
    assert!(caps.supports(Feature::Brightness));
    assert!(!caps.supports(Feature::Contrast));
}

// --- quirks: max_brightness, ddc_broken, no_input_switch, input source ---

#[test]
fn max_brightness_quirk_overrides_reported_maximum() {
    let quirks = ResolvedQuirks {
        max_brightness: Some(80),
        ..ResolvedQuirks::default()
    };
    // Hardware reports current 90 / max 100; the quirk caps both to 80.
    let t = FakeTransport::nominal().with_value(0x10, 90, 100);
    let mut c = controller(t, quirks);
    let range = c.get(Feature::Brightness).unwrap();
    assert_eq!(range.max, 80);
    assert_eq!(range.current, 80);
    // A set clamps against the overridden max, not the reported 100.
    c.set(Feature::Brightness, 200).unwrap();
    assert_eq!(c.transport().writes.last(), Some(&(0x10u8, 80u16)));
}

#[test]
fn ddc_broken_quirk_probes_empty_without_any_wire_io() {
    let quirks = ResolvedQuirks {
        ddc_broken: true,
        ..ResolvedQuirks::default()
    };
    let mut c = controller(FakeTransport::nominal(), quirks);
    let caps = c.probe().unwrap();
    assert!(caps.features.is_empty());
    assert!(!caps.hardware_range);
    assert!(c.transport().reads.is_empty());
    assert_eq!(c.transport().caps_calls, 0);
}

#[test]
fn no_input_switch_quirk_drops_the_input_source_feature() {
    let quirks = ResolvedQuirks {
        no_input_switch: true,
        ..ResolvedQuirks::default()
    };
    let t = FakeTransport::nominal().with_caps(Some("(vcp(10 12 60(11120F)))"));
    let mut c = controller(t, quirks);
    let caps = c.probe().unwrap();
    assert!(caps.supports(Feature::Brightness));
    assert!(!caps.supports(Feature::InputSource));
    // The value list is cleared alongside the dropped feature.
    assert!(caps.allowed_inputs.is_empty());
}

#[test]
fn probe_carries_input_value_list_from_caps() {
    // The 0x60 value list from the capability string reaches allowed_inputs.
    let t = FakeTransport::nominal().with_caps(Some("(vcp(10 12 60(11120F)))"));
    let mut c = controller(t, ResolvedQuirks::default());
    let caps = c.probe().unwrap();
    assert!(caps.supports(Feature::InputSource));
    assert_eq!(caps.allowed_inputs, vec![0x11, 0x12, 0x0F]);
}

#[test]
fn probe_intersects_caps_list_with_allowed_quirk() {
    // The caps string offers three inputs; the quirk narrows them to a subset.
    let quirks = ResolvedQuirks {
        input_source_allowed: Some(vec![0x11, 0x0F]),
        ..ResolvedQuirks::default()
    };
    let t = FakeTransport::nominal().with_caps(Some("(vcp(10 12 60(11120F)))"));
    let mut c = controller(t, quirks);
    let caps = c.probe().unwrap();
    assert_eq!(caps.allowed_inputs, vec![0x11, 0x0F]);
}

#[test]
fn input_source_write_is_gated_by_allowed_values() {
    let quirks = ResolvedQuirks {
        input_source_allowed: Some(vec![17, 18, 15]),
        ..ResolvedQuirks::default()
    };
    let t = FakeTransport::nominal().with_value(0x60, 15, 3);
    let mut c = controller(t, quirks);
    // A disallowed value is rejected with a clear backend error, no write.
    assert!(matches!(
        c.set(Feature::InputSource, 99),
        Err(ControlError::Backend(_))
    ));
    assert!(c.transport().writes.is_empty());
    // An allowed value is written through.
    c.set(Feature::InputSource, 17).unwrap();
    assert_eq!(c.transport().writes.last(), Some(&(0x60u8, 17u16)));
}

#[test]
fn probe_populates_raw_capabilities_from_the_string() {
    let mut c = controller(FakeTransport::nominal(), ResolvedQuirks::default());
    let caps = c.probe().unwrap();
    assert_eq!(caps.raw_capabilities.as_deref(), Some("(vcp(10 12))"));
}

#[test]
fn get_on_probed_unsupported_feature_reports_unsupported() {
    let mut c = controller(FakeTransport::nominal(), ResolvedQuirks::default());
    c.probe().unwrap();
    // Input source is not in the nominal caps string.
    assert!(matches!(
        c.get(Feature::InputSource),
        Err(ControlError::Unsupported)
    ));
    assert!(matches!(
        c.set(Feature::InputSource, 15),
        Err(ControlError::Unsupported)
    ));
}

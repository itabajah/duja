//! Live Windows tests for the overlay backend.
//!
//! These run in this session even though no real monitor is attached: overlay
//! windows exist off-screen (in the virtual desktop) and are inspectable with
//! the same Win32 calls a screen-capture or automation tool would use. They
//! prove the *recipe* — window existence, the exact ex-styles, alpha round-trip,
//! click-through, zero-alpha destruction, and a clean threaded shutdown — that
//! the P1 overlay spike verified visually on real hardware.
//!
//! Gamma and HDR live paths need a real display and are **not** exercised here
//! (see the `#[ignore]`d, `DUJA_HW_TESTS`-gated tests); their logic is covered
//! by the pure unit tests in the crate.
#![cfg(windows)]
// RATIONALE: this live test does raw Win32 handle/bit arithmetic and unwraps
// freely; the casts are inherent to the FFI and safe in-bounds here, so the
// pedantic cast lints and the panic-family lints are relaxed for the test only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::time::{Duration, Instant};

use duja_core::dimmer::{DimCommand, Dimmer, DisplayBounds};
use duja_core::id::StableDisplayId;
use duja_dimmer::{WindowsDimmer, plan::quantize_alpha};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongPtrW,
    GetWindowThreadProcessId, HTTRANSPARENT, SendMessageW, WM_NCHITTEST, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};
use windows::core::{PCWSTR, w};

/// The overlay window class this backend registers (kept in sync with `sys`).
const OVERLAY_CLASS: PCWSTR = w!("DujaDimmerOverlay");

/// Whether the operator opted into real-hardware tests.
fn hw_enabled() -> bool {
    std::env::var("DUJA_HW_TESTS").as_deref() == Ok("1")
}

/// A stable id for a synthetic display, keyed by a test-unique serial.
fn synth_id(serial: &str) -> StableDisplayId {
    StableDisplayId::from_parts("DUJ", 0x0001, Some(serial)).unwrap()
}

/// Off-screen synthetic bounds (far in the negative virtual-desktop quadrant so
/// an overlay can never disturb whatever is on the real primary).
fn offscreen(serial: &str, alpha: f32) -> DimCommand {
    DimCommand::new(
        synth_id(serial),
        DisplayBounds::new(-30_000, -30_000, 320, 240),
        alpha,
        None,
    )
}

/// The overlay windows this *process* currently owns (filtered by PID so a
/// parallel test process's overlays are never miscounted).
fn my_overlays() -> Vec<HWND> {
    let me = std::process::id();
    let mut out = Vec::new();
    let mut after: Option<HWND> = None;
    loop {
        // SAFETY: FindWindowExW walks top-level windows of the class; `after`
        // chains the enumeration. Returns Err (or a null handle) at the end.
        let found = unsafe { FindWindowExW(None, after, OVERLAY_CLASS, PCWSTR::null()) };
        let Ok(hwnd) = found else { break };
        if hwnd.0.is_null() {
            break;
        }
        let mut pid = 0u32;
        // SAFETY: `hwnd` is a live window handle from the enumeration.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
        if pid == me {
            out.push(hwnd);
        }
        after = Some(hwnd);
    }
    out
}

/// The extended-window-style bits of `hwnd`.
fn ex_style(hwnd: HWND) -> u32 {
    // SAFETY: `hwnd` is one of our live overlay windows.
    (unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }) as u32
}

/// The layered alpha byte currently set on `hwnd`.
fn layered_alpha(hwnd: HWND) -> u8 {
    let mut alpha = 0u8;
    // SAFETY: `hwnd` is a live layered overlay; we read only the alpha byte.
    unsafe { GetLayeredWindowAttributes(hwnd, None, Some(&raw mut alpha), None) }
        .expect("GetLayeredWindowAttributes on a layered overlay");
    alpha
}

/// The `WM_NCHITTEST` result at an arbitrary point (overlays answer uniformly).
fn hit_test(hwnd: HWND) -> isize {
    // SAFETY: synchronous cross-thread SendMessageW to a live window whose owner
    // thread is pumping messages; the overlay wndproc answers WM_NCHITTEST.
    unsafe { SendMessageW(hwnd, WM_NCHITTEST, Some(WPARAM(0)), Some(LPARAM(0))) }.0
}

#[test]
fn apply_creates_overlay_with_recipe_styles_alpha_and_clickthrough() {
    let mut dimmer = WindowsDimmer::spawn().expect("spawn overlay backend");
    dimmer.apply(&[offscreen("a", 0.5)]).expect("apply");

    let overlays = my_overlays();
    assert_eq!(overlays.len(), 1, "exactly one overlay should exist");
    let hwnd = overlays[0];

    // The exact spike recipe ex-styles.
    let ex = ex_style(hwnd);
    for (bit, name) in [
        (WS_EX_LAYERED.0, "LAYERED"),
        (WS_EX_TRANSPARENT.0, "TRANSPARENT"),
        (WS_EX_NOACTIVATE.0, "NOACTIVATE"),
        (WS_EX_TOOLWINDOW.0, "TOOLWINDOW"),
        (WS_EX_TOPMOST.0, "TOPMOST"),
    ] {
        assert!(
            ex & bit != 0,
            "overlay missing WS_EX_{name} (ex=0x{ex:08X})"
        );
    }

    // Alpha round-trips through the quantizer used by the planner.
    assert_eq!(layered_alpha(hwnd), quantize_alpha(0.5));

    // Click-through: WM_NCHITTEST answers HTTRANSPARENT.
    assert_eq!(hit_test(hwnd), HTTRANSPARENT as isize);

    dimmer.shutdown();
}

#[test]
fn alpha_change_round_trips() {
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer.apply(&[offscreen("a", 0.25)]).expect("apply low");
    let hwnd = my_overlays()[0];
    assert_eq!(layered_alpha(hwnd), quantize_alpha(0.25));

    // Re-apply the same display at a higher alpha: SetAlpha on the same HWND.
    dimmer.apply(&[offscreen("a", 0.8)]).expect("apply high");
    let after = my_overlays();
    assert_eq!(
        after.len(),
        1,
        "alpha change must not create a second window"
    );
    assert_eq!(layered_alpha(after[0]), quantize_alpha(0.8));

    dimmer.shutdown();
}

#[test]
fn zero_alpha_destroys_the_overlay() {
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer.apply(&[offscreen("a", 0.5)]).expect("apply visible");
    assert_eq!(my_overlays().len(), 1);

    dimmer.apply(&[offscreen("a", 0.0)]).expect("apply zero");
    assert_eq!(
        my_overlays().len(),
        0,
        "zero alpha must destroy the overlay"
    );

    dimmer.shutdown();
}

#[test]
fn apply_multiple_then_clear_leaves_none() {
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer
        .apply(&[
            offscreen("a", 0.5),
            offscreen("b", 0.7),
            offscreen("c", 0.3),
        ])
        .expect("apply three");
    assert_eq!(my_overlays().len(), 3);

    dimmer.clear().expect("clear");
    assert_eq!(my_overlays().len(), 0, "clear must remove every overlay");

    dimmer.shutdown();
}

#[test]
fn move_reuses_the_same_window() {
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer.apply(&[offscreen("a", 0.5)]).expect("apply");
    let before = my_overlays();
    assert_eq!(before.len(), 1);
    let original = before[0];

    // Same display, different bounds: MoveResize, not a recreate.
    let moved = DimCommand::new(
        synth_id("a"),
        DisplayBounds::new(-25_000, -25_000, 640, 480),
        0.5,
        None,
    );
    dimmer.apply(&[moved]).expect("apply moved");
    let after = my_overlays();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].0, original.0, "move must reuse the same HWND");

    dimmer.shutdown();
}

#[test]
fn drop_shuts_down_and_removes_overlays() {
    {
        let mut dimmer = WindowsDimmer::spawn().expect("spawn");
        dimmer.apply(&[offscreen("a", 0.5)]).expect("apply");
        assert_eq!(my_overlays().len(), 1);
        // Drop (not explicit shutdown) must tear the overlays down and join.
    }
    assert_eq!(
        my_overlays().len(),
        0,
        "dropping the dimmer must destroy its overlays"
    );
}

#[test]
fn spawn_and_shutdown_without_apply_is_clean() {
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    assert_eq!(my_overlays().len(), 0);
    dimmer.shutdown();
    // Second shutdown is a no-op (idempotent).
    dimmer.shutdown();
}

/// Perf budget (hardware-gated): 100 alpha updates, each under 16 ms.
///
/// Timed end-to-end through the worker thread (channel round-trip +
/// `SetLayeredWindowAttributes`). Gated with the hardware suite per the plan.
#[test]
#[ignore = "perf budget: run in the hardware suite (DUJA_HW_TESTS=1)"]
fn alpha_updates_meet_frame_budget() {
    if !hw_enabled() {
        eprintln!("skipping alpha_updates_meet_frame_budget: set DUJA_HW_TESTS=1 to run");
        return;
    }
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer.apply(&[offscreen("a", 0.5)]).expect("seed overlay");

    let budget = Duration::from_millis(16);
    let mut worst = Duration::ZERO;
    for i in 0..100u32 {
        // Alternate alpha so every apply emits a real SetAlpha op.
        let alpha = if i % 2 == 0 { 0.4 } else { 0.6 };
        let start = Instant::now();
        dimmer
            .apply(&[offscreen("a", alpha)])
            .expect("alpha update");
        let elapsed = start.elapsed();
        worst = worst.max(elapsed);
        assert!(
            elapsed < budget,
            "alpha update {i} took {elapsed:?}, over the 16ms budget"
        );
    }
    eprintln!("worst alpha-update latency: {worst:?}");
    dimmer.shutdown();
}

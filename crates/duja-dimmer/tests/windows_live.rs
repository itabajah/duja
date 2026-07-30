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
//!
//! Windows-only (`#![cfg(windows)]`), so exactly one of the three CI lanes
//! compiles and runs this file.
//!
//! # Attribution: why the overlays are enumerated and serialized the way they are
//!
//! Every count below means "how many overlays does *my* dimmer own", but nothing
//! visible from outside the backend distinguishes one dimmer's overlay windows
//! from another's — the finest grain available is the owning **process**. Two
//! consequences, both handled rather than documented around:
//!
//! - **Between processes**, the class is shared. [`overlay_windows`] takes a
//!   snapshot with `EnumWindows` and keeps only this process's windows. It does
//!   *not* walk `FindWindowExW`'s `hwndChildAfter` chain, which Microsoft
//!   documents as resuming "with the next child window in the Z order": that
//!   cursor is Z-order-relative, so a concurrent overlay of the same class in
//!   another process can make the walk return a window twice **or** truncate
//!   early when the handle held as the cursor is destroyed mid-walk. Both were
//!   reproduced on this tree (see the commit message for the recipe); the first
//!   produces exactly the `left: 2, right: 1` recorded in `docs/debt.md`, the
//!   second an undercount. `EnumWindows` has no cursor to invalidate, which is
//!   why Microsoft recommends it over a `GetWindow`/`FindWindowEx` loop for
//!   precisely these two reasons.
//! - **Within this process**, sibling tests would otherwise be counted as this
//!   test's. Each test therefore takes [`gate`] first, so only one dimmer is ever
//!   alive at a time. The guard is declared before the dimmer so it is released
//!   *after* the dimmer's overlays are gone. This costs nothing under
//!   `cargo nextest run` (one process per test, so the lock is uncontended) and
//!   is what makes a plain `cargo test -p duja-dimmer` — the workflow
//!   `CONTRIBUTING.md` lists first — pass as well.
//!
//! Every assertion here is therefore harness-independent, and a failure in this
//! file is a finding, not a precondition to wave off.
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

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use duja_core::dimmer::{DimCommand, Dimmer, DisplayBounds};
use duja_core::id::StableDisplayId;
use duja_dimmer::{WindowsDimmer, plan::quantize_alpha};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetLayeredWindowAttributes, GetWindowLongPtrW,
    GetWindowThreadProcessId, HTTRANSPARENT, SendMessageW, WM_NCHITTEST, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};
use windows::core::BOOL;

/// The overlay window class this backend registers (kept in sync with `sys`).
const OVERLAY_CLASS: &str = "DujaDimmerOverlay";

/// Serializes the whole file so only one [`WindowsDimmer`] is alive at a time.
///
/// See the [module docs](self): overlay windows carry no per-dimmer marking, so
/// two concurrent dimmers in one process are indistinguishable to
/// [`overlay_windows`]. Taking this first — before the dimmer, so it is released
/// after the dimmer's overlays are destroyed — is what makes the counts exact
/// under a shared-process harness. Uncontended under nextest.
static GATE: Mutex<()> = Mutex::new(());

/// Take the serialization gate for the rest of the current test.
///
/// A panicking test must not poison the gate and cascade into every other test
/// in the file (that would turn one real failure into seven), so a poisoned lock
/// is taken anyway: the data is `()`, there is no invariant to protect.
fn gate() -> MutexGuard<'static, ()> {
    GATE.lock().unwrap_or_else(PoisonError::into_inner)
}

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

/// The overlay windows this *process* currently owns.
///
/// A snapshot enumeration, deliberately not a `FindWindowExW` chain walk — see
/// the [module docs](self) for the two hazards that cursor carries and the
/// evidence that both are reachable here. `EnumWindows` visits each top-level
/// window once with no cursor to invalidate, so this needs neither a dedup nor a
/// cycle guard, and cannot truncate when a window in another process dies
/// mid-enumeration.
fn overlay_windows() -> Vec<HWND> {
    let mut found: Vec<HWND> = Vec::new();
    // SAFETY: `collect_overlay` matches the `WNDENUMPROC` signature and receives
    // `&mut found` as its `lparam` for the duration of the call; `EnumWindows`
    // is synchronous, so the borrow cannot outlive this statement. A failure
    // (the callback stopping early, which ours never does) leaves whatever was
    // collected, which is why the result is ignored.
    let _ = unsafe {
        EnumWindows(
            Some(collect_overlay),
            LPARAM(std::ptr::from_mut(&mut found) as isize),
        )
    };
    found
}

/// `EnumWindows` callback: append `hwnd` to the caller's vector when it is one of
/// *this* process's overlay windows.
unsafe extern "system" fn collect_overlay(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Vec<HWND>` that `overlay_windows` passed to
    // `EnumWindows`, which is alive for the whole synchronous enumeration.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    let mut pid = 0u32;
    // SAFETY: `hwnd` is a live top-level window supplied by the enumeration.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    if pid == std::process::id() && class_name(hwnd) == OVERLAY_CLASS {
        out.push(hwnd);
    }
    // Always continue: a snapshot of every matching window is the point.
    BOOL(1)
}

/// `hwnd`'s window-class name, or an empty string if it cannot be read.
fn class_name(hwnd: HWND) -> String {
    // The class name is bounded by 256 chars plus the NUL, per `RegisterClassW`.
    let mut buf = [0u16; 257];
    // SAFETY: `hwnd` is live; `GetClassNameW` writes at most `buf.len()` UTF-16
    // units including the terminator and returns the count excluding it.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
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
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
    let mut dimmer = WindowsDimmer::spawn().expect("spawn overlay backend");
    dimmer.apply(&[offscreen("a", 0.5)]).expect("apply");

    let overlays = overlay_windows();
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
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer.apply(&[offscreen("a", 0.25)]).expect("apply low");
    let hwnd = overlay_windows()[0];
    assert_eq!(layered_alpha(hwnd), quantize_alpha(0.25));

    // Re-apply the same display at a higher alpha: SetAlpha on the same HWND.
    dimmer.apply(&[offscreen("a", 0.8)]).expect("apply high");
    let after = overlay_windows();
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
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer.apply(&[offscreen("a", 0.5)]).expect("apply visible");
    assert_eq!(overlay_windows().len(), 1);

    dimmer.apply(&[offscreen("a", 0.0)]).expect("apply zero");
    assert_eq!(
        overlay_windows().len(),
        0,
        "zero alpha must destroy the overlay"
    );

    dimmer.shutdown();
}

#[test]
fn apply_multiple_then_clear_leaves_none() {
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer
        .apply(&[
            offscreen("a", 0.5),
            offscreen("b", 0.7),
            offscreen("c", 0.3),
        ])
        .expect("apply three");
    assert_eq!(overlay_windows().len(), 3);

    dimmer.clear().expect("clear");
    assert_eq!(
        overlay_windows().len(),
        0,
        "clear must remove every overlay"
    );

    dimmer.shutdown();
}

#[test]
fn move_reuses_the_same_window() {
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    dimmer.apply(&[offscreen("a", 0.5)]).expect("apply");
    let before = overlay_windows();
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
    let after = overlay_windows();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].0, original.0, "move must reuse the same HWND");

    dimmer.shutdown();
}

#[test]
fn drop_shuts_down_and_removes_overlays() {
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
    {
        let mut dimmer = WindowsDimmer::spawn().expect("spawn");
        dimmer.apply(&[offscreen("a", 0.5)]).expect("apply");
        assert_eq!(overlay_windows().len(), 1);
        // Drop (not explicit shutdown) must tear the overlays down and join.
    }
    assert_eq!(
        overlay_windows().len(),
        0,
        "dropping the dimmer must destroy its overlays"
    );
}

#[test]
fn spawn_and_shutdown_without_apply_is_clean() {
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
    let mut dimmer = WindowsDimmer::spawn().expect("spawn");
    assert_eq!(overlay_windows().len(), 0);
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
    // Declared first so it is released LAST — after the dimmer below is
    // dropped and its overlays are gone. See the module docs.
    let _serial = gate();
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

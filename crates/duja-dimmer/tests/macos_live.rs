//! Live macOS smoke test for the overlay backend.
//!
//! This is a **custom-harness** (`harness = false`) test so its `fn main` runs
//! on the process's true main thread — the default libtest harness runs each
//! test body on a worker thread, where `MainThreadMarker::new()` is `None` and
//! `AppKit` is unusable, and where the main-queue closures this backend dispatches
//! would never drain. On the main thread we can pump a `CFRunLoop`, which drains
//! the libdispatch main queue, so the marshalled overlay ops actually execute.
//!
//! It proves the real recipe end-to-end on a runner that has a window server:
//! an overlay is created on `apply`, an alpha change reuses the same window, and
//! `clear` / `shutdown` remove it — the macOS analogue of the Windows live test.
//! On non-macOS targets `main` is empty and the binary trivially succeeds.

// RATIONALE: a live test asserts freely and reads one Core Foundation extern
// static; the panic-family lints are relaxed for the test binary only (mirrors
// `tests/windows_live.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    // No window server here; the backend is the recording stub. Nothing to drive.
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::Instant;

    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};

    use duja_core::dimmer::{DimCommand, Dimmer, DisplayBounds};
    use duja_core::id::StableDisplayId;
    use duja_dimmer::MacDimmer;

    /// Run the main run loop for `seconds`, draining the dispatch main queue.
    fn pump(seconds: f64) {
        // SAFETY: `kCFRunLoopDefaultMode` is a Core Foundation constant-string
        // static; reading it yields a `'static` mode handle. Running the main
        // run loop on the main thread is always sound.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let _ = CFRunLoop::run_in_mode(mode, seconds, false);
    }

    /// Pump in short slices until `cond` holds or `max_seconds` elapses.
    fn pump_until(mut cond: impl FnMut() -> bool, max_seconds: f64) {
        let start = Instant::now();
        while !cond() && start.elapsed().as_secs_f64() < max_seconds {
            pump(0.05);
        }
    }

    fn cmd(id: &StableDisplayId, alpha: f32) -> DimCommand {
        DimCommand::new(id.clone(), DisplayBounds::new(0, 0, 200, 200), alpha, None)
    }

    pub fn run() {
        let mtm = MainThreadMarker::new()
            .expect("harness = false must run fn main on the true main thread");
        // Ensure an NSApplication context exists for window operations.
        let _app = NSApplication::sharedApplication(mtm);

        let id = StableDisplayId::from_parts("DUJ", 0x0001, Some("mac-smoke")).unwrap();
        let mut dimmer = MacDimmer::new();

        // 1. apply → one overlay is realised on the main queue.
        dimmer.apply(&[cmd(&id, 0.5)]).expect("apply create");
        pump_until(|| dimmer.live_overlay_count() == 1, 5.0);
        assert_eq!(
            dimmer.live_overlay_count(),
            1,
            "apply must realise exactly one overlay window"
        );
        assert_eq!(dimmer.current().len(), 1, "bookkeeping tracks one overlay");

        // 2. alpha change reuses the same window (SetAlpha op, no new window).
        dimmer.apply(&[cmd(&id, 0.8)]).expect("apply alpha");
        pump(0.3);
        assert_eq!(
            dimmer.live_overlay_count(),
            1,
            "an alpha change must not create a second window"
        );

        // 3. clear removes every overlay.
        dimmer.clear().expect("clear");
        pump_until(|| dimmer.live_overlay_count() == 0, 5.0);
        assert_eq!(
            dimmer.live_overlay_count(),
            0,
            "clear must remove the overlay"
        );

        // 4. re-apply, then drop-safe shutdown leaves nothing on screen.
        dimmer.apply(&[cmd(&id, 0.4)]).expect("re-apply");
        pump_until(|| dimmer.live_overlay_count() == 1, 5.0);
        assert_eq!(dimmer.live_overlay_count(), 1);
        dimmer.shutdown();
        pump_until(|| dimmer.live_overlay_count() == 0, 5.0);
        assert_eq!(
            dimmer.live_overlay_count(),
            0,
            "shutdown must tear the overlay down"
        );

        // Idempotent second shutdown must not panic.
        dimmer.shutdown();
        pump(0.1);

        eprintln!("macos_live: overlay create/alpha/remove/shutdown all verified");
    }
}

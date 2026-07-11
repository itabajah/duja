//! The macOS overlay backend.
//!
//! [`MacDimmer`] cannot own a worker thread the way the Windows backend does:
//! `AppKit` windows may only be created or mutated on the **main thread**, which
//! in `duja-app` runs Slint's (winit's) `NSApplication` loop. So [`apply`] runs
//! the pure [`plan`](crate::plan) diff on the *calling* thread, then marshals
//! the resulting [`OverlayOp`](crate::plan::OverlayOp)s onto the **main dispatch
//! queue** (`dispatch_async`). The overlay `NSWindow`s live in a main-thread
//! [`thread_local`] store keyed by dimmer instance, so nothing that is `!Send`
//! ever crosses a thread — the marshalled closure captures only plain data.
//!
//! # Requires a running main run loop
//!
//! `dispatch_async` to the main queue only drains while an `NSApplication` (or a
//! bare `CFRunLoop`) is running on the main thread. That is always true inside
//! `duja-app`. If no main loop ever runs after an [`apply`], the overlay ops
//! never execute (and the windows never appear) — the wave-2 assembly relies on
//! this contract, and the live smoke test pumps a loop to satisfy it.
//!
//! # Observable contract vs Windows (a deliberate difference)
//!
//! The Windows `apply` **blocks** on the worker's reply, so overlays are on
//! screen (and any OS error is returned) before it returns. `MacDimmer` cannot
//! block: `apply` may be called *from the main thread itself* (a UI event
//! handler), where a synchronous `dispatch_sync` to the main queue would
//! deadlock. So `apply`/`clear` do the diff and update bookkeeping
//! **synchronously** — a poisoned-state error surfaces inline as
//! [`DimmerError::Backend`] — then enqueue the window ops on the main queue and
//! **return immediately**. The ops realise on the next main-loop turn, and
//! errors from the `AppKit` calls there cannot be returned to the caller (they
//! are non-fatal by construction — creating or hiding a borderless window does
//! not fail in practice). Ordering is preserved: `&mut self` serialises
//! `apply`s and the main queue is FIFO.
//!
//! # Security invariant
//!
//! Every overlay sets `ignoresMouseEvents = true` (see [`sys`]); overlays never
//! intercept input. Fullscreen-exclusive apps and the OS secure/login screens
//! are documented known-limits (an overlay cannot cover them).

// RATIONALE: the backend's public vocabulary (`MacDimmer`) namespaces the crate
// concept; the qualified names read best at call sites.
#![allow(clippy::module_name_repetitions)]

mod edr;
mod gamma;
mod sys;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::NSWindow;

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};
use duja_core::id::StableDisplayId;

use crate::plan::{OverlayEntry, OverlayOp, apply_ops, plan_transition};

pub use edr::{GammaSupport, display_supports_gamma, gamma_support_from_hdr, is_hdr_active};
pub use gamma::{
    GammaDisplay, RestoreReport, enumerate_gamma_displays, restore_all, restore_identity, set_gamma,
};

/// Source of unique per-instance ids so several `MacDimmer`s never share the
/// one main-thread window store.
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The overlay windows, per dimmer instance. Lives only on the main thread —
    /// every access is inside a main-queue closure (or an on-main call) that has
    /// proven the thread with a [`MainThreadMarker`]. Holding `!Send`
    /// `Retained<NSWindow>`s here is sound precisely because they never leave it.
    static OVERLAY_STORE: RefCell<HashMap<u64, InstanceWindows>> =
        RefCell::new(HashMap::new());
}

/// One dimmer instance's live overlay windows, keyed by display id.
#[derive(Default)]
struct InstanceWindows {
    windows: HashMap<StableDisplayId, Retained<NSWindow>>,
}

/// The macOS software-dimming backend: per-display click-through overlays
/// marshalled onto the main queue.
///
/// Construct with [`new`](Self::new) (or [`spawn`](Self::spawn) for surface
/// parity with the Windows backend). [`Drop`] enqueues teardown of this
/// instance's overlays on the main queue.
#[derive(Debug)]
pub struct MacDimmer {
    /// Unique key into the main-thread [`OVERLAY_STORE`].
    instance: u64,
    /// The overlays this backend believes are on screen (the `plan` bookkeeping);
    /// updated synchronously in [`apply`]/[`clear`], mirrors the stub/Windows
    /// backends. Exclusive via `&mut self`, so no lock is needed.
    current: Vec<OverlayEntry>,
}

impl MacDimmer {
    /// Construct a dimmer. Cannot fail: no thread or window is created here —
    /// overlays are realised lazily on the main queue once a run loop drains it.
    #[must_use]
    pub fn new() -> Self {
        MacDimmer {
            instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            current: Vec::new(),
        }
    }

    /// Construct a dimmer, mirroring the Windows `WindowsDimmer::spawn` surface.
    /// Always `Ok` — there is nothing to fail on macOS.
    ///
    /// # Errors
    /// Never returns `Err`; the `Result` exists only for API parity.
    pub fn spawn() -> Result<Self, DimmerError> {
        Ok(Self::new())
    }

    /// The overlays this backend believes are on screen after the last apply.
    #[must_use]
    pub fn current(&self) -> &[OverlayEntry] {
        &self.current
    }

    /// The number of overlay `NSWindow`s actually realised for this instance in
    /// the main-thread store. **Main-thread only** — returns `0` from any other
    /// thread (the store is thread-local to the main thread). For the live smoke
    /// test; hidden from the public docs.
    #[doc(hidden)]
    #[must_use]
    pub fn live_overlay_count(&self) -> usize {
        if MainThreadMarker::new().is_none() {
            return 0;
        }
        OVERLAY_STORE.with_borrow(|store| store.get(&self.instance).map_or(0, |i| i.windows.len()))
    }

    /// Enqueue teardown of this instance's overlays on the main queue.
    /// Idempotent — a second call finds an empty store entry.
    pub fn shutdown(&mut self) {
        self.current.clear();
        let instance = self.instance;
        dispatch_main(move || remove_instance(instance));
    }
}

impl Default for MacDimmer {
    fn default() -> Self {
        Self::new()
    }
}

impl Dimmer for MacDimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        let ops = plan_transition(&self.current, commands);
        if ops.is_empty() {
            return Ok(());
        }
        self.current = apply_ops(&self.current, &ops);
        let instance = self.instance;
        dispatch_main(move || exec_ops_on_main(instance, ops));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        self.current.clear();
        let instance = self.instance;
        dispatch_main(move || remove_instance(instance));
        Ok(())
    }
}

impl Drop for MacDimmer {
    fn drop(&mut self) {
        let instance = self.instance;
        dispatch_main(move || remove_instance(instance));
    }
}

/// Enqueue `work` on the main dispatch queue. Non-blocking, FIFO, and safe to
/// call from any thread — including the main thread, where it simply runs on the
/// next run-loop turn (a synchronous dispatch would deadlock instead).
fn dispatch_main<F: FnOnce() + Send + 'static>(work: F) {
    DispatchQueue::main().exec_async(work);
}

/// Execute a diffed op list against instance `instance`'s overlays. Runs on the
/// main thread (via the main queue), so it can create and mutate `NSWindow`s.
fn exec_ops_on_main(instance: u64, ops: Vec<OverlayOp>) {
    // Only ever invoked from the main queue; if somehow not on the main thread,
    // skip rather than risk an AppKit call off-main.
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let primary_height = sys::primary_display_height_points();
    OVERLAY_STORE.with_borrow_mut(|store| {
        let inst = store.entry(instance).or_default();
        for op in ops {
            match op {
                OverlayOp::Create { id, bounds, alpha } => {
                    let window = sys::create_overlay(mtm, bounds, alpha, primary_height);
                    inst.windows.insert(id, window);
                }
                OverlayOp::MoveResize { id, bounds } => {
                    if let Some(window) = inst.windows.get(&id) {
                        sys::move_overlay(window, bounds, primary_height);
                    }
                }
                OverlayOp::SetAlpha { id, alpha } => {
                    if let Some(window) = inst.windows.get(&id) {
                        sys::set_alpha(window, alpha);
                    }
                }
                OverlayOp::Destroy { id } => {
                    if let Some(window) = inst.windows.remove(&id) {
                        sys::destroy_overlay(&window);
                    }
                }
            }
        }
    });
}

/// Remove instance `instance` entirely: hide and drop every overlay it owns.
/// Runs on the main thread.
fn remove_instance(instance: u64) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    OVERLAY_STORE.with_borrow_mut(|store| {
        if let Some(inst) = store.remove(&instance) {
            for window in inst.windows.values() {
                sys::destroy_overlay(window);
            }
        }
    });
}

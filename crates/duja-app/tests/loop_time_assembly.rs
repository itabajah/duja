//! Pins the **mechanism** the tray's loop-time assembly rides on: a
//! zero-duration [`slint::Timer::single_shot`] queued *before*
//! [`slint::run_event_loop_until_quit`] fires from **inside** the running event
//! loop, exactly once, and leaves no timer behind.
//!
//! # Why this is a test and not just a comment
//!
//! `bin_support::tray::run` builds the tray icon and the global-hotkey manager
//! inside such a closure because both crates require a running main-thread loop
//! on macOS (`tray-icon` names winit's `StartCause::Init` as the earliest legal
//! moment to create a status item). The argument that the closure really runs
//! in-loop is a *by-construction* one about two pinned third-party versions:
//! `slint 1.17.1` drains timers only from
//! `i_slint_core::platform::update_timers_and_animations`, and
//! `i-slint-backend-winit 1.17.1` calls that from its `ApplicationHandler::new_events`
//! hook. Both facts live in dependency source, and this repo lets dependabot
//! merge `cargo-minor-patch` group bumps unattended. A Slint minor that relocated
//! that call would move the tray build back outside the loop with an otherwise
//! fully green suite — and the consequence would surface on macOS, where it is
//! fatal, not on Windows, where it is invisible. So the mechanism is asserted
//! against the real Slint/winit stack instead of being asserted in prose.
//!
//! # How "it ran inside the loop" is proven
//!
//! Not by observation — by [`slint::quit_event_loop`]'s generation stamping. The
//! winit backend stamps its `Exit` event with the current event-loop generation
//! (`i-slint-backend-winit` `lib.rs`, `Proxy::quit_event_loop`), `Backend::run_event_loop`
//! bumps that generation immediately *before* running the loop, and
//! `EventLoopState::user_event` silently discards an `Exit` whose generation does
//! not match. So a quit issued *before* the loop starts is dropped and the loop
//! runs forever. The closure below does nothing but count itself and quit;
//! therefore **if `run_event_loop_until_quit` returns at all, the closure
//! provably ran with the loop already running.** The failure mode of the property
//! this file exists to protect is a hang, which the repo's nextest
//! `slow-timeout`/`terminate-after` guard (`.config/nextest.toml`, added by `#84`
//! for exactly this class of bug) reports as a named failing test.
//!
//! # One loop-driving test per binary
//!
//! Slint initialises its platform per-thread, so a second test that instantiates
//! a window in this same binary fails with "The Slint platform was initialized in
//! another thread" when the harness runs them on different threads. Do not add a
//! sibling `#[test]` here — give it its own file (its own test binary) if one is
//! ever needed.
//!
//! One was: `tests/loop_running_token.rs`, which is that rule being followed
//! rather than a duplicate of this file. It drives the loop through
//! `bin_support::tray::loop_running` — duja's own wrapper around the timer — and
//! asserts the property this file cannot, that the closure is **queued** rather
//! than called. What is pinned here is the timer itself, including the
//! leaves-nothing-scheduled half that the other file's watchdog makes
//! unassertable.
//!
//! Windows-only - though **not** because `tray.rs` is, which is what this said
//! until `#167`. That module has been un-gated since P7 wave 5 and builds on all
//! three lanes; what cannot run off Windows is a *test that drives an event
//! loop*, for the reasons `loop_running_token.rs` gives. The mechanism is
//! platform-independent by design (that is the point of not `cfg`-splitting the
//! ordering), so verifying it on the shipped platform verifies it for the macOS
//! port that will reuse it.
#![cfg(windows)]
// RATIONALE: integration tests are a separate crate and do not inherit the
// library's `cfg(test)` lint allows. This test uses expect for brevity; it never
// ships.
#![allow(clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use duja_ui::{FlyoutShell, FlyoutVm};

/// A zero-duration single-shot queued before the loop runs *inside* it, once,
/// and schedules nothing further.
///
/// Three assertions, one per property `tray.rs` relies on:
///
/// 1. **It ran, and it ran in-loop.** `run_event_loop_until_quit` returning is
///    itself the assertion (see the module doc on generation stamping); `fires`
///    then confirms the closure — not something else — is what ended the loop.
/// 2. **Exactly once.** A repeated timer here would re-run the whole tray
///    assembly on every wakeup.
/// 3. **Nothing left scheduled.** `duration_until_next_timer_update() == None`
///    means the loop returns to `ControlFlow::Wait` with no periodic wakeup,
///    which is the ADR-0001 / P1 zero-idle-wakeup budget the tray is held to.
///
/// A window is created first because that is what initialises the Slint platform
/// (and therefore the winit backend) — the same order `run` uses, where the
/// flyout is built during pre-loop acquisition. It is never shown.
#[test]
fn a_zero_duration_single_shot_runs_once_inside_the_running_event_loop() {
    let vm = Rc::new(RefCell::new(FlyoutVm::new()));
    let _shell = FlyoutShell::new(vm).expect("the flyout window must be creatable");

    let fires = Rc::new(Cell::new(0_u32));

    slint::Timer::single_shot(Duration::ZERO, {
        let fires = Rc::clone(&fires);
        move || {
            fires.set(fires.get() + 1);
            // Stamped with the *current* generation. Had this closure run before
            // the loop started, the generation would already be stale and the
            // quit silently dropped — the loop would never return and this test
            // would hang rather than pass.
            let _ = slint::quit_event_loop();
        }
    });

    slint::run_event_loop_until_quit().expect("the event loop must run and then quit");

    assert_eq!(
        fires.get(),
        1,
        "the queued closure must fire exactly once, from inside the running loop"
    );
    assert_eq!(
        slint::platform::duration_until_next_timer_update(),
        None,
        "a SingleShot timer must leave no entry behind, so the loop can go back to \
         ControlFlow::Wait with zero periodic wakeups"
    );
}

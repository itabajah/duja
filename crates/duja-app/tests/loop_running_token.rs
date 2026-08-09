//! Pins the two runtime properties [`LoopRunning`] cannot state as a type: that
//! `when_loop_running` **queues** its closure rather than calling it, and that the
//! closure then runs from inside the event loop.
//!
//! # Why the type is not enough on its own
//!
//! `loop_running.rs` makes the witness unforgeable, so a `&LoopRunning` provably
//! came from inside `when_loop_running`'s callback. What no type can say is where
//! *that callback* runs, because the answer lives in two pinned dependency
//! versions: a Slint timer is drained only from
//! `i_slint_core::platform::update_timers_and_animations`, which
//! `i-slint-backend-winit` calls from its `ApplicationHandler::new_events` hook.
//! Dependabot merges `cargo-minor-patch` bumps here unattended. A bump that
//! relocated that drain would leave the whole module compiling and its witness
//! quietly false — which is precisely the failure this repo keeps meeting: a
//! guarantee that reads as checked and is not.
//!
//! So the guarantee is split. The compiler owns "you cannot build a tray without a
//! witness"; this file owns "a witness means what it says".
//!
//! # The module is pulled in through `#[path]`
//!
//! `bin_support` belongs to the `duja` **binary**, so an integration test cannot
//! `use` it. `loop_running.rs` is deliberately free of every other `bin_support`
//! import — it depends on `slint` and nothing else — which is what lets it be
//! compiled a second time into this test binary. That is the same technique the
//! repo uses to cross-check a single app module against another target, and the
//! constraint it puts on the module is worth keeping: the first `use super::…`
//! or `super::`-rooted path in `loop_running.rs`'s **code** stops this file
//! compiling, and the mechanism goes back to being unpinned.
//!
//! Its *doc comments* do link outward (`super::run()`,
//! `super::wiring::build_tray`), and those resolve in the binary and not here.
//! Harmless, and stated so nobody reads the paragraph above as false on sight:
//! `cargo doc` does not document test targets, so the second copy's links are
//! never checked by the gate that would object to them.
//!
//! # One loop-driving test per binary
//!
//! Slint initialises its platform per thread, so a second test that instantiates a
//! window in this same binary fails with "The Slint platform was initialized in
//! another thread" when the harness runs the two on different threads. This file
//! holds exactly one `#[test]` for that reason, as does `loop_time_assembly.rs`.
//! Do not add a sibling — give it its own file.
//!
//! Windows-only, like `loop_time_assembly.rs` - but **not** because the tray is.
//! `bin_support::tray` has been un-gated since P7 wave 5, and this change grows a
//! parameter on its Linux arms too. The gate here is the event loop: a CI ubuntu
//! runner has no X server, and a macOS loop must own the process's main thread,
//! so neither can host this. The **type** half compiles and is
//! enforced on all three lanes; it is only the mechanism check that is pinned on
//! the one platform anybody runs. That is sound for the same reason the ordering
//! is not `cfg`-split: Windows exercises the exact sequence macOS depends on.
#![cfg(windows)]
// RATIONALE: integration tests are a separate crate and do not inherit the
// binary's `cfg(test)` lint allows. This test uses expect for brevity; it never
// ships.
#![allow(clippy::expect_used)]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use duja_ui::{FlyoutShell, FlyoutVm};

// The module under test, compiled a second time into this binary. See the header.
#[path = "../src/bin_support/tray/loop_running.rs"]
mod loop_running;

/// How long the watchdog gives the loop before concluding that the queued closure
/// did not stop it.
///
/// Generous on purpose. It is not a latency budget and nothing measures it: on a
/// correct run it never fires at all, because the closure quits the loop on the
/// first pass. Its only job is to bound the *failing* run, and a shared CI runner
/// under unknown load is exactly where a tight bound would turn a real red into a
/// flaky one.
const WATCHDOG: Duration = Duration::from_millis(500);

/// `when_loop_running` defers its closure to the loop, and the loop is what runs
/// it.
///
/// Three assertions, in the order they can fail:
///
/// 1. **Not called yet.** Checked between queueing and starting the loop, so a
///    `when_loop_running` that invoked its argument directly fails here, on a
///    plain equality, before any loop exists. This is the assertion that
///    corresponds to the historical regression: work that was supposed to happen
///    at loop time happening before it.
/// 2. **The watchdog stayed asleep.** `slint::quit_event_loop` stamps its request
///    with the current event-loop generation and `EventLoopState::user_event`
///    discards one whose generation is stale, so a quit issued before the loop
///    starts is dropped and the loop runs forever. The watchdog turns that hang
///    into a named assertion. Being honest about its reach: it does not detect
///    *which* pre-loop route was taken, only that the closure's own quit did not
///    take effect. `loop_time_assembly.rs` reaches the same property through the
///    hang itself and the nextest `terminate-after` guard; naming it is the only
///    thing this adds.
/// 3. **Exactly once.** A repeated timer here would re-run the entire tray
///    assembly on every wakeup.
///
/// There is deliberately **no** "nothing left scheduled" assertion, which
/// `loop_time_assembly.rs` does carry: the watchdog is still armed when this loop
/// exits, so `duration_until_next_timer_update()` is legitimately `Some` here. The
/// property is real and is pinned there, on a test that queues nothing else.
///
/// The flyout window is built first because that is what initialises the Slint
/// platform, and because it is the order `tray::run` uses — both windows are
/// acquired pre-loop, and only then is the assembly queued.
#[test]
fn the_queued_closure_is_deferred_to_the_loop_and_runs_there_once() {
    let vm = Rc::new(std::cell::RefCell::new(FlyoutVm::new()));
    let _shell = FlyoutShell::new(vm).expect("the flyout window must be creatable");

    let fires = Rc::new(Cell::new(0_u32));
    let watchdog_fired = Rc::new(Cell::new(false));

    loop_running::when_loop_running({
        let fires = Rc::clone(&fires);
        move |_running| {
            fires.set(fires.get().saturating_add(1));
            // Stamped with the *current* generation. Had this run before the loop
            // started, the stamp would already be stale, the quit would be
            // silently discarded, and the watchdog below would be what ends the
            // loop instead.
            let _ = slint::quit_event_loop();
        }
    });

    assert_eq!(
        fires.get(),
        0,
        "when_loop_running must queue its closure, not call it: the tray was \
         being built before the event loop existed"
    );

    slint::Timer::single_shot(WATCHDOG, {
        let watchdog_fired = Rc::clone(&watchdog_fired);
        move || {
            watchdog_fired.set(true);
            let _ = slint::quit_event_loop();
        }
    });

    slint::run_event_loop_until_quit().expect("the event loop must run and then quit");

    assert!(
        !watchdog_fired.get(),
        "the watchdog ended the loop, so the queued closure's quit did not take \
         effect - it was discarded as stale, issued late, or never issued. Which \
         of those is not something this assertion can tell you"
    );
    assert_eq!(
        fires.get(),
        1,
        "the queued closure must fire exactly once, from inside the running loop"
    );
}

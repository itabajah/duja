//! The witness that the Slint event loop is running, and the only way to get one.
//!
//! # The gap this closes
//!
//! `tray-icon` and `global-hotkey` both require a *running* main-thread event
//! loop on macOS, so [`super::run()`] builds neither in its pre-loop phase: both
//! happen inside a closure queued onto the loop. Nothing enforced that. Moving
//! those two calls back to where they sat before the event-loop-first
//! restructure — the exact regression that restructure exists to prevent — left
//! the whole suite green. Measured rather than argued: with the defect restored
//! at its historical site, **377 `duja-app` tests pass**, `tests/loop_time_assembly.rs`
//! among them, because that file pins the *mechanism* and never the wiring to it.
//! Windows tolerates the old ordering; only macOS does not, and no macOS has ever
//! run this.
//!
//! [`LoopRunning`] closes it by making the ordering a **type**. Both
//! [`build_tray`](super::wiring::build_tray) and
//! [`init_hotkeys`](super::wiring::init_hotkeys) take a `&LoopRunning`; the only
//! value of that type a production path can reach is the one
//! [`when_loop_running`] mints inside its own timer callback; and the constructor
//! is private to this module. So the re-inlining above stops being a green test
//! run and becomes a compile error, at the moment somebody writes it rather than
//! on a machine this project does not own.
//!
//! # What the type proves, and what it does not
//!
//! It proves *provenance*: a `&LoopRunning` can only have come from inside
//! [`when_loop_running`]'s callback. It does not prove that the callback runs
//! in-loop — that rests on the mechanism below, which is a property of two pinned
//! dependency versions rather than of the type. If a Slint bump relocated the
//! timer drain, this module would keep compiling while its witness quietly became
//! false. That is why the mechanism is asserted against the real Slint/winit
//! stack by `tests/loop_running_token.rs` and `tests/loop_time_assembly.rs`
//! instead of being left to this paragraph.
//!
//! # The mechanism, and why this one
//!
//! A zero-duration single-shot Slint timer — deliberately **not**
//! [`slint::invoke_from_event_loop`] and **not**
//! `i_slint_backend_winit::Backend::builder().with_custom_application_handler`.
//! Verified in the pinned dependency sources:
//!
//! - **It can only fire from inside the running loop.** A Slint timer is drained
//!   only by `i_slint_core::platform::update_timers_and_animations`, and
//!   `i-slint-backend-winit` calls that from exactly two places:
//!   `ApplicationHandler::new_events` (`event_loop.rs`) and the Apple display-link
//!   callback (`frame_throttle/apple_display_link.rs`, which only exists once a
//!   window is rendering). Both are inside the loop, so the closure fires at
//!   `StartCause::Init` on the loop's first pass — exactly the point `tray-icon`
//!   documents as the earliest legal moment to create a macOS status item.
//!   (`i_slint_core::platform::set_platform` also calls it, but that already ran
//!   when the flyout window was created, before this timer exists.)
//! - **It does not need `Send`.** `Timer::single_shot` takes `FnOnce() + 'static`;
//!   `invoke_from_event_loop` additionally requires `Send`, which `FlyoutShell`,
//!   `SettingsShell`, `Rc<RefCell<…Vm>>` and `gamma::GammaBackend` (it owns a
//!   bare `Box<dyn FnMut>`) are not. Using it would mean pushing those
//!   main-thread-only values across a `Send` bound via a second thread-local or an
//!   `unsafe impl Send`.
//! - **The user-event ordering question does not arise.** Winit *does* deliver
//!   queued user events strictly after `StartCause::Init` on both platforms (on
//!   macOS they are drained only in the `BeforeWaiting` observer, gated on
//!   `is_running`, which `applicationDidFinishLaunching:` sets immediately before
//!   dispatching Init + Resumed), so `invoke_from_event_loop` would also have been
//!   late enough. The timer is preferred because it does not *depend* on that
//!   answer: it hangs off the backend's own `new_events` hook rather than on
//!   user-event delivery order.
//! - **A custom application handler costs too much.** Its
//!   `new_events(_, StartCause::Init)` hook is the documented "loop is running
//!   now" callback, but taking over backend construction means calling
//!   `slint::platform::set_platform` by hand and re-asserting the software
//!   renderer, which ADR-0009 makes load-bearing for both the RAM and
//!   binary-size budgets.
//! - **It schedules nothing further.** A `SingleShot` timer is removed from the
//!   timer list once its callback returns (`TimerList::maybe_activate_timers`), so
//!   `duration_until_next_timer_update` is `None` again and the loop is back to
//!   `ControlFlow::Wait` with zero periodic wakeups (ADR-0001).
//!
//! The first and last points rest on two *pinned dependency versions*, and
//! dependabot merges `cargo-minor-patch` bumps here unattended, so they are not
//! left as prose: `tests/loop_time_assembly.rs` asserts them against the real
//! Slint/winit stack. If a Slint bump relocates the timer drain, that test fails
//! on Windows instead of the tray quietly moving back outside the loop and the
//! consequence surfacing on macOS, where it is fatal.

use std::time::Duration;

/// Proof that the Slint event loop is running.
///
/// Carried by reference into every call the OS only permits from inside a running
/// main-thread loop. The field is private and [`LoopRunning::mint`] is private to
/// this module, so the only value a **safe** production path can obtain is the one
/// [`when_loop_running`] makes inside its timer callback. Not "the only value":
/// `bin_support` lives in the binary, which drops `forbid(unsafe_code)` for the
/// toast FFI, so a `transmute` from `()` would mint one. That is not a path a
/// refactor reaches by accident, and `undocumented_unsafe_blocks = "deny"` makes
/// it loud, which is the whole distinction being drawn.
///
/// Zero-sized: it costs nothing at runtime and exists entirely for the compiler.
/// Deliberately **not** `Clone`, `Copy` or `'static`-storable by value from
/// outside this module — a witness that outlived the callback would be a witness
/// to nothing.
pub(super) struct LoopRunning(());

impl LoopRunning {
    /// The one mint. Private to this module by design: see the type's docs.
    fn mint() -> Self {
        LoopRunning(())
    }

    /// A witness for a test that is deliberately not driving an event loop.
    ///
    /// **It asserts nothing.** The name is `assumed` rather than `for_test`
    /// because that is the whole content of it: the caller is stating that the
    /// loop question is not what their test is about, and taking a token on that
    /// basis. A test that uses this has not shown anything about ordering, and a
    /// test that wants to show something about ordering must drive a real loop —
    /// `tests/loop_running_token.rs` is the worked example.
    ///
    /// It exists for exactly one caller: `wiring`'s `#[ignore]`d D-102
    /// experiment, which measures whether `build_tray` succeeds in a *test
    /// process* and whose recorded answer would change meaning if a running loop
    /// were added underneath it. Production code cannot reach this — `run` is not
    /// `cfg(test)` — which is the property the type is for.
    // RATIONALE: `cfg(test)` is set for integration-test crates too, and
    // `tests/loop_running_token.rs` pulls this module in through `#[path]` to
    // drive it against the real stack. That build has no D-102 experiment in it,
    // so the item is genuinely unused there and would otherwise fail `-D warnings`.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn assumed_for_test() -> Self {
        LoopRunning(())
    }
}

/// Run `f` as the first work the (not yet running) event loop does, handing it
/// the witness that the loop is running.
///
/// The callback is **not** invoked before this function returns; it is queued.
/// That deferral is the property the whole module rests on, and
/// `tests/loop_running_token.rs` asserts it directly rather than inferring it
/// from the loop terminating.
pub(super) fn when_loop_running(f: impl FnOnce(&LoopRunning) + 'static) {
    slint::Timer::single_shot(Duration::ZERO, move || {
        // The one place in the binary where a witness comes into existence, and
        // it is inside the callback rather than around it on purpose: the value
        // must not outlive the moment it describes.
        let running = LoopRunning::mint();
        f(&running);
    });
}

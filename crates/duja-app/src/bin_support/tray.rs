//! The real tray application: tray icon + Slint flyout cohabiting on the
//! Windows main thread, wired to the engine, dimmer, config and state.
//!
//! # One thread, zero idle wakeups (the P1 `spike/eventloop` recipe)
//!
//! `tray-icon` creates its window on the thread that builds it, so its `WM_*`
//! messages land in the main-thread queue that Slint's winit backend already
//! pumps — no second pump, no polling timer. Foreign event handlers
//! (tray/menu) hop onto the Slint loop via
//! [`slint::invoke_from_event_loop`], which wakes the loop only when a real
//! event fires. [`slint::run_event_loop_until_quit`] keeps the process alive
//! while the flyout is hidden.
//!
//! # Two-phase startup (the event loop starts first)
//!
//! [`run()`] is deliberately split in two:
//!
//! 1. **Pre-loop resource acquisition** — paths, guards, crash recovery, config,
//!    the HDR verdict, both Slint windows, the engine/pump/dimmer and the gamma
//!    channel. Each of these is safe to create before the loop runs, and the two
//!    fallible windows can therefore still `?` straight out of `run`.
//! 2. **Loop-time assembly** — the tray icon, the global-hotkey manager, the
//!    [`AppState`] they belong to, every event-source registration, and the IPC
//!    server. This runs inside [`assemble_with_loop_running`], queued as a
//!    zero-duration [`slint::Timer::single_shot`] so it executes as the first
//!    work the event loop does.
//!
//! Both `tray-icon` and `global-hotkey` require a *running* main-thread event
//! loop on macOS (`tray-icon` names winit's `StartCause::Init` as the earliest
//! legal moment to create a status item), and a Slint timer can only fire from
//! `i_slint_core::platform::update_timers_and_animations`, which the winit
//! backend calls from its `new_events` hook — i.e. from inside the loop, at
//! `StartCause::Init` on the first pass. That mechanism is pinned against the
//! real Slint/winit stack by `tests/loop_time_assembly.rs`, not merely asserted
//! here. The ordering is **not** `cfg`-split: Windows, the shipped platform,
//! exercises exactly the sequence macOS depends on.
//!
//! **Nor is the split left to this paragraph.** [`build_tray`] and
//! [`init_hotkeys`] each take a [`loop_running::LoopRunning`], a witness the
//! queued callback is the only thing that can mint, so moving either into phase 1
//! is a compile error rather than a green test run — which is what it used to be,
//! measured across 377 tests. See [`loop_running`] for what that does and does not
//! prove.
//!
//! # Continuum ownership
//!
//! The app owns each display's *user* level (persisted in the state file). A
//! slider change maps through the continuum into one declarative batch: the
//! hardware target (pinned at the floor below it) goes to the engine via
//! `SetUserLevel`; the overlay-alpha channel goes to the
//! [`Dimmer`](duja_core::dimmer::Dimmer); and the opt-in gamma channel goes to
//! [`gamma::GammaBackend`], which owns the persistent-ramp crash marker. The
//! engine is kept dimmer-agnostic — the notification loop here drives the
//! dimmer and the gamma channel.
//!
//! # Degradation
//!
//! Everything that can fail in a headless/disconnected session (flyout window,
//! tray icon, overlay dimmer) is handled: the flyout/tray are fatal (logged,
//! non-zero exit — there is no app without them), while a missing dimmer only
//! disables software dimming (hardware brightness still works).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use crossbeam_channel::{Receiver, Sender};
use tracing::{debug, error, info, warn};

use duja_app::{Engine, EngineCommand, EngineConfig, EngineNotification, Enumeration};
use duja_core::config::Config;
use duja_core::id::StableDisplayId;
use duja_dimmer::PlatformDimmer;
use duja_platform::{Autostart, PipeServer};
use duja_ui::{AccentChoice, FlyoutShell, FlyoutVm, SettingsShell, SettingsVm};

use crate::bin_support::bounds::BoundsMap;
use crate::bin_support::clone_group::CloneGrouping;
use crate::bin_support::level_forward::{EngineLevelSink, LevelForwarder};
use crate::bin_support::paths::DujaPaths;
use crate::bin_support::state_store::StateStore;
use crate::bin_support::{backend, gamma, ipc, run, settings, settings_apply, startup};

use self::state::AppState;
use self::wiring::{build_tray, init_hotkeys, wire_event_sources};

mod geometry;
// One module name, two implementations, chosen by target. `#[path]` rather than
// two `use` aliases so every importer — `wiring.rs`, `state.rs` — names
// `hotkey_os` unconditionally and no caller carries the platform switch.
#[cfg(not(target_os = "linux"))]
mod hotkey_os;
// The path is relative to the directory holding *this* file (`bin_support/`),
// not to the module's own directory, which is why it carries the `tray/` prefix.
#[cfg(target_os = "linux")]
#[path = "tray/hotkey_none.rs"]
mod hotkey_os;
// `tray-icon`-shaped: it returns a `tray_icon::Icon` and a `tray_icon::BadIcon`,
// neither of which exists on Linux, where that crate is not a dependency. The
// Linux glyph is built in `ksni_tray` against `ksni::Icon` instead, from the same
// `duja_ui::icon::monitor_rgba` source, so the two arms share the drawing and
// differ only in the type they hand their library.
#[cfg(not(target_os = "linux"))]
mod icon;
#[cfg(target_os = "linux")]
mod ksni_tray;
// The Linux tray's one host-testable rule. `cfg(any(test, …))` rather than
// `cfg(target_os = "linux")` so every lane's `cargo test` compiles it — see the
// module's own header for why that distinction is load-bearing here.
//
// `//` and not `///`, deliberately. An outer doc comment here would be
// concatenated with the module's own `//!` header, and rustdoc resolves the
// combined text in the scope of the *declaration* — so the header's
// `[`super::ksni_tray`]` would start looking in `bin_support` instead of
// `bin_support::tray` and fail on the ubuntu lane alone. It did.
#[cfg(any(test, target_os = "linux"))]
mod linux_icon;
// Owns the `Timer::single_shot` the loop-time assembly rides on, and the witness
// type that makes re-inlining it into the pre-loop phase a compile error. Kept
// free of every other `bin_support` import so `tests/loop_running_token.rs` can
// pull it in through `#[path]` and drive it against the real Slint/winit stack.
mod loop_running;
mod policy;
mod state;
mod surface;
mod update_flow;
mod wiring;

// The flyout's geometry lives in `duja-ui`, next to the `.slint` markup it is
// arithmetic over. It used to be three constants and a method here, in the crate
// that cannot see the file they each claim to match - which is how the frame
// probe came to measure a window size the app never presents. Re-exported rather
// than re-declared so every call site below is unchanged.
use duja_ui::layout::{FLYOUT_LOGICAL_WIDTH, FLYOUT_MAX_LOGICAL_HEIGHT, FLYOUT_MIN_LOGICAL_HEIGHT};
/// The settings window's initial logical size (matches `settings.slint`'s
/// `preferred-width`/`preferred-height`). The window is user-resizable from here.
const SETTINGS_LOGICAL_WIDTH: f32 = 560.0;
const SETTINGS_LOGICAL_HEIGHT: f32 = 700.0;
/// Gap kept from the work-area edges when placing the flyout.
const FLYOUT_MARGIN: i32 = 12;

thread_local! {
    /// The main-thread application state, reachable from the foreign
    /// (tray/menu/notification) event handlers that hop onto the Slint loop.
    /// Access always goes through [`with_app`] / [`with_app_ref`], never a raw
    /// borrow, so a re-entrant Slint callback can never nest the borrow.
    static APP: ReentrantCell<AppState> = const { ReentrantCell::new() };
}

/// A single-threaded cell that **serialises** mutable access so a re-entrant call
/// (one made from inside a running access) is deferred and drained afterwards
/// rather than nesting the borrow.
///
/// This is the structural cure for the latent double-borrow the P5 gate flagged
/// (debt.md): a settings/flyout callback calls `update_from_vm`/`set_*`/`show`,
/// and if any such Slint write were to synchronously fire another Slint callback
/// (a `changed`/`toggled`/two-way-binding write-back), that callback would
/// re-enter and `borrow_mut()` the already-borrowed cell, panicking straight into
/// Slint's FFI (→ abort — the `0xe06d7363` → `0xc0000409` live-QA crash). A
/// re-entrant [`with`](ReentrantCell::with) instead finds `busy == true`, queues its work,
/// and returns immediately; the in-flight call drains the queue after its own
/// borrow ends, so no two `with` bodies ever hold the borrow at once.
struct ReentrantCell<T> {
    slot: RefCell<Option<T>>,
    busy: Cell<bool>,
    queue: RefCell<VecDeque<Deferred<T>>>,
}

/// One deferred unit of work queued by a re-entrant [`ReentrantCell::with`].
type Deferred<T> = Box<dyn FnOnce(&mut T)>;

impl<T> ReentrantCell<T> {
    const fn new() -> Self {
        ReentrantCell {
            slot: RefCell::new(None),
            busy: Cell::new(false),
            queue: RefCell::new(VecDeque::new()),
        }
    }

    /// Install (or clear) the held value. Used once at startup and teardown, when
    /// nothing is running.
    fn set(&self, value: Option<T>) {
        *self.slot.borrow_mut() = value;
    }

    /// Run `f` against the value if present, re-entrancy-safe (see the type doc).
    /// A call made while another is in progress is deferred and drained by the
    /// active call.
    fn with(&self, f: impl FnOnce(&mut T) + 'static) {
        if self.busy.get() {
            self.queue.borrow_mut().push_back(Box::new(f));
            return;
        }
        self.busy.set(true);
        self.run_one(Box::new(f));
        while let Some(next) = self.pop() {
            self.run_one(next);
        }
        self.busy.set(false);
    }

    /// Borrow the value for exactly one queued unit of work; the borrow is
    /// released before the next unit runs, so nothing nests.
    fn run_one(&self, f: Deferred<T>) {
        if let Some(value) = self.slot.borrow_mut().as_mut() {
            f(value);
        }
    }

    /// Pop the next deferred unit of work, releasing the queue borrow first.
    fn pop(&self) -> Option<Deferred<T>> {
        self.queue.borrow_mut().pop_front()
    }

    /// Read the value immutably (setup only, never re-entrant): register
    /// callbacks that themselves route through [`with`](ReentrantCell::with).
    fn with_ref<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.slot.borrow().as_ref().map(f)
    }
}

/// The one way every foreign event handler (tray/menu/hotkey/IPC/notification,
/// and each Slint callback) reaches [`AppState`] — re-entrancy-safe.
fn with_app(f: impl FnOnce(&mut AppState) + 'static) {
    APP.with(|cell| cell.with(f));
}

/// The read-only twin of [`with_app`], for the one-time setup borrows that only
/// need `&AppState` (registering Slint callbacks). Returns `None` before the
/// state is installed or while a `with` body holds the borrow.
///
/// This exists so [`APP`] stays confined to this module: a raw
/// `APP.with(|cell| ...)` elsewhere could reach past the serialising cell and
/// re-open the double-borrow abort described on [`ReentrantCell`].
fn with_app_ref<R>(f: impl FnOnce(&AppState) -> R) -> Option<R> {
    APP.with(|cell| cell.with_ref(f))
}

/// An action requested by a tray/menu/hotkey interaction, applied on the Slint
/// thread.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// Show the flyout at the tray/cursor anchor.
    Open,
    /// Toggle the flyout's visibility.
    Toggle,
    /// Open the settings window.
    OpenSettings,
    /// Restore the screen (clear overlays + identity gamma on every display).
    Restore,
    /// Nudge every display's brightness by the given signed step (a hotkey).
    ///
    /// The only [`Action`] with no menu item behind it, which is why it is the
    /// only one Linux never constructs.
    // RATIONALE (dead_code): see `hotkey::Modifiers::is_empty`.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Nudge(i16),
    /// Open the GitHub releases page (the "Update available" menu item). Duja
    /// only ever opens the page — it never downloads.
    OpenReleases,
    /// Restart: spawn a fresh instance that takes over once this one has quit.
    Restart,
    /// Begin a clean shutdown.
    Quit,
}

/// Run the tray application. Returns the process exit code.
///
/// What this function exists to keep visible in one place is the **ordering**,
/// and since the event-loop-first restructure that ordering is two phases:
///
/// 1. pre-loop resource acquisition — paths, instance/installer guards, crash
///    recovery, config + HDR verdict, both windows, engine/pump/dimmer, gamma;
/// 2. the loop-time assembly ([`assemble_with_loop_running`]) queued onto the
///    loop, then the loop itself, then teardown in the reverse order of
///    acquisition (IPC → engine → forwarder → [`AppState`] → instance guard).
///
/// The `#[allow(clippy::too_many_lines)]` this function used to carry is gone:
/// moving the tray-dependent tail (and with it the ~30-line `AppState` literal)
/// into phase 2 brought the body under the threshold honestly, rather than by
/// re-arguing the exemption.
///
/// # Errors
/// Fatal setup failures bubble up so `main` exits non-zero: the flyout or
/// settings window failing to be created and the platform event pump failing to
/// start return directly, while a loop-time failure (the tray icon) is recorded
/// by the queued assembly, quits the loop, and is re-raised here — see
/// [`LoopAssembly`].
///
/// The tray-failure **message and exit code are unchanged** by the restructure,
/// but its *route* is not, and the difference is not free: `build_tray` used to
/// run before `run::start_platform`, `Engine::spawn` and `PlatformDimmer::spawn`,
/// so a tray failure returned having started nothing. It now happens after all
/// three, so a launch that cannot create a tray has already run a full hardware
/// enumeration (DDC I2C reads across every monitor) and spawned the pump and the
/// overlay-dimmer thread, and unwinds through the engine's bounded (~2 s)
/// worker-join. Unavoidable while the closure owns the engine sender, and a
/// failed launch is not a hot path — but it is a real change in what a failed
/// launch costs.
pub(crate) fn run(verbose: bool, relaunch: bool) -> anyhow::Result<ExitCode> {
    let _ = verbose; // logging is initialised by the caller.
    let paths = DujaPaths::resolve_or_fallback();

    // 1. Single-instance guard: a second launch asks the running instance to
    //    surface its flyout over IPC, then exits 0. A `relaunch` (spawned by the
    //    tray "Restart" item) instead WAITS for the outgoing instance to release
    //    the lock, then takes over.
    let instance = acquire_single_instance(relaunch);
    if instance.already_running() {
        // A relaunch that timed out waiting for the outgoing instance falls
        // through and starts anyway: bailing to "show the running flyout and exit"
        // here would leave NO instance once the old one finishes quitting — a
        // restart that killed the app. A brief two-instance overlap is the safer
        // failure. A plain second launch still hands off and exits.
        if relaunch {
            warn!(
                "relaunch: the previous instance is still present after waiting; starting anyway"
            );
        } else if ipc::show_running_instance() {
            info!("another duja instance is running; asked it to show its flyout");
            return Ok(ExitCode::SUCCESS);
        } else {
            info!("another duja instance is already running; exiting");
            return Ok(ExitCode::SUCCESS);
        }
    }

    // 1b. Hold the fixed-name installer-detection mutex for our whole lifetime so
    //     the Windows installer (`AppMutex=`) can detect a running instance; see
    //     `InstallerGuard`. Named binding = held across `run()`; no-op off Windows.
    let _installer_guard = duja_platform::InstallerGuard::acquire();

    // 2. Crash-marker recovery: a dirty gamma exit is undone before we start.
    startup::recover_from_crash_marker(&paths.crash_marker, || {
        let report = duja_dimmer::restore_all();
        warn!(
            restored = report.restored.len(),
            failed = report.failed.len(),
            "recovered screen gamma after a dirty exit"
        );
    });

    // 3. Config + the once-per-run HDR gamma verdict.
    let config = load_config(&paths);
    let gamma_allowed =
        duja_dimmer::gamma_support_from_hdr(duja_dimmer::is_hdr_active()).allows_gamma();
    debug!(gamma_allowed, "resolved HDR gamma verdict");
    let theme = settings::ui_theme(
        config.general.theme,
        os_theme_if_needed(config.general.theme),
    );
    let accent = settings_apply::accent_to_choice(config.general.accent);

    // 4. Flyout window FIRST (icon-first: the UI must exist or there is no app).
    let (shell, vm) = build_flyout(theme, accent)?;

    // 4b. Settings window + autostart backend (window stays hidden until opened).
    let (settings_shell, settings_vm, autostart) = build_settings_window(accent)?;

    // 5. Async pipeline: engine (with a bounds-updating enumerator) + event pump
    //    + overlay dimmer. The dimmer is optional — its absence only disables
    //    software dimming.
    let bounds = Arc::new(Mutex::new(BoundsMap::default()));
    let (tick_rx, mut forwarder) = run::start_platform().context("starting the event pump")?;
    let (engine, notifications) = Engine::spawn(
        EngineConfig::default(),
        bounds_updating_enumerator(bounds.clone()),
        run::controller_factory(),
        tick_rx,
    );
    let dimmer: Option<Box<dyn duja_core::dimmer::Dimmer>> = match PlatformDimmer::spawn() {
        Ok(d) => Some(Box::new(d)),
        Err(e) => {
            error!(error = %e, "overlay dimmer unavailable; software dimming disabled");
            None
        }
    };

    // 6. The gamma channel correlates a resolved display id to the token that
    //    ADDRESSES it, via the same bounds map the overlay planner reads. Every
    //    DDC display carries one — external monitors and a DDC-fallback internal
    //    panel alike — and so does a macOS `DisplayServices` panel, which is
    //    addressed by its own `CGDirectDisplayID`: gamma on the built-in screen is
    //    reachable, deliberately, and `docs/debt.md` carries the hazard that comes
    //    with it. The two that carry no token are a Windows WMI panel and a macOS
    //    panel whose bounds came back degenerate, and gamma never targets those.
    //
    //    `gamma_token_for`, never `surface_token_for`: the two are the same GDI
    //    device name on Windows but diverge on macOS, where a mirror clone's
    //    surface token is the MASTER's display id — possibly one Duja never
    //    enumerated — and driving gamma through it would dim the wrong screen.
    let gamma = gamma::GammaBackend::new(paths.crash_marker.clone(), {
        let bounds = bounds.clone();
        move |id| bounds.lock().ok().and_then(|b| b.gamma_token_for(id))
    });

    // 7. Queue the loop-time assembly (tray icon, hotkeys, `AppState`, event
    //    sources, IPC) as the first work the event loop does — see
    //    `queue_loop_time_assembly` for why the tray cannot be built here, and
    //    `LoopAssembly` for how a failure inside the closure still exits non-zero.
    let resources = LoopStartResources {
        paths,
        config,
        accent,
        gamma_allowed,
        shell,
        vm,
        settings_shell,
        settings_vm,
        autostart,
        bounds,
        dimmer,
        engine_tx: engine.sender(),
        gamma,
        notifications,
    };
    let assembly: Rc<LoopAssembly<PipeServer>> = Rc::new(LoopAssembly::new());
    queue_loop_time_assembly(resources, &assembly);

    let loop_result = slint::run_event_loop_until_quit();
    // A loop that fails to *start* is logged and still exits 0 — pre-existing
    // behaviour, deliberately not changed here. Note what it now also implies: the
    // queued assembly would never have run, so there would be no tray either.
    // Unreachable in practice (winit dispatches `StartCause::Init` on the loop's
    // first pass on both platforms, which is what fires the closure), and tracked
    // in debt.md rather than fixed inside a behaviour-neutral restructure.
    if let Err(e) = loop_result {
        error!(error = %e, "event loop exited with an error");
    }

    // 8. Clean teardown (state was flushed on Quit; this joins the threads). The
    //    order is unchanged by the restructure: the IPC server built inside the
    //    closure is threaded back out through `LoopAssembly` rather than moved
    //    into `AppState`, so overlays are still cleared *after* the engine stops.
    if let Some(server) = assembly.take_server() {
        server.shutdown();
    }
    engine.shutdown();
    forwarder.shutdown();
    APP.with(|cell| {
        // Dropping the AppState clears overlays via the dimmer's own teardown, and
        // drops the tray icon it now owns (same ordering as the old `drop(tray)`).
        cell.set(None);
    });
    drop(instance);
    // Re-raise a loop-time failure after teardown, so a tray that could not be
    // created exits non-zero with the same message it did when `build_tray` ran
    // before the loop.
    if let Some(error) = assembly.take_failure() {
        return Err(error);
    }
    Ok(ExitCode::SUCCESS)
}

/// Queue [`assemble_with_loop_running`] to run as the first work the (not yet
/// running) event loop does, reporting its outcome through `assembly`.
///
/// The queueing mechanism — a zero-duration single-shot Slint timer, and why that
/// rather than [`slint::invoke_from_event_loop`] or a custom winit application
/// handler — lives with the code that performs it, in [`loop_running`]. What
/// belongs here is only the outcome routing: a closure queued onto the loop can
/// neither `?` out of [`run()`] nor return a value, so both directions go through
/// [`LoopAssembly`], and the fatal arm has to end the loop itself.
fn queue_loop_time_assembly(
    resources: LoopStartResources,
    assembly: &Rc<LoopAssembly<PipeServer>>,
) {
    let assembly = Rc::clone(assembly);
    loop_running::when_loop_running(move |running| {
        match assemble_with_loop_running(resources, running) {
            Ok(server) => {
                assembly.store_server(server);
                // Logged here, not before the loop: everything this message claims
                // is running (tray, hotkeys, state, IPC) exists only now.
                info!("duja tray running");
            }
            Err(error) => {
                // There is no app without a tray. Record the cause, stop the loop,
                // and let `run` re-raise it after teardown so the process still
                // exits non-zero with this message rather than lingering as a
                // running-but-invisible app.
                error!(error = %format!("{error:#}"), "fatal: tray assembly failed");
                assembly.record_failure(error);
                // This call is the only thing that ends the process on this path, so
                // a failure to send the quit gets a line of its own: the result
                // would be an invisible process with both shells already dropped and
                // no way for the user to exit it — worse than the failure it is
                // reporting. Unreachable (the loop is running by construction here,
                // so `quit_event_loop`'s generation stamp matches), but not silently
                // discarded.
                if let Err(e) = slint::quit_event_loop() {
                    error!(error = %e, "could not stop the event loop after a fatal tray failure");
                }
            }
        }
    });
}

/// The resources [`run()`] acquires **before** the event loop starts, moved
/// wholesale into the queued loop-time assembly.
///
/// Grouped into one struct rather than passed as arguments because
/// [`assemble_with_loop_running`] needs all of them and the list would otherwise
/// trip `clippy::too_many_arguments`. Every field is a resource whose acquisition
/// is safe (and, for the two windows, fallibly `?`-able) before the loop runs —
/// unlike the tray icon and the global-hotkey manager, which are built from it at
/// loop time.
struct LoopStartResources {
    /// Config/state/crash-marker paths, consumed by the `AppState` literal.
    paths: DujaPaths,
    /// The loaded config (defaults if unreadable).
    config: Config,
    /// The resolved accent, for the tray glyph colour.
    accent: AccentChoice,
    /// The once-per-run HDR gamma verdict, probed pre-loop.
    gamma_allowed: bool,
    /// The flyout window.
    shell: FlyoutShell,
    /// The flyout's shared view-model.
    vm: Rc<RefCell<FlyoutVm>>,
    /// The (hidden) settings window.
    settings_shell: SettingsShell,
    /// The settings window's shared view-model.
    settings_vm: Rc<RefCell<SettingsVm>>,
    /// The platform autostart backend, if one resolved.
    autostart: Option<Box<dyn Autostart>>,
    /// The shared display-bounds map the enumerator refreshes.
    bounds: Arc<Mutex<BoundsMap>>,
    /// The overlay dimmer, if it spawned. Boxed behind its trait so `AppState`'s
    /// field can be substituted in a test - see that field's doc.
    dimmer: Option<Box<dyn duja_core::dimmer::Dimmer>>,
    /// The engine command channel (cloned for the level forwarder and the IPC
    /// bridge).
    engine_tx: Sender<EngineCommand>,
    /// The opt-in gamma sub-floor channel.
    gamma: gamma::GammaBackend,
    /// The engine's notification stream, bridged onto the Slint loop at loop time.
    notifications: Receiver<EngineNotification>,
}

/// The two things the loop-time assembly has to hand back to [`run()`], shared with
/// the queued closure through an `Rc`.
///
/// A closure queued onto the event loop cannot `?` out of [`run()`] and cannot
/// return a value, so both directions go through this cell:
///
/// - **the fatal error.** `build_tray` failing is fatal — there is no app without
///   a tray — and that must stay a non-zero exit with the same message, not a
///   silently running invisible app. The closure records it, quits the loop, and
///   `run` re-raises it after teardown.
/// - **the IPC server.** Teardown order is load-bearing (IPC → engine →
///   forwarder → `AppState`), so the server is threaded back out here rather than
///   parked in `AppState`; moving it there would clear overlays before the engine
///   stopped.
///
/// Generic over the server handle purely so that hand-back contract is unit
/// testable: a real [`PipeServer`] in a test would bind the process-wide pipe name
/// and collide with any running Duja.
struct LoopAssembly<S> {
    /// The first fatal loop-time failure, if any.
    failure: RefCell<Option<anyhow::Error>>,
    /// The IPC server handle, awaiting teardown.
    server: RefCell<Option<S>>,
}

impl<S> LoopAssembly<S> {
    /// An assembly that has neither failed nor produced a server yet.
    fn new() -> Self {
        LoopAssembly {
            failure: RefCell::new(None),
            server: RefCell::new(None),
        }
    }

    /// Record a fatal loop-time failure. The **first** one wins, so the reported
    /// cause is the one that actually stopped the assembly rather than whatever
    /// failed last on the way out.
    fn record_failure(&self, error: anyhow::Error) {
        let mut slot = self.failure.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    }

    /// Take the recorded failure, if any. `None` means the assembly succeeded and
    /// [`run()`] may report success.
    fn take_failure(&self) -> Option<anyhow::Error> {
        self.failure.borrow_mut().take()
    }

    /// Hand the (optional) IPC server back for teardown. `None` is a legitimate
    /// outcome: the transport being unavailable only disables the control API.
    fn store_server(&self, server: Option<S>) {
        *self.server.borrow_mut() = server;
    }

    /// Take the IPC server for shutdown. Taking (rather than borrowing) is
    /// deliberate: the handle must be shut down exactly once.
    fn take_server(&self) -> Option<S> {
        self.server.borrow_mut().take()
    }
}

/// Build the tray icon and the global-hotkey manager, publish [`AppState`], wire
/// every event source, and start the IPC server — the whole tray-dependent tail
/// of startup, running with the event loop already going.
///
/// The relative order here is the same one `run` used before the restructure, and
/// two parts of it are load-bearing:
///
/// - the tray icon and the hotkey manager are created **first**, because both
///   crates require a running main-thread loop on macOS and both feed `AppState`;
/// - the IPC server starts **last**, strictly after `AppState` is published, so a
///   `dujactl` request can never arrive before the state its handler reaches for
///   exists.
///
/// `running` is what makes the first of those two structural rather than
/// documented: it is the witness [`loop_running`] mints inside the queued
/// callback, both calls below require one, and there is no way to obtain one in
/// [`run()`]'s pre-loop phase. Moving them back there — which used to leave the
/// whole suite green — no longer compiles.
///
/// # Errors
/// Returns an error if the tray icon or its menu cannot be created. That is fatal
/// (see [`LoopAssembly`]); everything else here degrades in place.
fn assemble_with_loop_running(
    resources: LoopStartResources,
    running: &loop_running::LoopRunning,
) -> anyhow::Result<Option<PipeServer>> {
    let LoopStartResources {
        paths,
        config,
        accent,
        gamma_allowed,
        shell,
        vm,
        settings_shell,
        settings_vm,
        autostart,
        bounds,
        dimmer,
        engine_tx,
        gamma,
        notifications,
    } = resources;

    // Become a menu-bar-only app on macOS. No-op elsewhere. FIRST, and inside the
    // loop rather than before it — see `become_accessory_app` for why the obvious
    // placement does not work.
    become_accessory_app();

    // Tray icon + menu on the Slint main thread (glyph/colour shared with the
    // taskbar icons via `duja_ui::icon`), plus the update-surface handles.
    let tray = build_tray(running, accent).context("creating the tray icon")?;

    // Global hotkeys: same main-thread-with-a-running-loop requirement as the
    // tray. A failure only disables the affected binding.
    let (hotkeys, hotkey_outcomes) = init_hotkeys(running, &config);

    APP.with(|cell| {
        cell.set(Some(AppState {
            shell,
            vm,
            settings_shell,
            settings_vm,
            autostart,
            config_path: paths.config.clone(),
            snapshots: Vec::new(),
            dimmer,
            config,
            gamma_allowed,
            // The verdict was probed during pre-loop acquisition, a few
            // milliseconds ago; start the TTL clock here so the first
            // enumeration does not immediately re-probe.
            last_gamma_probe: Some(Instant::now()),
            bounds,
            state: StateStore::load(paths.state.clone()),
            crash_marker: paths.crash_marker.clone(),
            engine_tx: engine_tx.clone(),
            levels: LevelForwarder::new(EngineLevelSink::new(engine_tx.clone())),
            gamma,
            displays: Vec::new(),
            groups: CloneGrouping::default(),
            unresponsive: BTreeSet::new(),
            user_controlled: BTreeSet::new(),
            flyout_visible: false,
            last_hidden: None,
            hotkeys,
            hotkey_outcomes,
            tray,
            update_available: None,
            update_check_in_flight: false,
        }));
    });
    wire_event_sources(notifications);

    // IPC control server: dujactl and second launches talk to us over the pipe.
    Ok(ipc::start(Arc::new(ipc::TrayBridge::new(engine_tx))))
}

/// The settings window shell, its shared view-model, and the (optional)
/// autostart backend, as returned by [`build_settings_window`].
type SettingsSetup = (
    SettingsShell,
    Rc<RefCell<SettingsVm>>,
    Option<Box<dyn Autostart>>,
);

/// Build the flyout window, seeded with the resolved theme and accent.
///
/// The view-model carries both, so the shell's first render already paints the
/// right palette; the taskbar icon is seeded here too, since it is a raster buffer
/// rather than a palette property.
///
/// # Errors
/// Returns an error if the flyout window cannot be created (fatal — without a UI
/// there is no app).
fn build_flyout(
    theme: duja_ui::Theme,
    accent: AccentChoice,
) -> anyhow::Result<(FlyoutShell, Rc<RefCell<FlyoutVm>>)> {
    let vm = Rc::new(RefCell::new(FlyoutVm::new()));
    {
        let mut v = vm.borrow_mut();
        v.set_theme(theme);
        v.set_accent(accent);
    }
    let shell = FlyoutShell::new(vm.clone())
        .map_err(|e| anyhow::anyhow!("failed to create the flyout window: {e}"))?;
    shell.set_icon_rgb(duja_ui::accent::icon_rgb(accent));
    Ok((shell, vm))
}

/// Create the settings window shell + view-model and resolve the platform
/// autostart backend.
///
/// # Errors
/// Returns an error if the settings window cannot be created (fatal, like the
/// flyout). An autostart resolve failure is *not* fatal — it only disables the
/// launch-at-login toggle.
fn build_settings_window(accent: AccentChoice) -> anyhow::Result<SettingsSetup> {
    let settings_vm = Rc::new(RefCell::new(SettingsVm::new()));
    let settings_shell = SettingsShell::new(settings_vm.clone())
        .map_err(|e| anyhow::anyhow!("failed to create the settings window: {e}"))?;
    // Seed the taskbar icon; the palette itself follows on the first
    // `rebuild_settings`, which pushes the accent through the view-model.
    settings_shell.set_icon_rgb(duja_ui::accent::icon_rgb(accent));
    let autostart: Option<Box<dyn Autostart>> = match duja_platform::autostart::system() {
        Ok(a) => Some(Box::new(a)),
        Err(e) => {
            warn!(error = %e, "autostart unavailable; the launch-at-login toggle is disabled");
            None
        }
    };
    Ok((settings_shell, settings_vm, autostart))
}

/// Build the enumerator the engine calls each refresh: it discovers displays and
/// their bounds, updates the shared bounds map, and returns the enumeration.
fn bounds_updating_enumerator(bounds: Arc<Mutex<BoundsMap>>) -> duja_app::Enumerator {
    Box::new(move || {
        let (displays, discovered_bounds) = backend::discover_all();
        if let Ok(mut guard) = bounds.lock() {
            *guard = BoundsMap::new(discovered_bounds);
        }
        Enumeration { displays }
    })
}

/// Open `url` in the user's default browser. Best-effort: a failure is logged,
/// never fatal. Duja only ever opens the releases *page* — it never downloads
/// anything.
///
/// The platform call itself lives in `duja_platform::desktop`; this wrapper is
/// only the logging policy, which is the app's to decide.
/// # Under `cfg(test)` this records instead of opening, and that is not optional
///
/// `duja_platform::open_url` is a real `ShellExecuteW`. It is not inert in a
/// test process: an `AppState` test that reaches `Action::OpenReleases` or
/// `SettingsCommand::OpenReleasesPage` **launches the operator's browser**, and
/// one did - on every `cargo test`, measured through browser process start
/// times.
///
/// That is the hazard `toast::notify_update_available` was given a seam for one
/// commit earlier, in a message that even names this call ("opened the releases
/// page if clicked"). The rule was written down and then walked past into the
/// un-seamed sibling on the same code path. So the sibling has the seam too, and
/// the same `cfg!` **expression** rather than a `#[cfg]` attribute, so
/// `duja_platform::open_url` stays referenced and linted in the test profile.
fn open_url(url: &str) {
    if cfg!(test) {
        #[cfg(test)]
        opened::record(url);
        return;
    }
    if let Err(failure) = duja_platform::open_url(url) {
        warn!(url, code = ?failure.code, "failed to open the releases page");
    }
}

/// What [`open_url`] was asked to open, for the tests that would otherwise have
/// opened it. Thread-local, so tests on different threads cannot see each
/// other's.
#[cfg(test)]
pub(crate) mod opened {
    use std::cell::RefCell;

    thread_local! {
        static URLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Record one URL instead of opening it.
    pub(super) fn record(url: &str) {
        URLS.with(|urls| urls.borrow_mut().push(url.to_owned()));
    }

    /// Every URL opened on this thread so far, oldest first.
    pub(crate) fn urls() -> Vec<String> {
        URLS.with(|urls| urls.borrow().clone())
    }

    /// Forget everything recorded on this thread.
    pub(crate) fn clear() {
        URLS.with(|urls| urls.borrow_mut().clear());
    }
}

/// Apply an IPC `set` on the Slint main thread through the flyout's own
/// `set_user_level` path, so the persisted level and the overlay/gamma batch
/// stay consistent with a slider drag. Callable from the IPC handler thread.
pub(crate) fn ipc_apply_set_level(id: StableDisplayId, pct: u8) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(move |app| app.set_user_level(&id, pct));
    });
}

/// Surface the flyout on the Slint main thread (IPC `ShowFlyout` / second
/// instance). Callable from the IPC handler thread.
pub(crate) fn ipc_show_flyout() {
    let _ = slint::invoke_from_event_loop(|| {
        with_app(AppState::show_flyout);
    });
}

/// How long a relaunched instance waits for the outgoing one to release the
/// single-instance lock before giving up and starting anyway. The outgoing
/// instance's own shutdown is bounded well below this (a ~2s worker-join budget),
/// so the wait is comfortably longer than a clean quit takes.
const RELAUNCH_WAIT: Duration = Duration::from_secs(5);

/// The poll gap while waiting for the single-instance lock to free.
const RELAUNCH_POLL: Duration = Duration::from_millis(50);

/// Spawn a detached replacement `duja` (`--relaunch`) that takes over once this
/// instance releases the single-instance lock. The child is an independent
/// process, so it survives our own exit.
///
/// # Errors
/// Propagates a failure to resolve the current executable or to spawn it.
fn spawn_relaunch() -> std::io::Result<()> {
    use std::process::Stdio;
    let exe = std::env::current_exe()?;
    // Detach the child's std streams (the tray build has no console anyway) so it
    // never shares an inherited stdio handle with this exiting process.
    std::process::Command::new(exe)
        .arg("--relaunch")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Acquire the single-instance lock, waiting a bounded window when we were
/// relaunched by "Restart" (the outgoing instance may still hold the lock for a
/// moment after spawning us).
///
/// Each failed attempt **drops** its guard so its handle to the named object is
/// closed; once the outgoing process also exits, no handle remains, the object is
/// destroyed, and the next attempt creates it fresh (`already_running == false`).
/// On timeout the last guard is returned as-is (still `already_running`), and the
/// caller starts anyway rather than leaving the user with no instance.
fn acquire_single_instance(relaunch: bool) -> duja_platform::SingleInstance {
    let instance = duja_platform::SingleInstance::acquire();
    if !relaunch || !instance.already_running() {
        return instance;
    }
    // Release our handle before waiting so the named object can be torn down the
    // instant the outgoing process closes its own.
    drop(instance);
    let start = Instant::now();
    let deadline = start.checked_add(RELAUNCH_WAIT).unwrap_or(start);
    loop {
        std::thread::sleep(RELAUNCH_POLL);
        let instance = duja_platform::SingleInstance::acquire();
        if !instance.already_running() || Instant::now() >= deadline {
            return instance;
        }
        drop(instance);
    }
}

/// Load the config, tolerating a missing file (defaults) and logging a broken
/// one (also defaults — never blocks startup).
fn load_config(paths: &DujaPaths) -> Config {
    use duja_core::config::ConfigDocument;
    match ConfigDocument::load(&paths.config).and_then(|doc| doc.config()) {
        Ok(config) => config,
        Err(e) => {
            warn!(error = %e, "config unreadable; using defaults");
            Config::default()
        }
    }
}

/// Make this a menu-bar-only application on macOS: no Dock icon, no app menu,
/// no window in the ⌘-Tab switcher. A no-op on every other platform.
///
/// `NSApplicationActivationPolicy::Accessory` is the supported way to say "I am a
/// status-item app". Without it Duja takes a Dock tile and a menu bar of its own,
/// which for a tray utility is wrong on both counts.
///
/// The Dock tile and the menu bar are genuinely prevented. **Focus is not, quite:**
/// winit calls `activateIgnoringOtherApps(true)` eleven lines before it dispatches
/// `StartCause::Init`, so the activation request is already with the window server
/// by the time this downgrades the policy. The practical effect is likely nil — at
/// that instant Duja has no visible window, and an accessory app with none holds
/// nothing meaningful — but it is unverified, so this claims the two halves it
/// actually delivers.
///
/// # Why this is called from inside the running loop
///
/// The obvious placement — early in `run`, before any window exists — **does not
/// work**, and the reasoning that recommends it is backwards. winit sets the
/// policy itself, later, and would overwrite an early call:
///
/// - Slint never specifies one. `i-slint-backend-winit` builds the
///   `EventLoopBuilder` and calls only `with_default_menu(false)`, so winit's
///   `activation_policy` stays `None`.
/// - With `None` **and an unbundled process**, winit's
///   `applicationDidFinishLaunching` forces `Regular` — the branch exists so a
///   *bundled* app can express `LSUIElement` in its `Info.plist` instead. Duja is
///   unbundled until the packaging work lands, so that branch is the one taken.
/// - That override runs *before* `dispatch_init_events`, i.e. before
///   `StartCause::Init`, which is what fires `#94`'s queued
///   [`slint::Timer::single_shot`].
///
/// So the loop-time assembly is not merely an acceptable home for this, it is the
/// only one that survives — the opposite of the ordering `#94` established for the
/// tray icon, and worth stating plainly because the intuition points the other way.
/// winit's own comment gives the same advice from the other side: it delays setting
/// the policy "until `applicationDidFinishLaunching` has been called" because
/// otherwise "the menu bar is initially unresponsive on macOS 10.15".
///
/// # The declarative half, now that there is a bundle
///
/// C6 gives Duja a `.app` whose `Info.plist` carries `LSUIElement` (composed by
/// `xtask`'s `bundle` module), which is the declarative way to say the same thing —
/// so for a bundled copy winit's `is_bundled` branch stops overriding anything and
/// this call is belt-and-braces. Setting it here as well is still right, for the
/// reason that is actually checkable: a `cargo run` or portable copy has no
/// `Info.plist` to read at all. (Not for the reason it is tempting to give — a
/// `launchd`-exec'd copy *inside* the bundle is still bundled, because `NSBundle`
/// resolves upward from the executable path, which is the same thing winit's
/// `is_bundled` branch asks about.)
///
/// `NSApplication::sharedApplication` creates the shared instance if none exists,
/// which is harmless here — winit 0.30 deliberately swizzles rather than
/// subclassing `NSApplication` precisely so the app object can be someone else's.
/// (This is why `desktop::os_dark_theme` still avoids it: there, creating the app
/// object would be a side effect rather than the point.)
#[cfg(target_os = "macos")]
fn become_accessory_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    // `run` is called from `main` on the process's main thread, so this marker is
    // always available; if that ever stops being true, silently skipping is the
    // right degrade — a Dock icon is a cosmetic wart, not a broken app.
    let Some(mtm) = MainThreadMarker::new() else {
        warn!("not on the main thread; leaving the activation policy alone");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
        warn!("could not set the accessory activation policy; Duja may show a Dock icon");
    }
}

/// No activation policy to set on this platform.
#[cfg(not(target_os = "macos"))]
const fn become_accessory_app() {}

/// The OS dark/light preference, for the `System` theme setting.
///
/// Answered by `duja_platform::desktop` straight from the OS (Windows'
/// `AppsUseLightTheme`, macOS' `AppleInterfaceStyle`) rather than through
/// winit/Slint, neither of which exposes it in the pinned versions — which is why
/// this returned a flat `None` from P4 until now. `None` still means "no answer",
/// and `settings::ui_theme` still resolves that to dark.
fn os_dark_theme() -> Option<bool> {
    duja_platform::os_dark_theme()
}

/// [`os_dark_theme`], but **only** when the user's preference actually depends on
/// it.
///
/// `refresh_system_theme` runs before every flyout show and its own docs say it
/// is "a no-op when the preference is `Light`/`Dark` (the OS is not consulted)".
/// That was true of the resolution and false of the call: Rust evaluates
/// arguments eagerly, so the query ran on every show whatever the preference was.
///
/// On Windows that costs one `RegGetValueW` and the difference is invisible. On
/// Linux the same call is a **session-bus connection**: a SASL handshake, a
/// zbus connection with its own executor thread, and an XDG portal method call
/// with no client-side timeout — on the Slint main thread, before every tray
/// click. If `xdg-desktop-portal` is not already running, that call triggers bus
/// activation, whose `dbus-daemon` start timeout is 25 seconds, and the tray is
/// frozen for all of it.
pub(crate) fn os_theme_if_needed(pref: duja_core::config::Theme) -> Option<bool> {
    matches!(pref, duja_core::config::Theme::System)
        .then(os_dark_theme)
        .flatten()
}

/// Seconds since the Unix epoch (saturating; `0` if the clock is before epoch).
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// # What these tests do **not** cover
///
/// [`run()`] itself is unreachable from a test: it acquires a single-instance
/// lock, opens two real Slint windows, spawns the engine and pump, and then
/// blocks in the event loop. So the [`LoopAssembly`] tests below pin the
/// hand-back **cell** and nothing more — the *storage* contract, not what
/// [`run()`] does with it. Specifically **not** covered:
///
/// - that [`run()`] actually calls [`LoopAssembly::take_failure`] and returns the
///   error (delete those three lines and every test here still passes), nor that
///   it calls [`LoopAssembly::take_server`] at the right point in the teardown
///   order. The cell can only promise that what went in comes back out;
/// - that duja's own [`assemble_with_loop_running`] is the thing being queued.
///   The **mechanism** — that a zero-duration [`slint::Timer::single_shot`]
///   queued before the loop fires once, from inside it, leaving no timer behind —
///   *is* pinned, against the real Slint/winit stack, in
///   `tests/loop_time_assembly.rs`, and `tests/loop_running_token.rs` adds that
///   [`loop_running::when_loop_running`] defers rather than calls. Neither says
///   this closure is the one carrying duja's assembly.
///
/// **The wiring between the two is no longer in this list**, and the reason it
/// left is worth reading: it was never testable from here. Re-inlining
/// `build_tray`/`init_hotkeys` into the pre-loop phase used to keep the whole
/// suite green — 377 `duja-app` tests, measured with the defect restored at its
/// historical site — and it now fails to compile, because both take a
/// [`loop_running::LoopRunning`] that only the queued callback can produce. A
/// gap that a test could not reach was closed by a type instead.
///
/// The gaps that remain are Windows-invisible by construction (Windows tolerates
/// the old ordering), which is why this restructure landed on its own and why the
/// interactive smoke test is part of its evidence rather than an optional extra.
#[cfg(test)]
mod tests {
    #[test]
    fn reentrant_cell_defers_instead_of_nesting_the_borrow() {
        use super::ReentrantCell;
        thread_local! {
            static CELL: ReentrantCell<Vec<u32>> = const { ReentrantCell::new() };
        }
        CELL.with(|c| c.set(Some(Vec::new())));

        CELL.with(|c| {
            c.with(|v| {
                v.push(1);
                // Re-enter from inside a running `with`. A raw `borrow_mut`
                // (the pre-fix pattern) would panic here with `BorrowMutError`
                // and unwind into Slint's FFI → abort. The cell must instead
                // defer this unit of work.
                CELL.with(|c| c.with(|v| v.push(3)));
                v.push(2);
            });
        });

        let out = CELL.with(|c| c.with_ref(Clone::clone));
        // The deferred re-entrant push ran *after* the outer body finished, and
        // nothing panicked — the structural cure for P0 bugs 1 & 2.
        assert_eq!(out, Some(vec![1, 2, 3]));
    }

    /// A recorded failure comes back out with its message intact.
    ///
    /// Named for what it asserts, not for the consequence: whether `run` then
    /// *exits non-zero* is a property of `run`, which no test here reaches. What
    /// this does catch is a `record_failure` that drops its argument or a
    /// `take_failure` that always answers `None` — either of which would compile,
    /// keep the whole suite green, and turn "the tray could not be created" from
    /// the old inline `?` into a silent success with no tray and no window.
    #[test]
    fn a_recorded_failure_comes_back_out_with_its_message_intact() {
        let assembly = super::LoopAssembly::<u32>::new();
        assembly.record_failure(anyhow::anyhow!("failed to create the tray icon: nope"));

        let raised = assembly.take_failure().expect("the failure must come back");
        assert_eq!(
            format!("{raised}"),
            "failed to create the tray icon: nope",
            "the message `main` would print must be the one the assembly recorded"
        );
    }

    /// The first failure wins, so the reported cause is the one that stopped the
    /// assembly rather than whatever failed last on the way out.
    ///
    /// The one test here that pins a real decision — the `if slot.is_none()` guard
    /// in [`super::LoopAssembly::record_failure`]. Plain last-write-wins reds it.
    #[test]
    fn the_first_failure_wins_so_the_root_cause_is_what_gets_reported() {
        let assembly = super::LoopAssembly::<u32>::new();
        assembly.record_failure(anyhow::anyhow!("root cause"));
        assembly.record_failure(anyhow::anyhow!("later noise"));

        let raised = assembly.take_failure().expect("a failure was recorded");
        assert_eq!(format!("{raised}"), "root cause");
    }

    /// The server slot hands its handle back once, then reads empty.
    ///
    /// Again named for the assertion rather than the consequence: that `run`
    /// *shuts the server down* in the right teardown position is not covered here.
    /// The storage contract is: a stored handle comes back (a `store_server` that
    /// dropped it would leave the pipe-server thread running past the process's own
    /// shutdown sequence), a second take does not hand the same handle out twice,
    /// and `store_server(None)` — a legitimate outcome when the transport is
    /// unavailable — is indistinguishable from empty by design.
    #[test]
    fn the_server_slot_hands_its_handle_back_once_and_then_reads_empty() {
        let assembly = super::LoopAssembly::new();
        assert!(assembly.take_server().is_none(), "nothing stored yet");

        assembly.store_server(Some(7_u32));
        assert_eq!(assembly.take_server(), Some(7));
        assert!(
            assembly.take_server().is_none(),
            "a second take must not hand the same handle out again"
        );

        assembly.store_server(None);
        assert!(assembly.take_server().is_none());
    }
}

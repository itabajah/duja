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
//! `StartCause::Init` on the first pass. The ordering is **not** `cfg`-split:
//! Windows, the shipped platform, exercises exactly the sequence macOS depends
//! on.
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
use self::wiring::{TrayHandles, build_tray, init_hotkeys, wire_event_sources};

mod geometry;
mod hotkey_os;
mod icon;
mod policy;
mod state;
mod update_flow;
mod wiring;

/// The flyout's fixed logical width (matches `flyout.slint`).
const FLYOUT_LOGICAL_WIDTH: f32 = 360.0;
/// The flyout's hard maximum logical height. Beyond this the rows scroll rather
/// than the window growing (matches the `clamp(..., 620px)` in `flyout.slint`).
const FLYOUT_MAX_LOGICAL_HEIGHT: f32 = 620.0;
/// The flyout's minimum logical height (the empty-state / single-row floor,
/// matching the `clamp(160px, …)` in `flyout.slint`). The work-area cap is never
/// allowed to shrink the window below this.
const FLYOUT_MIN_LOGICAL_HEIGHT: f32 = 160.0;
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
    let theme = settings::ui_theme(config.general.theme, os_dark_theme());
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
    let dimmer = match PlatformDimmer::spawn() {
        Ok(d) => Some(d),
        Err(e) => {
            error!(error = %e, "overlay dimmer unavailable; software dimming disabled");
            None
        }
    };

    // 6. The gamma channel correlates a resolved display id to its GDI device via
    //    the same bounds map the overlay planner reads (DDC displays — external
    //    monitors and a DDC-fallback internal panel — carry a device name; WMI
    //    panels do not, so gamma never targets those).
    let gamma = gamma::GammaBackend::new(paths.crash_marker.clone(), {
        let bounds = bounds.clone();
        move |id| bounds.lock().ok().and_then(|b| b.device_for(id))
    });

    // 7. Queue the loop-time assembly (tray icon, hotkeys, `AppState`, event
    //    sources, IPC) as the first work the event loop does. See the module doc
    //    for why the tray cannot be built here, and `LoopAssembly` for how a
    //    failure inside the closure still exits non-zero.
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
    // DECISION (event-loop-first, verified in the pinned sources): a
    // zero-duration single-shot Slint timer is the queueing mechanism, not
    // `slint::invoke_from_event_loop` and not a custom winit application
    // handler.
    //
    // - It can only fire from `i_slint_core::platform::update_timers_and_animations`,
    //   and `i-slint-backend-winit` calls that from exactly two places:
    //   `ApplicationHandler::new_events` (`event_loop.rs`) and the Apple
    //   display-link callback (`frame_throttle/apple_display_link.rs`, which only
    //   exists once a window is rendering). Both are inside the running loop, so
    //   by construction the closure fires at `StartCause::Init` on the loop's
    //   first pass — exactly the point `tray-icon` documents as the earliest legal
    //   moment to create a macOS status item. (`i_slint_core::platform::set_platform`
    //   also calls it, but that already ran when the flyout window was created,
    //   before this timer exists.)
    // - `Timer::single_shot` takes `FnOnce() + 'static`; `invoke_from_event_loop`
    //   additionally requires `Send`, which `FlyoutShell`, `SettingsShell`,
    //   `Rc<RefCell<…Vm>>` and `GammaBackend` (it owns a bare `Box<dyn FnMut>`)
    //   are not. Using it would mean smuggling those main-thread-only values
    //   across a `Send` bound via a second thread-local or an `unsafe impl Send`.
    // - Winit *does* deliver queued user events strictly after
    //   `StartCause::Init` on both platforms (on macOS they are drained only in
    //   the `BeforeWaiting` observer, gated on `is_running`, which
    //   `applicationDidFinishLaunching:` sets immediately before dispatching
    //   Init + Resumed), so `invoke_from_event_loop` would also be late enough.
    //   The timer is preferred because it does not depend on that answer: it
    //   hangs off the backend's own `new_events` hook rather than on user-event
    //   delivery order.
    // - It schedules nothing further: a `SingleShot` timer is removed from the
    //   timer list once its callback returns (`TimerList::maybe_activate_timers`),
    //   so `duration_until_next_timer_update` is `None` again and the loop is back
    //   to `ControlFlow::Wait` with zero periodic wakeups (ADR-0001).
    slint::Timer::single_shot(Duration::ZERO, {
        let assembly = Rc::clone(&assembly);
        move || match assemble_with_loop_running(resources) {
            Ok(server) => assembly.store_server(server),
            Err(error) => {
                // There is no app without a tray. Record the cause, stop the
                // loop, and let `run` re-raise it below so the process still
                // exits non-zero with this message rather than lingering as a
                // running-but-invisible app.
                error!(error = %format!("{error:#}"), "fatal: tray assembly failed");
                assembly.record_failure(error);
                let _ = slint::quit_event_loop();
            }
        }
    });

    info!("duja tray running");
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
    /// The overlay dimmer, if it spawned.
    dimmer: Option<PlatformDimmer>,
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
/// # Errors
/// Returns an error if the tray icon or its menu cannot be created. That is fatal
/// (see [`LoopAssembly`]); everything else here degrades in place.
fn assemble_with_loop_running(resources: LoopStartResources) -> anyhow::Result<Option<PipeServer>> {
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

    // Tray icon + menu on the Slint main thread (glyph/colour shared with the
    // taskbar icons via `duja_ui::icon`), plus the update-surface handles.
    let TrayHandles {
        tray,
        menu: tray_menu,
        update_item,
    } = build_tray(accent).context("creating the tray icon")?;

    // Global hotkeys: same main-thread-with-a-running-loop requirement as the
    // tray. A failure only disables the affected binding.
    let (hotkeys, hotkey_outcomes) = init_hotkeys(&config);

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
            menu: tray_menu,
            update_item,
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

/// Open `url` in the user's default browser via `ShellExecuteW`. Best-effort:
/// a failure is logged, never fatal. Duja only ever opens the releases *page* —
/// it never downloads anything.
fn open_url(url: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};

    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated wide string that outlives the call;
    // the "open" verb (`w!`) is a static NUL-terminated literal. Passing a null
    // HWND/dir/params is valid for opening a URL. The returned HINSTANCE is a
    // legacy success/error code we do not dereference.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value > 32 on success (legacy convention).
    if result.0 as usize <= 32 {
        warn!(
            url,
            code = result.0 as usize,
            "failed to open the releases page"
        );
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

/// Best-effort OS dark-theme detection. Not trivially available through
/// winit/slint in this version, so P4 returns `None` (⇒ the flyout defaults to
/// its dark theme). Documented deviation; a real query lands with the settings
/// window.
fn os_dark_theme() -> Option<bool> {
    None
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
/// hand-back *cell* — a recorded failure survives, the root cause wins, the IPC
/// handle is taken exactly once — and nothing more. In particular these are
/// **not** covered:
///
/// - that `run()` actually calls [`LoopAssembly::take_failure`] and returns the
///   error (delete those three lines and every test still passes);
/// - that the queued closure really runs with the event loop already going, as
///   opposed to being re-inlined into the pre-loop phase — the point of the whole
///   restructure, and untestable here for the reason recorded in debt.md;
/// - the teardown *order* around [`LoopAssembly::take_server`].
///
/// All three are Windows-invisible by construction (Windows tolerates the old
/// ordering), which is exactly why this restructure landed on its own, verified
/// by the interactive smoke test rather than by a green suite.
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

    /// A clean assembly reports no failure, so `run` exits 0.
    ///
    /// The floor of the contract: if this ever returned `Some`, every successful
    /// launch would exit non-zero.
    #[test]
    fn an_assembly_that_did_not_fail_has_no_failure_to_re_raise() {
        let assembly = super::LoopAssembly::<u32>::new();
        assert!(assembly.take_failure().is_none());
    }

    /// A loop-time failure must survive the trip back to `run`.
    ///
    /// This is the pin on the regression the event-loop-first restructure could
    /// have introduced. Before it, `build_tray` ran inline in `run` and a failure
    /// was a plain `?` — the process exited non-zero with that message. Inside a
    /// queued closure there is no `?`, so the error has to be *recorded* and
    /// re-raised after the loop; a `record_failure` that dropped its argument (or
    /// a `take_failure` that always answered `None`) would compile, keep the whole
    /// suite green, and turn "the tray could not be created" into a silent
    /// exit-0 with no tray and no window — strictly worse than the crash it
    /// replaced.
    #[test]
    fn a_recorded_failure_is_handed_back_so_the_process_still_exits_non_zero() {
        let assembly = super::LoopAssembly::<u32>::new();
        assembly.record_failure(anyhow::anyhow!("failed to create the tray icon: nope"));

        let raised = assembly
            .take_failure()
            .expect("the failure must be re-raised");
        assert_eq!(
            format!("{raised}"),
            "failed to create the tray icon: nope",
            "the message `main` prints must be the one the assembly recorded"
        );
    }

    /// The first failure wins, so the reported cause is the one that stopped the
    /// assembly rather than whatever failed last on the way out.
    #[test]
    fn the_first_failure_wins_so_the_root_cause_is_what_gets_reported() {
        let assembly = super::LoopAssembly::<u32>::new();
        assembly.record_failure(anyhow::anyhow!("root cause"));
        assembly.record_failure(anyhow::anyhow!("later noise"));

        let raised = assembly.take_failure().expect("a failure was recorded");
        assert_eq!(format!("{raised}"), "root cause");
    }

    /// The IPC server handle must reach teardown, and exactly once.
    ///
    /// `run` shuts the server down first, before the engine, so losing the handle
    /// here would leak the pipe server thread past the process's own shutdown
    /// sequence; handing it out twice would double-shut-down. `store_server(None)`
    /// is a legitimate outcome (no transport ⇒ no control API), so it must not be
    /// confused with "not yet assembled".
    #[test]
    fn the_ipc_server_reaches_teardown_exactly_once() {
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

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
use tracing::{debug, error, info, warn};

use duja_app::{Engine, EngineConfig, Enumeration};
use duja_core::config::Config;
use duja_core::id::StableDisplayId;
use duja_dimmer::PlatformDimmer;
use duja_platform::Autostart;
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
/// # Errors
/// Fatal setup failures (flyout window or tray icon cannot be created, the
/// platform event pump cannot start) bubble up so `main` exits non-zero.
// RATIONALE(clippy::too_many_lines): `run` is the app-assembly entry point — a
// single linear sequence of one-time wiring (paths, instance/installer guards,
// crash recovery, HDR verdict, windows, engine, IPC, handlers) before the event
// loop. The tray.rs module split has since happened, and it deliberately did NOT
// shrink this function: the split moved the *bodies* out (`build_tray`,
// `init_hotkeys`, `wire_event_sources`, …) and left the ordering — which is the
// load-bearing part — visible in one place. Most of what remains is one-time
// acts on distinct resources; the exception is the ~30-line `AppState` literal,
// which is field assembly rather than a step, but extracting it would still
// leave this well over the 100-line threshold while splitting the struct's
// construction from the resource acquisition that feeds it. A further
// extraction purely to satisfy the line count would scatter the ordering across
// helpers that each run exactly once, and hide the startup sequence the
// degradation story depends on.
#[allow(clippy::too_many_lines)]
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

    // 5. Tray icon + menu on the same thread (glyph/colour shared with the
    //    taskbar icons via `duja_ui::icon`), plus the update-surface handles.
    let TrayHandles {
        tray,
        menu: tray_menu,
        update_item,
    } = build_tray(accent).context("creating the tray icon")?;

    // 6. Async pipeline: engine (with a bounds-updating enumerator) + event pump
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

    // 7. Publish the shared state and wire every event source. The gamma channel
    //    correlates a resolved display id to its GDI device via the same bounds
    //    map the overlay planner reads (DDC displays — external monitors and a
    //    DDC-fallback internal panel — carry a device name; WMI panels do not, so
    //    gamma never targets those).
    let gamma = gamma::GammaBackend::new(paths.crash_marker.clone(), {
        let bounds = bounds.clone();
        move |id| bounds.lock().ok().and_then(|b| b.device_for(id))
    });
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
            // The verdict above was just probed; start the TTL clock from now so
            // the first enumeration does not immediately re-probe.
            last_gamma_probe: Some(Instant::now()),
            bounds,
            state: StateStore::load(paths.state.clone()),
            crash_marker: paths.crash_marker.clone(),
            engine_tx: engine.sender(),
            levels: LevelForwarder::new(EngineLevelSink::new(engine.sender())),
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
    let ipc_server = ipc::start(std::sync::Arc::new(ipc::TrayBridge::new(engine.sender())));

    info!("duja tray running");
    let loop_result = slint::run_event_loop_until_quit();
    if let Err(e) = loop_result {
        error!(error = %e, "event loop exited with an error");
    }

    // 8. Clean teardown (state was flushed on Quit; this joins the threads).
    if let Some(server) = ipc_server {
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
    Ok(ExitCode::SUCCESS)
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
}

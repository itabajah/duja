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
//! `SetUserLevel`; the overlay-alpha channel goes to the [`Dimmer`]; and the
//! opt-in gamma channel goes to [`gamma::GammaBackend`], which owns the
//! persistent-ramp crash marker. The engine is kept dimmer-agnostic — the
//! notification loop here drives the dimmer and the gamma channel.
//!
//! # Degradation
//!
//! Everything that can fail in a headless/disconnected session (flyout window,
//! tray icon, overlay dimmer) is handled: the flyout/tray are fatal (logged,
//! non-zero exit — there is no app without them), while a missing dimmer only
//! disables software dimming (hardware brightness still works).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use crossbeam_channel::Sender;
use global_hotkey::hotkey::{Code, HotKey, Modifiers as GhkModifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tracing::{debug, error, info, warn};

use duja_app::{Engine, EngineCommand, EngineConfig, EngineNotification, Enumeration};
use duja_core::config::Config;
use duja_core::continuum::map_user_level;
use duja_core::dimmer::{DimCommand, Dimmer};
use duja_core::id::StableDisplayId;
use duja_core::manager::DEFAULT_USER_LEVEL_PCT;
use duja_core::model::{DimMode, DisplayKind, DisplaySnapshot};
use duja_dimmer::PlatformDimmer;
use duja_platform::Autostart;
use duja_ui::{
    FlyoutShell, FlyoutVm, HotkeyRow, SettingsCommand, SettingsShell, SettingsVm, ThemeChoice,
    UiCommand, UpdateStatus,
};

use crate::bin_support::bounds::BoundsMap;
use crate::bin_support::dimming::{self, DisplayInput};
use crate::bin_support::hotkey::{self, Accelerator, HotkeyAction, Modifiers as AccelModifiers};
use crate::bin_support::paths::DujaPaths;
use crate::bin_support::state_store::StateStore;
use crate::bin_support::updates::{self, HttpsTransport, UpdateOutcome};
use crate::bin_support::{backend, gamma, ipc, run, settings, settings_apply, startup};

/// The brightness step (percentage points) a `brightness_up` / `brightness_down`
/// hotkey applies to every display. Fixed in P5; a configurable step is a
/// settings-UI follow-up.
const HOTKEY_BRIGHTNESS_STEP: i16 = 5;

/// The hardware floor (%) the flyout's "Software dimming" toggle establishes when
/// it is switched on for a display that has none set.
///
/// Software dimming (an overlay/gamma sub-floor) only has a region to act in when
/// there is a non-zero hardware floor — below it the hardware is pinned and the
/// overlay takes over. The schema default is `hw_floor_pct = 0` (ADR-0007: "no
/// artificial floor until the user sets one"), so a fresh display's toggle would
/// otherwise be a visual no-op. Turning the toggle *on* is the user opting into
/// that sub-floor regime, so we seed a sensible floor here; the Settings window's
/// per-monitor slider then fine-tunes it, and setting the floor back to 0 there
/// restores the full hardware range. Turning the toggle off leaves the floor
/// (the slider still bottoms out at it, communicating the range) — it is
/// re-tunable in Settings.
const DEFAULT_SOFTWARE_DIM_FLOOR_PCT: u8 = 20;

/// How long after the flyout is hidden a tray-icon click is treated as the same
/// dismissing gesture (rather than a fresh open), closing the click-outside race.
const TOGGLE_GUARD: Duration = Duration::from_millis(200);

/// What a tray-icon click resolves to, given flyout visibility + recency of hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToggleDecision {
    /// Open the flyout.
    Show,
    /// Hide the visible flyout.
    Hide,
    /// Swallow the click (it is the tail of the gesture that just dismissed the
    /// flyout via focus-loss; re-opening would fight the user).
    Ignore,
}

/// The floor to seed when the flyout "Software dimming" toggle is switched on,
/// given the display's current floor.
///
/// Returns [`DEFAULT_SOFTWARE_DIM_FLOOR_PCT`] only when dimming is being turned
/// *on* for a display with no floor (`0`) — enabling the toggle is the user
/// opting into a sub-floor regime, which needs a non-zero floor to be visible.
/// Turning it off, or a display that already has a floor, seeds nothing.
fn floor_to_seed_on_dimming(on: bool, current_floor: u8) -> Option<u8> {
    (on && current_floor == 0).then_some(DEFAULT_SOFTWARE_DIM_FLOOR_PCT)
}

/// The user level to seed for a freshly-sighted display the user has not yet taken
/// control of, choosing between the live hardware reading and the persisted value.
///
/// Item 5 — Duja adopts the monitor's CURRENT brightness on launch. This prefers
/// the hardware reading (`reading_pct`, from the engine's initial Get) so the
/// slider mirrors reality and nothing moves. It falls back to the persisted
/// last-known only when the reading is still the pre-probe placeholder
/// ([`DEFAULT_USER_LEVEL_PCT`] — before the Get lands, or when it fails) and a
/// persisted value exists: the documented fallback for a failed hardware read.
/// It never itself writes to hardware — adoption reflects reality, it does not
/// restore a saved level.
fn level_to_adopt(reading_pct: u8, persisted: Option<u8>) -> u8 {
    match persisted {
        Some(saved) if reading_pct == DEFAULT_USER_LEVEL_PCT => saved,
        _ => reading_pct,
    }
}

/// Adopt a fresh enumeration into the state book: record each display's adopted
/// user level (see [`level_to_adopt`]) for every display the user has **not** taken
/// control of this session.
///
/// This is the startup/hot-plug "adopt reality" step (item 5). It has **no engine
/// channel by construction** — adoption records the level for the UI and persists
/// it, but pushes NOTHING to the hardware, so a launch can never move the
/// brightness. (The pre-fix code sent an `EngineCommand::SetUserLevel` for the
/// persisted level here, which dimmed the monitor to the last-saved level on every
/// launch.) A user-controlled display is skipped so a late enumeration echo cannot
/// overwrite the user's chosen value.
fn adopt_enumeration(
    snapshots: &[DisplaySnapshot],
    user_controlled: &BTreeSet<String>,
    state: &mut StateStore,
    now_unix: i64,
) {
    for snap in snapshots {
        if user_controlled.contains(snap.id.as_str()) {
            continue;
        }
        let level = level_to_adopt(snap.user_level_pct, state.level(snap.id.as_str()));
        state.record(snap.id.as_str(), level, now_unix);
    }
}

/// Decide what a tray-icon click should do.
///
/// A visible flyout hides. An already-hidden flyout normally re-opens — *unless*
/// it was hidden within [`TOGGLE_GUARD`] of this click, which means focus-loss
/// dismissal already fired for this same click; then the click is swallowed so
/// the flyout does not immediately re-open (P0 live-QA bug 5 follow-up: clicking
/// the tray icon while the flyout is open toggles it closed, not re-open).
fn toggle_decision(
    visible: bool,
    since_hidden: Option<Duration>,
    guard: Duration,
) -> ToggleDecision {
    if visible {
        ToggleDecision::Hide
    } else if since_hidden.is_some_and(|elapsed| elapsed < guard) {
        ToggleDecision::Ignore
    } else {
        ToggleDecision::Show
    }
}

mod geometry;
mod icon;

/// The flyout's fixed logical width (matches `flyout.slint`).
const FLYOUT_LOGICAL_WIDTH: f32 = 320.0;
/// The settings window's fixed logical design size (matches `settings.slint`).
const SETTINGS_LOGICAL_WIDTH: f32 = 440.0;
const SETTINGS_LOGICAL_HEIGHT: f32 = 600.0;
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
/// One deferred unit of work queued by a re-entrant [`ReentrantCell::with`].
type Deferred<T> = Box<dyn FnOnce(&mut T)>;

struct ReentrantCell<T> {
    slot: RefCell<Option<T>>,
    busy: Cell<bool>,
    queue: RefCell<VecDeque<Deferred<T>>>,
}

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
    /// Begin a clean shutdown.
    Quit,
}

/// Run the tray application. Returns the process exit code.
///
/// # Errors
/// Fatal setup failures (flyout window or tray icon cannot be created, the
/// platform event pump cannot start) bubble up so `main` exits non-zero.
pub(crate) fn run(verbose: bool) -> anyhow::Result<ExitCode> {
    let _ = verbose; // logging is initialised by the caller.
    let paths = DujaPaths::resolve().unwrap_or_else(fallback_paths);

    // 1. Single-instance guard: a second launch asks the running instance to
    //    surface its flyout over IPC, then exits 0.
    let instance = duja_platform::SingleInstance::acquire();
    if instance.already_running() {
        if ipc::show_running_instance() {
            info!("another duja instance is running; asked it to show its flyout");
        } else {
            info!("another duja instance is already running; exiting");
        }
        return Ok(ExitCode::SUCCESS);
    }

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

    // 4. Flyout window FIRST (icon-first: the UI must exist or there is no app).
    let vm = Rc::new(RefCell::new(FlyoutVm::new()));
    vm.borrow_mut().set_theme(theme);
    let shell = FlyoutShell::new(vm.clone())
        .map_err(|e| anyhow::anyhow!("failed to create the flyout window: {e}"))?;

    // 4b. Settings window + autostart backend (window stays hidden until opened).
    let (settings_shell, settings_vm, autostart) = build_settings_window()?;

    // 5. Tray icon + menu on the same thread.
    let tray = build_tray().context("creating the tray icon")?;

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
    //    map the overlay planner reads (external displays carry a device name;
    //    panels do not, and gamma never targets them).
    let gamma = gamma::GammaBackend::new(paths.crash_marker.clone(), {
        let bounds = bounds.clone();
        move |id| bounds.lock().ok().and_then(|b| b.device_for(id))
    });
    // Global hotkeys: register from config on this (main) thread. A failure to
    // create the manager or register a binding only disables that hotkey — the
    // app runs on without it. The registrar is kept in the app state so the
    // settings window can rebind and re-register it live.
    let mut hotkeys = OsHotkeyRegistrar::new();
    let initial_plan = hotkey::resolve(&config.hotkeys);
    log_hotkey_issues(&initial_plan);
    let hotkey_outcomes = outcomes_by_action(&hotkey::apply_plan(&mut hotkeys, &initial_plan));
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
            bounds,
            state: StateStore::load(paths.state.clone()),
            crash_marker: paths.crash_marker.clone(),
            engine_tx: engine.sender(),
            gamma,
            displays: Vec::new(),
            user_controlled: BTreeSet::new(),
            flyout_visible: false,
            last_hidden: None,
            hotkeys,
            hotkey_outcomes,
        }));
    });
    wire_ui_commands();
    wire_settings_commands();
    wire_tray_handlers();
    install_hotkey_event_handler();
    spawn_notification_bridge(notifications);

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
        // Dropping the AppState clears overlays via the dimmer's own teardown.
        cell.set(None);
    });
    drop(tray);
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

/// Create the settings window shell + view-model and resolve the platform
/// autostart backend.
///
/// # Errors
/// Returns an error if the settings window cannot be created (fatal, like the
/// flyout). An autostart resolve failure is *not* fatal — it only disables the
/// launch-at-login toggle.
fn build_settings_window() -> anyhow::Result<SettingsSetup> {
    let settings_vm = Rc::new(RefCell::new(SettingsVm::new()));
    let settings_shell = SettingsShell::new(settings_vm.clone())
        .map_err(|e| anyhow::anyhow!("failed to create the settings window: {e}"))?;
    let autostart: Option<Box<dyn Autostart>> = match duja_platform::autostart::system() {
        Ok(a) => Some(Box::new(a)),
        Err(e) => {
            warn!(error = %e, "autostart unavailable; the launch-at-login toggle is disabled");
            None
        }
    };
    Ok((settings_shell, settings_vm, autostart))
}

/// The main-thread application state driven by every event source.
struct AppState {
    shell: FlyoutShell,
    vm: Rc<RefCell<FlyoutVm>>,
    /// The settings window shell and its shared view-model.
    settings_shell: SettingsShell,
    settings_vm: Rc<RefCell<SettingsVm>>,
    /// The platform launch-at-login backend (`None` if unavailable — the toggle
    /// is then shown disabled).
    autostart: Option<Box<dyn Autostart>>,
    /// The user-facing config file, for format-preserving settings writes.
    config_path: std::path::PathBuf,
    /// The most recent full snapshots (with capabilities), for the settings
    /// per-monitor sections.
    snapshots: Vec<DisplaySnapshot>,
    dimmer: Option<PlatformDimmer>,
    config: Config,
    gamma_allowed: bool,
    bounds: Arc<Mutex<BoundsMap>>,
    state: StateStore,
    crash_marker: std::path::PathBuf,
    engine_tx: Sender<EngineCommand>,
    /// The opt-in gamma sub-floor channel (RAII crash-marker owner + engage/
    /// restore executor). Drives [`DimCommand`]s carrying a gamma factor to the
    /// GPU ramp; identity-restored on quit/restore.
    gamma: gamma::GammaBackend,
    /// The current display set (resolved id + class) from the last enumeration.
    displays: Vec<(StableDisplayId, DisplayKind)>,
    /// Displays the user has explicitly driven this session (slider / hotkey /
    /// IPC). Until a display is in this set, Duja only *adopts* its current
    /// hardware brightness (mirrors it into the UI, writes nothing — item 5); once
    /// the user acts it becomes authoritative, so a later enumeration echo never
    /// clobbers the user's value, and its overlay/gamma may engage.
    user_controlled: BTreeSet<String>,
    flyout_visible: bool,
    /// When the flyout was last hidden, for the tray-click toggle guard
    /// ([`toggle_decision`]).
    last_hidden: Option<Instant>,
    /// The live global-hotkey registrar (OS manager + id→action map), re-applied
    /// whenever the hotkey config changes.
    hotkeys: OsHotkeyRegistrar,
    /// The last live-registration result per action, for settings-row feedback
    /// (conflict / OS-rejected).
    hotkey_outcomes: BTreeMap<HotkeyAction, hotkey::RegisterResult>,
}

impl AppState {
    /// Apply a tray/menu action.
    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Open => self.show_flyout(),
            Action::Toggle => {
                let since_hidden = self.last_hidden.map(|hidden| hidden.elapsed());
                match toggle_decision(self.flyout_visible, since_hidden, TOGGLE_GUARD) {
                    ToggleDecision::Hide => self.hide_flyout(),
                    ToggleDecision::Show => self.show_flyout(),
                    ToggleDecision::Ignore => {}
                }
            }
            Action::OpenSettings => self.open_settings(),
            Action::Restore => self.restore_screen(),
            Action::Nudge(delta) => self.nudge_all(delta),
            Action::Quit => self.begin_quit(),
        }
    }

    /// Adjust every known display's brightness by `delta` percentage points
    /// (clamped 0..=100), routing each change through the same user-level path
    /// the flyout slider uses so state, engine and overlays stay consistent.
    fn nudge_all(&mut self, delta: i16) {
        let ids: Vec<StableDisplayId> = self.displays.iter().map(|(id, _)| id.clone()).collect();
        for id in ids {
            let current = i16::from(self.state.level(id.as_str()).unwrap_or(100));
            let next = current.saturating_add(delta).clamp(0, 100);
            let pct = u8::try_from(next).unwrap_or(0);
            self.set_user_level(&id, pct);
        }
    }

    /// Show the flyout anchored near the tray/cursor, in one shot.
    ///
    /// The window is sized *and* anchored in physical pixels **while still hidden**
    /// — using the target monitor's OS-queried scale (Per-Monitor-V2; Win32 rects
    /// are physical) — then shown exactly once, with no resize or move afterwards.
    /// A post-show resize (the former buffer re-assert) made the software renderer
    /// occasionally present a partial/transparent first frame that only repaired on
    /// a later click (item 1); presenting a correctly-sized, correctly-placed window
    /// in one shot removes that race. The anchor still uses the true physical size
    /// so the window lands flush against the tray edge at any scale (P0 live-QA bug
    /// 4); Slint sizes the buffer natively for the monitor it is shown on (PR #29).
    fn show_flyout(&mut self) {
        use crate::bin_support::positioning::{flyout_origin, physical_window_size};
        // Drive the window height from the row count (a no-frame window is not
        // auto-grown to its content preferred size). Logical px — Slint scales it.
        let logical_height = self.flyout_logical_height();
        self.shell.set_content_height(logical_height);

        let (cursor, work, scale) = geometry::cursor_work_area_and_scale();
        let physical = physical_window_size(FLYOUT_LOGICAL_WIDTH, logical_height, scale);
        let (x, y) = flyout_origin(cursor, work, physical, FLYOUT_MARGIN);
        self.shell
            .present_at(FLYOUT_LOGICAL_WIDTH, logical_height, x, y);
        self.flyout_visible = true;

        // Keep the flyout above other windows while visible and focus it so
        // Esc/keyboard work immediately (user-reported: it opened underneath).
        // This never resizes/moves the window; its redraw request just forces the
        // first presented frame to be complete.
        self.shell.surface(true);
    }

    /// The flyout window's content-derived logical height.
    ///
    /// A no-frame window is not auto-sized to its preferred height, so this
    /// mirrors the `.slint` layout arithmetic (chrome + one card per row) to size
    /// it. Approximate by design — a few pixels of slack sit at the bottom.
    fn flyout_logical_height(&self) -> f32 {
        const CHROME: f32 = 118.0; // padding + header + footer + inter-section gaps
        const CARD: f32 = 132.0; // one monitor card
        const CARD_GAP: f32 = 8.0;
        let rows = self.vm.borrow().rows().len();
        let body = if rows == 0 {
            100.0 // empty-state panel
        } else {
            let n = f32::from(u16::try_from(rows).unwrap_or(u16::MAX));
            n * CARD + (n - 1.0) * CARD_GAP
        };
        (CHROME + body).clamp(160.0, 620.0)
    }

    /// Hide the flyout (process keeps running in the tray).
    fn hide_flyout(&mut self) {
        self.shell.hide();
        self.flyout_visible = false;
        self.last_hidden = Some(Instant::now());
    }

    /// Dismiss the flyout when it loses focus (the user clicked outside it).
    ///
    /// Routed through the app so [`flyout_visible`](Self::flyout_visible) is kept
    /// in sync — the next tray click then re-opens it (P0 live-QA bug 5).
    fn on_focus_lost(&mut self) {
        if self.flyout_visible {
            self.hide_flyout();
        }
    }

    /// Restore the screen: clear overlays and reset identity gamma everywhere.
    fn restore_screen(&mut self) {
        if let Some(dimmer) = self.dimmer.as_mut()
            && let Err(e) = dimmer.clear()
        {
            warn!(error = %e, "failed to clear overlays");
        }
        // Restore the displays this session engaged (clearing the crash marker),
        // then a belt-and-suspenders global identity pass for anything left over
        // from a prior dirty run.
        self.gamma.restore_all();
        let report = duja_dimmer::restore_all();
        info!(
            restored = report.restored.len(),
            failed = report.failed.len(),
            "restored screen on request"
        );
    }

    /// Clean shutdown: persist state, restore gamma (clearing the marker), quit
    /// the event loop.
    fn begin_quit(&mut self) {
        let _ = self.state.flush(Instant::now());
        // The gamma guard restores every engaged display and clears the crash
        // marker; the explicit remove is a redundant safety net.
        self.gamma.restore_all();
        let _ = std::fs::remove_file(&self.crash_marker);
        if let Some(dimmer) = self.dimmer.as_mut() {
            let _ = dimmer.clear();
        }
        if let Err(e) = slint::quit_event_loop() {
            error!(error = %e, "failed to signal event-loop quit");
        }
    }

    /// Handle a UI command emitted by the flyout view-model.
    fn on_ui_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::SetLevel { id, pct } => self.set_user_level(&id, pct),
            UiCommand::Refresh => {
                let _ = self.engine_tx.send(EngineCommand::RefreshNow);
            }
            UiCommand::OpenSettings => self.open_settings(),
            UiCommand::SetDimmingEnabled { id, on } => self.set_dimming_enabled(&id, on),
        }
    }

    /// Apply a flyout dimming toggle: persist the display's dim mode (overlay when
    /// on, off when off), re-plan its dimmer batch, and refresh both windows.
    ///
    /// Routed through the same config-write + re-apply path a settings dim-mode
    /// change uses, so the flyout toggle and the settings picker stay consistent.
    fn set_dimming_enabled(&mut self, id: &StableDisplayId, on: bool) {
        // Turning dimming on for a display with no floor set would have no visible
        // effect (no sub-floor region), so seed a sensible floor first — the user
        // is opting into the sub-floor regime by enabling the toggle. See
        // [`DEFAULT_SOFTWARE_DIM_FLOOR_PCT`] / [`floor_to_seed_on_dimming`].
        let current_floor = settings::monitor_config(&self.config, id.as_str()).hw_floor_pct;
        if let Some(pct) = floor_to_seed_on_dimming(on, current_floor) {
            let floor = SettingsCommand::SetMonitorFloor {
                id: id.clone(),
                pct,
            };
            match settings_apply::persist_config_change(&self.config_path, &floor) {
                Ok(true) => self.reload_config(),
                Ok(false) => {}
                Err(e) => warn!(error = %e, "failed to seed a software-dimming floor"),
            }
        }

        let mode = if on { DimMode::Overlay } else { DimMode::Off };
        let command = SettingsCommand::SetMonitorDimMode {
            id: id.clone(),
            mode,
        };
        match settings_apply::persist_config_change(&self.config_path, &command) {
            Ok(true) => self.reload_config(),
            Ok(false) => {}
            Err(e) => warn!(error = %e, "failed to persist dimming toggle"),
        }
        self.reapply_display(id);
        self.refresh_flyout_dimming();
        self.render();
        // Keep the settings per-monitor picker in sync if it is open.
        self.settings_vm.borrow_mut().set_displays(
            &self.snapshots,
            &self.config,
            self.gamma_allowed,
        );
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());
    }

    /// Rebuild the flyout's per-display dimming info (floor + on/off) from the
    /// current config and push it into the flyout view-model.
    fn refresh_flyout_dimming(&self) {
        use duja_core::config::DimMode as ConfigDimMode;
        let info: BTreeMap<StableDisplayId, duja_ui::DimmingInfo> = self
            .displays
            .iter()
            .map(|(id, kind)| {
                let monitor = settings::monitor_config(&self.config, id.as_str());
                let cfg = settings::continuum_for(*kind, &monitor, self.gamma_allowed);
                (
                    id.clone(),
                    duja_ui::DimmingInfo {
                        hardware_floor: cfg.hardware_floor,
                        // Reflect the *configured* mode (not the HDR-guarded one)
                        // so the toggle shows what the user chose.
                        dimming_on: monitor.dim_mode != ConfigDimMode::Off,
                    },
                )
            })
            .collect();
        self.vm.borrow_mut().set_dimming_info(info);
    }

    /// Resolve a fired global hotkey id to its action and apply it.
    fn on_hotkey_fired(&mut self, id: u32) {
        if let Some(action) = self.hotkeys.action_for_id(id) {
            self.handle_action(action_for(action));
        }
    }

    /// Re-resolve the hotkey config and re-register live, updating the
    /// settings-row feedback (conflict / OS-rejected) and re-rendering.
    fn reregister_hotkeys(&mut self) {
        let plan = hotkey::resolve(&self.config.hotkeys);
        log_hotkey_issues(&plan);
        let outcomes = hotkey::apply_plan(&mut self.hotkeys, &plan);
        self.hotkey_outcomes = outcomes_by_action(&outcomes);
        let rows = resolved_hotkey_rows(&self.config, &self.hotkey_outcomes);
        self.settings_vm.borrow_mut().set_hotkeys(rows);
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());
    }

    /// Rebuild the settings view-model from live state and show the window, in one
    /// shot (same partial-first-paint fix as the flyout — item 1).
    fn open_settings(&mut self) {
        use crate::bin_support::positioning::{center_in, physical_window_size};
        self.rebuild_settings();
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());
        // Drive the content height (logical); Slint clamps it to the window's
        // min/max.
        self.settings_shell
            .set_content_height(SETTINGS_LOGICAL_HEIGHT);

        // Size + centre the window on the active monitor in physical pixels while
        // still hidden (using the monitor's OS-queried scale), then show once — no
        // post-show resize/move. Centring on the active monitor also avoids the OS
        // default cascade position (P0 live-QA bug 4).
        let (_cursor, work, scale) = geometry::cursor_work_area_and_scale();
        let physical = physical_window_size(SETTINGS_LOGICAL_WIDTH, SETTINGS_LOGICAL_HEIGHT, scale);
        let (x, y) = center_in(work, physical);
        self.settings_shell
            .present_at(SETTINGS_LOGICAL_WIDTH, SETTINGS_LOGICAL_HEIGHT, x, y);
        // Bring settings to the foreground (normal level, not topmost).
        self.settings_shell.focus();
    }

    /// Refresh the settings view-model from the current config, snapshots,
    /// autostart state, and hotkey table. Does not touch the window.
    fn rebuild_settings(&mut self) {
        let autostart_supported = self.autostart.is_some();
        let autostart_on = self
            .autostart
            .as_ref()
            .and_then(|a| a.is_enabled().ok())
            .unwrap_or(false);
        let theme = settings_apply::theme_to_choice(self.config.general.theme);
        let dark = self.resolved_dark();
        let update_check_on = self.config.general.update_check;

        let hotkeys = resolved_hotkey_rows(&self.config, &self.hotkey_outcomes);
        {
            let mut vm = self.settings_vm.borrow_mut();
            vm.set_general(
                autostart_on,
                autostart_supported,
                theme,
                update_check_on,
                dark,
            );
            vm.set_displays(&self.snapshots, &self.config, self.gamma_allowed);
            vm.set_hotkeys(hotkeys);
        }
    }

    /// Handle a command emitted by the settings view-model.
    fn on_settings_command(&mut self, command: SettingsCommand) {
        // Guard: never persist a hotkey binding the parser would reject (an
        // exotic key the .slint let through). The recorder just yields nothing.
        if let SettingsCommand::SetHotkey { binding, .. } = &command
            && Accelerator::parse(binding).is_err()
        {
            warn!(binding = %binding, "ignoring unparseable hotkey binding");
            return;
        }

        // 1. Persist the config-affecting part (format-preserving), then reload
        //    the typed config so in-memory state matches disk.
        match settings_apply::persist_config_change(&self.config_path, &command) {
            Ok(true) => self.reload_config(),
            Ok(false) => {}
            Err(e) => warn!(error = %e, "failed to persist settings change"),
        }

        // 2. Apply the live side effect.
        match command {
            SettingsCommand::SetAutostart(on) => self.apply_autostart(on),
            SettingsCommand::SetTheme(choice) => self.apply_theme(choice),
            SettingsCommand::SetUpdateCheck(_) => {
                // Config-only; the VM already reflects the toggle.
            }
            SettingsCommand::CheckUpdates => self.start_update_check(),
            SettingsCommand::OpenReleasesPage => open_url(updates::RELEASES_PAGE_URL),
            SettingsCommand::SetMonitorFloor { id, .. }
            | SettingsCommand::SetMonitorDimMode { id, .. } => {
                self.reapply_display(&id);
                self.refresh_flyout_dimming();
                self.render();
            }
            SettingsCommand::SetInput { id, value } => {
                let _ = self.engine_tx.send(EngineCommand::SetInput { id, value });
            }
            SettingsCommand::SetHotkey { .. } | SettingsCommand::ClearHotkey { .. } => {
                self.reregister_hotkeys();
            }
        }

        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());
    }

    /// Reload the typed config from disk after a settings write.
    fn reload_config(&mut self) {
        use duja_core::config::ConfigDocument;
        match ConfigDocument::load(&self.config_path).and_then(|doc| doc.config()) {
            Ok(config) => self.config = config,
            Err(e) => {
                warn!(error = %e, "config reload after settings write failed; keeping in-memory copy");
            }
        }
    }

    /// Apply a launch-at-login change through the platform trait, keeping the
    /// view-model honest with the actual state on failure.
    fn apply_autostart(&mut self, on: bool) {
        let Some(autostart) = self.autostart.as_mut() else {
            return;
        };
        if let Err(e) = autostart.set_enabled(on) {
            warn!(error = %e, "failed to change launch-at-login");
        }
        // Reflect the actual state (which may differ from the request on error).
        let actual = autostart.is_enabled().unwrap_or(on);
        let supported = true;
        let theme = settings_apply::theme_to_choice(self.config.general.theme);
        // `autostart`'s &mut borrow ends above (last used for `actual`), so the
        // whole-`self` `resolved_dark` call is free of a borrow conflict here.
        let dark = self.resolved_dark();
        self.settings_vm.borrow_mut().set_general(
            actual,
            supported,
            theme,
            self.config.general.update_check,
            dark,
        );
    }

    /// Re-resolve the flyout palette after a theme change and re-render it. Also
    /// refreshes the settings view-model so its window follows the same palette
    /// (the caller re-renders the settings shell after this returns).
    fn apply_theme(&mut self, _choice: ThemeChoice) {
        let theme = settings::ui_theme(self.config.general.theme, os_dark_theme());
        self.vm.borrow_mut().set_theme(theme);
        self.rebuild_settings();
        self.render();
    }

    /// The resolved palette (`true` = dark) for the current theme preference — the
    /// same resolution the flyout uses (`settings::ui_theme`), so the settings
    /// window renders the identical light/dark palette rather than a fixed one.
    fn resolved_dark(&self) -> bool {
        matches!(
            settings::ui_theme(self.config.general.theme, os_dark_theme()),
            duja_ui::Theme::Dark
        )
    }

    /// Re-apply a display's dimming after a floor/dim-mode change by re-driving
    /// its current user level through the normal path (recomputes the hardware
    /// target against the new continuum and re-plans overlays/gamma).
    fn reapply_display(&mut self, id: &StableDisplayId) {
        let level = self.state.level(id.as_str()).unwrap_or(100);
        self.set_user_level(id, level);
    }

    /// Kick off the opt-in update check on a background thread (never blocks the
    /// UI thread), then fold the result back onto the Slint loop.
    fn start_update_check(&mut self) {
        self.state.record_update_check(unix_now());
        let _ = self.state.maybe_flush(Instant::now());
        std::thread::Builder::new()
            .name("duja-update-check".to_owned())
            .spawn(|| {
                let outcome = updates::check_for_update(&HttpsTransport, env!("CARGO_PKG_VERSION"));
                let status = update_status_from(outcome);
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(move |app| {
                        app.settings_vm.borrow_mut().set_update_status(status);
                        app.settings_shell.update_from_vm(&app.settings_vm.borrow());
                    });
                });
            })
            .ok();
    }

    /// Record a user level, forward the hardware write to the engine, and
    /// re-apply the overlay batch.
    ///
    /// Every `SetUserLevel` is forwarded — there is no UI-side throttle. The
    /// engine worker enforces `write_min_gap` with last-wins coalescing, which
    /// bounds the hardware write rate *and* guarantees the final value of a drag
    /// lands (see P4 gate Finding 1: a leading-edge UI throttle used to drop the
    /// final sample, leaving the hardware at an intermediate level).
    fn set_user_level(&mut self, id: &StableDisplayId, pct: u8) {
        let now = Instant::now();
        // A genuine user action: this display is now user-controlled, so it writes
        // to hardware here and its overlay/gamma may engage — and a later
        // enumeration will not re-adopt (clobber) this level (item 5).
        self.user_controlled.insert(id.as_str().to_owned());
        self.state.record(id.as_str(), pct, unix_now());

        if let Some(kind) = self.kind_of(id) {
            let hw = self.hardware_target(kind, id.as_str(), pct);
            let _ = self.engine_tx.send(EngineCommand::SetUserLevel {
                id: id.clone(),
                pct: hw,
            });
        }
        self.apply_overlays();
        let _ = self.state.maybe_flush(now);
    }

    /// Handle an engine notification (runs on the Slint thread).
    fn on_notification(&mut self, notification: EngineNotification) {
        match notification {
            EngineNotification::DisplaysChanged(snapshots) => self.on_displays_changed(snapshots),
            EngineNotification::DisplayUnresponsive(id) => {
                self.vm.borrow_mut().set_unresponsive(&id, true);
                self.render();
            }
            EngineNotification::DisplayResponsive(id) => {
                self.vm.borrow_mut().set_unresponsive(&id, false);
                self.render();
            }
        }
    }

    /// Adopt a fresh enumeration: mirror each display's CURRENT hardware brightness
    /// into the UI (writing NOTHING to the hardware — item 5), rebuild the flyout
    /// rows against *user* levels, and re-apply overlays for user-controlled
    /// displays.
    ///
    /// A launch (or hot-plug) must never move the brightness: Duja adopts what the
    /// monitor is actually at (`snap.user_level_pct`, from the engine's initial
    /// Get), not the persisted file, and pushes no `SetUserLevel`. Persisted state
    /// only seeds the UI as a fallback while that reading is still the pre-probe
    /// placeholder (see [`level_to_adopt`]). Only a genuine user action
    /// ([`set_user_level`](Self::set_user_level)) writes to hardware thereafter.
    fn on_displays_changed(&mut self, snapshots: Vec<DisplaySnapshot>) {
        self.displays = snapshots.iter().map(|s| (s.id.clone(), s.kind)).collect();
        // Keep the full snapshots (with capabilities) for the settings sections,
        // and refresh the (possibly-open) settings window's per-monitor list.
        self.snapshots.clone_from(&snapshots);
        self.settings_vm.borrow_mut().set_displays(
            &self.snapshots,
            &self.config,
            self.gamma_allowed,
        );
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());

        let now = Instant::now();
        // Adopt reality: seed the state book from each display's live hardware
        // reading (persisted last-known only as a placeholder fallback), for the
        // displays the user has not taken control of. This writes NOTHING to the
        // hardware — the former `SetUserLevel` push here is what dimmed the monitor
        // to the last-saved level on every launch (item 5).
        adopt_enumeration(
            &snapshots,
            &self.user_controlled,
            &mut self.state,
            unix_now(),
        );

        // Rebuild the flyout rows showing the *user* levels (not the engine's
        // hardware echo).
        let rows: Vec<DisplaySnapshot> = snapshots
            .into_iter()
            .map(|mut s| {
                s.user_level_pct = self.state.level(s.id.as_str()).unwrap_or(s.user_level_pct);
                s
            })
            .collect();
        self.vm.borrow_mut().set_displays(rows);
        self.refresh_flyout_dimming();
        self.render();
        // Keep the content-driven height current if the flyout is open (the row
        // count may have changed), re-asserting the logical size so the buffer
        // tracks it.
        if self.flyout_visible {
            let logical_height = self.flyout_logical_height();
            self.shell.set_content_height(logical_height);
            self.shell
                .enforce_logical_size(FLYOUT_LOGICAL_WIDTH, logical_height);
        }
        self.apply_overlays();
        let _ = self.state.maybe_flush(now);
    }

    /// Push the current view-model state into the Slint component.
    fn render(&self) {
        self.shell.update_from_vm(&self.vm.borrow());
    }

    /// Compute and apply the full overlay batch for every known display, then
    /// drive the gamma channel for any command carrying a gamma factor.
    ///
    /// Overlays and gamma are the two halves of one declarative batch: the
    /// overlay backend diffs the alpha channel, while [`gamma::GammaBackend`]
    /// engages/restores the GPU ramp for the (opt-in, SDR-only) gamma channel.
    /// HDR/unknown displays never carry a gamma factor here — `effective_mode`
    /// forces them onto the overlay path — so they can never reach the ramp.
    fn apply_overlays(&mut self) {
        let commands = self.plan_commands();
        if let Some(dimmer) = self.dimmer.as_mut()
            && let Err(e) = dimmer.apply(&commands)
        {
            warn!(error = %e, "overlay apply failed");
        }
        self.gamma.apply(&commands);
    }

    /// Build the declarative overlay command batch (pure; borrows `&self`).
    ///
    /// Only displays the user has taken control of this session get an overlay/
    /// gamma command; an untouched display is left at reality (no dimming) — Duja
    /// never restores an overlay/gamma on launch, it adopts the current screen
    /// (item 5). The batch is a diff, so an absent display is simply not dimmed.
    fn plan_commands(&self) -> Vec<DimCommand> {
        let inputs: Vec<DisplayInput> = self
            .displays
            .iter()
            .filter(|(id, _)| self.user_controlled.contains(id.as_str()))
            .map(|(id, kind)| DisplayInput {
                id: id.clone(),
                kind: *kind,
                user_pct: self.state.level(id.as_str()).unwrap_or(100),
            })
            .collect();
        let guard = self.bounds.lock().ok();
        let plan = dimming::plan(
            &inputs,
            |d| {
                settings::continuum_for(
                    d.kind,
                    &settings::monitor_config(&self.config, d.id.as_str()),
                    self.gamma_allowed,
                )
            },
            |id| guard.as_ref().and_then(|b| b.bounds_for(id)),
        );
        plan.commands
    }

    /// The engine hardware target for a user level (continuum-floored).
    fn hardware_target(&self, kind: DisplayKind, id: &str, user_pct: u8) -> u8 {
        let cfg = settings::continuum_for(
            kind,
            &settings::monitor_config(&self.config, id),
            self.gamma_allowed,
        );
        map_user_level(user_pct, &cfg)
            .hardware_pct
            .unwrap_or(user_pct)
    }

    /// The class of a known display id.
    fn kind_of(&self, id: &StableDisplayId) -> Option<DisplayKind> {
        self.displays
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, kind)| *kind)
    }
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

/// Wire the flyout's command fan-out to the app state.
fn wire_ui_commands() {
    // Read-only setup borrow (runs once, never re-entrant): register the
    // handler, which routes every command through the re-entrancy-safe
    // [`with_app`] dispatcher.
    APP.with(|cell| {
        cell.with_ref(|app| {
            app.shell.on_command(|command| {
                with_app(move |app| app.on_ui_command(command));
            });
            // Click-outside-to-dismiss: hide the flyout when it loses focus,
            // routed through the app so `flyout_visible` stays honest (bug 5).
            app.shell.on_focus_lost(|| {
                with_app(AppState::on_focus_lost);
            });
        });
    });
}

/// Wire the settings window's command fan-out to the app state.
fn wire_settings_commands() {
    APP.with(|cell| {
        cell.with_ref(|app| {
            app.settings_shell.on_command(|command| {
                with_app(move |app| app.on_settings_command(command));
            });
        });
    });
}

/// Build the editable hotkey rows for the settings window.
///
/// One row per [`HotkeyAction`] (in a stable order), so every action shows a
/// record/clear affordance even when currently unbound. Each row carries the
/// configured binding (empty when unbound), a conflict flag (bound to the same
/// combo as another action), and an OS-rejected flag (`unavailable`) from the
/// last live registration outcome.
fn resolved_hotkey_rows(
    config: &Config,
    outcomes: &BTreeMap<HotkeyAction, hotkey::RegisterResult>,
) -> Vec<HotkeyRow> {
    let plan = hotkey::resolve(&config.hotkeys);
    let conflicting: BTreeSet<Accelerator> =
        plan.conflicts.iter().map(|c| c.accel.clone()).collect();
    HotkeyAction::ALL
        .into_iter()
        .map(|action| {
            let binding = plan
                .bindings
                .iter()
                .find(|b| b.action == action)
                .map(|b| b.raw.clone())
                .unwrap_or_default();
            let conflicted = plan
                .bindings
                .iter()
                .any(|b| b.action == action && conflicting.contains(&b.accel));
            let unavailable = matches!(
                outcomes.get(&action),
                Some(hotkey::RegisterResult::OsRejected)
            );
            HotkeyRow {
                action_key: action.config_key().to_owned(),
                action_label: action_label(action).to_owned(),
                binding,
                conflicted,
                unavailable,
            }
        })
        .collect()
}

/// A human label for a hotkey action (the settings list is read-only English
/// chrome; a localized label is a follow-up).
fn action_label(action: HotkeyAction) -> &'static str {
    match action {
        HotkeyAction::BrightnessUp => "Brightness up",
        HotkeyAction::BrightnessDown => "Brightness down",
        HotkeyAction::ToggleFlyout => "Toggle flyout",
    }
}

/// Map an update-check [`UpdateOutcome`] onto the settings [`UpdateStatus`].
fn update_status_from(outcome: UpdateOutcome) -> UpdateStatus {
    match outcome {
        UpdateOutcome::UpToDate => UpdateStatus::UpToDate,
        UpdateOutcome::UpdateAvailable { version } => UpdateStatus::Available { version },
        UpdateOutcome::Failed(_) => UpdateStatus::Failed,
    }
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

/// Dispatch an [`Action`] onto the Slint main thread.
fn dispatch(action: Action) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(move |app| app.handle_action(action));
    });
}

/// Register the tray-icon and menu event handlers (they hop onto the Slint loop
/// via [`dispatch`]).
fn wire_tray_handlers() {
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent, menu::MenuEvent};

    let ids = MENU_IDS.with(|cell| cell.borrow().clone());
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = if event.id() == &ids.open {
            Action::Open
        } else if event.id() == &ids.settings {
            Action::OpenSettings
        } else if event.id() == &ids.restore {
            Action::Restore
        } else if event.id() == &ids.quit {
            Action::Quit
        } else {
            return;
        };
        dispatch(action);
    }));

    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            dispatch(Action::Toggle);
        }
    }));
}

/// The live global-hotkey registrar: owns the OS manager, the currently
/// registered [`HotKey`]s (so they can be unregistered on a re-plan), and the
/// id → action map the event handler resolves against.
///
/// Implements the pure [`hotkey::HotkeyRegistrar`] seam so [`hotkey::apply_plan`]
/// drives it; the OS-touching parts live here, behind that seam.
struct OsHotkeyRegistrar {
    /// The OS manager, kept alive so registrations stay live. `None` if the
    /// manager could not be created (hotkeys then silently unavailable).
    manager: Option<GlobalHotKeyManager>,
    /// The hotkeys currently registered with the OS (for `unregister_all`).
    registered: Vec<HotKey>,
    /// Which action each live hotkey id fires.
    map: BTreeMap<u32, HotkeyAction>,
}

impl OsHotkeyRegistrar {
    /// Create the registrar, eagerly building the OS manager on this (main)
    /// thread. A manager failure is logged and leaves hotkeys unavailable.
    fn new() -> Self {
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => Some(manager),
            Err(e) => {
                warn!(error = %e, "global hotkey manager unavailable; hotkeys disabled");
                None
            }
        };
        OsHotkeyRegistrar {
            manager,
            registered: Vec::new(),
            map: BTreeMap::new(),
        }
    }

    /// The action a live hotkey id fires, if any.
    fn action_for_id(&self, id: u32) -> Option<HotkeyAction> {
        self.map.get(&id).copied()
    }
}

impl hotkey::HotkeyRegistrar for OsHotkeyRegistrar {
    fn clear(&mut self) {
        if let Some(manager) = &self.manager
            && !self.registered.is_empty()
            && let Err(e) = manager.unregister_all(&self.registered)
        {
            warn!(error = %e, "failed to unregister previous hotkeys");
        }
        self.registered.clear();
        self.map.clear();
    }

    fn register(&mut self, accel: &Accelerator, action: HotkeyAction) -> bool {
        let Some(manager) = &self.manager else {
            return false;
        };
        let Some(hk) = accel_to_hotkey(accel) else {
            warn!(accel = %accel, "hotkey key not supported by the OS backend; skipping");
            return false;
        };
        if accel.modifiers.is_empty() {
            warn!(accel = %accel, "modifierless global hotkey may capture the key system-wide");
        }
        let id = hk.id();
        match manager.register(hk) {
            Ok(()) => {
                self.registered.push(hk);
                self.map.insert(id, action);
                debug!(accel = %accel, action = action.config_key(), "registered hotkey");
                true
            }
            Err(e) => {
                warn!(accel = %accel, error = %e, "failed to register hotkey (already owned?); skipping");
                false
            }
        }
    }
}

/// Log the parse errors and conflicts in a resolved [`hotkey::HotkeyPlan`].
fn log_hotkey_issues(plan: &hotkey::HotkeyPlan) {
    for err in &plan.errors {
        warn!(key = %err.key, binding = %err.raw, reason = %err.reason, "ignoring invalid hotkey binding");
    }
    for conflict in &plan.conflicts {
        let actions: Vec<&str> = conflict.actions.iter().map(|a| a.config_key()).collect();
        warn!(combo = %conflict.accel, ?actions, "hotkey combo bound to multiple actions; skipping all");
    }
}

/// Index a batch of [`hotkey::RegisterOutcome`]s by action for settings-row
/// feedback (the last outcome per action wins).
fn outcomes_by_action(
    outcomes: &[hotkey::RegisterOutcome],
) -> BTreeMap<HotkeyAction, hotkey::RegisterResult> {
    outcomes.iter().map(|o| (o.action, o.result)).collect()
}

/// Install the global-hotkey event handler. A pressed hotkey is resolved to its
/// action against the live registrar (in the app state) on the Slint loop, so a
/// live re-registration is picked up without re-installing the handler.
fn install_hotkey_event_handler() {
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        // Fire on the press edge only (the release edge arrives on global-hotkey's
        // worker thread); hop onto the Slint loop via `with_app`.
        if event.state() == HotKeyState::Pressed {
            let id = event.id();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(move |app| app.on_hotkey_fired(id));
            });
        }
    }));
}

/// Map a [`HotkeyAction`] onto the tray [`Action`] it triggers.
fn action_for(action: HotkeyAction) -> Action {
    match action {
        HotkeyAction::BrightnessUp => Action::Nudge(HOTKEY_BRIGHTNESS_STEP),
        HotkeyAction::BrightnessDown => Action::Nudge(-HOTKEY_BRIGHTNESS_STEP),
        HotkeyAction::ToggleFlyout => Action::Toggle,
    }
}

/// Convert a parsed [`Accelerator`] into a `global_hotkey` [`HotKey`], or `None`
/// if the key has no `global_hotkey` [`Code`].
fn accel_to_hotkey(accel: &Accelerator) -> Option<HotKey> {
    let code = code_for_key(accel.key.as_str())?;
    Some(HotKey::new(Some(ghk_modifiers(accel.modifiers)), code))
}

/// Translate Duja's modifier set into `global_hotkey`'s.
fn ghk_modifiers(mods: AccelModifiers) -> GhkModifiers {
    let mut out = GhkModifiers::empty();
    if mods.contains(AccelModifiers::CONTROL) {
        out |= GhkModifiers::CONTROL;
    }
    if mods.contains(AccelModifiers::ALT) {
        out |= GhkModifiers::ALT;
    }
    if mods.contains(AccelModifiers::SHIFT) {
        out |= GhkModifiers::SHIFT;
    }
    if mods.contains(AccelModifiers::SUPER) {
        out |= GhkModifiers::SUPER;
    }
    out
}

/// Map a canonical key token (see [`hotkey`]) onto a `global_hotkey` [`Code`]
/// via its W3C `KeyboardEvent.code` name.
fn code_for_key(token: &str) -> Option<Code> {
    use std::str::FromStr as _;
    let w3c = match token {
        "UP" => "ArrowUp".to_owned(),
        "DOWN" => "ArrowDown".to_owned(),
        "LEFT" => "ArrowLeft".to_owned(),
        "RIGHT" => "ArrowRight".to_owned(),
        "SPACE" => "Space".to_owned(),
        "ENTER" => "Enter".to_owned(),
        "TAB" => "Tab".to_owned(),
        "ESCAPE" => "Escape".to_owned(),
        "HOME" => "Home".to_owned(),
        "END" => "End".to_owned(),
        "PAGEUP" => "PageUp".to_owned(),
        "PAGEDOWN" => "PageDown".to_owned(),
        "INSERT" => "Insert".to_owned(),
        "DELETE" => "Delete".to_owned(),
        "BACKSPACE" => "Backspace".to_owned(),
        other => {
            if let Some(digits) = other.strip_prefix('F')
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
            {
                other.to_owned() // F1..=F24
            } else {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_uppercase() => format!("Key{c}"),
                    (Some(c), None) if c.is_ascii_digit() => format!("Digit{c}"),
                    _ => return None,
                }
            }
        }
    };
    Code::from_str(&w3c).ok()
}

thread_local! {
    /// The menu item ids, captured so the (Send) menu handler can match them.
    static MENU_IDS: RefCell<MenuIds> = RefCell::new(MenuIds::default());
}

/// The tray menu item ids, for matching menu events.
#[derive(Clone, Default)]
struct MenuIds {
    open: tray_icon::menu::MenuId,
    settings: tray_icon::menu::MenuId,
    restore: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}

/// Build the tray icon with its right-click menu (Open / Settings / Restore
/// screen / Quit).
fn build_tray() -> anyhow::Result<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuItem};
    use tray_icon::{TrayIconBuilder, menu::PredefinedMenuItem};

    let menu = Menu::new();
    let open = MenuItem::new("Open", true, None);
    let settings = MenuItem::new("Settings", true, None);
    let restore = MenuItem::new("Restore screen", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[
        &open,
        &settings,
        &restore,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|e| anyhow::anyhow!("failed to build tray menu: {e}"))?;

    MENU_IDS.with(|cell| {
        *cell.borrow_mut() = MenuIds {
            open: open.id().clone(),
            settings: settings.id().clone(),
            restore: restore.id().clone(),
            quit: quit.id().clone(),
        };
    });

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Duja")
        .with_icon(icon::sun_icon()?)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to create the tray icon: {e}"))
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

/// Fallback paths under the OS temp dir when no home directory is resolvable.
fn fallback_paths() -> DujaPaths {
    let root = std::env::temp_dir().join("duja");
    warn!(root = %root.display(), "no home directory; using a temp data root");
    DujaPaths {
        config: root.join("config.toml"),
        state: root.join("state.toml"),
        crash_marker: root.join("gamma.dirty"),
        log_dir: root.join("logs"),
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

/// Bridge engine notifications onto the Slint loop on a side thread.
fn spawn_notification_bridge(notifications: crossbeam_channel::Receiver<EngineNotification>) {
    std::thread::Builder::new()
        .name("duja-notify-bridge".to_owned())
        .spawn(move || {
            while let Ok(notification) = notifications.recv() {
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(move |app| app.on_notification(notification));
                });
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    //! Coverage for the pure accelerator → `global_hotkey` conversion boundary
    //! and the action mapping. The actual OS delivery of a `WM_HOTKEY` to the
    //! registered handler is NOT unit-tested here (global-hotkey's test story is
    //! weak and synthesising `WM_HOTKEY` does not reliably reach its handler); it
    //! is covered by the P1 `spike/eventloop` proof and manual hardware QA.
    use super::{Accelerator, Action, Code, GhkModifiers, HotkeyAction};
    use super::{
        DEFAULT_SOFTWARE_DIM_FLOOR_PCT, TOGGLE_GUARD, floor_to_seed_on_dimming, toggle_decision,
    };
    use super::{
        DEFAULT_USER_LEVEL_PCT, ToggleDecision, accel_to_hotkey, action_for, adopt_enumeration,
        code_for_key, ghk_modifiers, level_to_adopt,
    };
    use crate::bin_support::state_store::StateStore;
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};
    use std::collections::BTreeSet;
    use std::time::Duration;

    // --- Item 5: launching Duja must NOT change the monitor brightness ---------
    //
    // The bug (confirmed on the live box: floor 20, overlay, persisted 48): the
    // first enumeration pushed the PERSISTED level to the engine, dimming the
    // monitor to the last-saved level on every launch, and seeded the UI from the
    // persisted file. The fix ADOPTS the monitor's current hardware reading
    // (`snap.user_level_pct`) and writes nothing. `adopt_enumeration` has no engine
    // channel by construction, so adoption structurally cannot push a level; the
    // live 20×-cycle probe confirms the brightness stays put on launch.

    fn adopt_snap(serial: &str, reading_pct: u8) -> DisplaySnapshot {
        DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap(),
            name: format!("Monitor {serial}"),
            kind: DisplayKind::ExternalDdc,
            user_level_pct: reading_pct,
            capabilities: Capabilities::default(),
        }
    }

    fn temp_state() -> (StateStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (StateStore::load(dir.path().join("state.toml")), dir)
    }

    #[test]
    fn level_to_adopt_prefers_the_live_hardware_reading_over_persisted() {
        // A real reading is adopted verbatim, even against a different (low — the
        // bug's trigger) persisted level: the slider mirrors reality, nothing moves.
        assert_eq!(level_to_adopt(70, Some(20)), 70);
        assert_eq!(level_to_adopt(70, None), 70);
    }

    #[test]
    fn level_to_adopt_falls_back_to_persisted_only_for_the_pre_probe_placeholder() {
        // While the reading is still the pre-probe placeholder, the persisted
        // last-known seeds the UI (the documented fallback for a failed/pending read)…
        assert_eq!(level_to_adopt(DEFAULT_USER_LEVEL_PCT, Some(20)), 20);
        // …but with nothing persisted there is nothing better than the placeholder.
        assert_eq!(
            level_to_adopt(DEFAULT_USER_LEVEL_PCT, None),
            DEFAULT_USER_LEVEL_PCT
        );
    }

    #[test]
    fn adoption_seeds_from_the_reading_and_pushes_no_level() {
        // The old code pushed the persisted 48 to hardware here (dimming on launch);
        // adoption takes the live reading (70) into state and — since this fn has no
        // engine channel — sends ZERO SetUserLevel. The seed is the reading.
        let snap = adopt_snap("A", 70);
        let id = snap.id.as_str().to_owned();
        let (mut state, _dir) = temp_state();
        state.record(&id, 48, 1); // low persisted level, as on the live box
        adopt_enumeration(&[snap], &BTreeSet::new(), &mut state, 2);
        assert_eq!(
            state.level(&id),
            Some(70),
            "must adopt the live hardware reading, not the persisted 48"
        );
    }

    #[test]
    fn adoption_never_clobbers_a_user_controlled_display() {
        // After a genuine user change, a later enumeration echo must not re-adopt
        // (overwrite) the user's chosen level.
        let snap = adopt_snap("A", 70);
        let id = snap.id.as_str().to_owned();
        let (mut state, _dir) = temp_state();
        state.record(&id, 35, 1); // the user's chosen level
        let controlled: BTreeSet<String> = std::iter::once(id.clone()).collect();
        adopt_enumeration(&[snap], &controlled, &mut state, 2);
        assert_eq!(
            state.level(&id),
            Some(35),
            "a user-controlled level must survive an enumeration echo"
        );
    }

    // --- Defect 2: the flyout "Software dimming" toggle must actually engage ---
    //
    // The schema default `hw_floor_pct = 0` leaves no sub-floor region, so
    // `map_user_level` never produces an overlay and the toggle was a visual
    // no-op. Turning it on now seeds a floor so dimming has a region to act in;
    // the pure `dimming::plan` tests then prove an overlay engages below a
    // non-zero floor.
    #[test]
    fn dimming_toggle_seeds_a_floor_only_when_turned_on_without_one() {
        // Turned on with no floor → seed the default so the overlay can engage.
        assert_eq!(
            floor_to_seed_on_dimming(true, 0),
            Some(DEFAULT_SOFTWARE_DIM_FLOOR_PCT)
        );
        // Turned on with a floor already set → leave the user's floor alone.
        assert_eq!(floor_to_seed_on_dimming(true, 25), None);
        // Turned off → never seed (this was the pre-fix behaviour for *every*
        // case, which is exactly why the on-with-no-floor toggle did nothing).
        assert_eq!(floor_to_seed_on_dimming(false, 0), None);
        assert_eq!(floor_to_seed_on_dimming(false, 25), None);
    }

    #[test]
    fn toggle_decision_hides_a_visible_flyout() {
        // Visible → hide, regardless of the last-hidden timestamp.
        assert_eq!(
            toggle_decision(true, None, TOGGLE_GUARD),
            ToggleDecision::Hide
        );
        assert_eq!(
            toggle_decision(true, Some(Duration::from_millis(10)), TOGGLE_GUARD),
            ToggleDecision::Hide
        );
    }

    #[test]
    fn toggle_decision_ignores_a_click_right_after_focus_loss_hide() {
        // Hidden within the guard window: this click is the tail of the gesture
        // that just dismissed the flyout; swallow it (do not re-open).
        assert_eq!(
            toggle_decision(false, Some(Duration::from_millis(50)), TOGGLE_GUARD),
            ToggleDecision::Ignore
        );
    }

    #[test]
    fn toggle_decision_opens_when_hidden_long_ago_or_never() {
        // Never shown, or hidden well before this click → open.
        assert_eq!(
            toggle_decision(false, None, TOGGLE_GUARD),
            ToggleDecision::Show
        );
        assert_eq!(
            toggle_decision(false, Some(Duration::from_millis(500)), TOGGLE_GUARD),
            ToggleDecision::Show
        );
    }

    fn accel(s: &str) -> Accelerator {
        Accelerator::parse(s).expect("valid accelerator")
    }

    #[test]
    fn code_for_key_maps_every_supported_key_family() {
        assert_eq!(code_for_key("UP"), Some(Code::ArrowUp));
        assert_eq!(code_for_key("DOWN"), Some(Code::ArrowDown));
        assert_eq!(code_for_key("F9"), Some(Code::F9));
        assert_eq!(code_for_key("F24"), Some(Code::F24));
        assert_eq!(code_for_key("A"), Some(Code::KeyA));
        assert_eq!(code_for_key("7"), Some(Code::Digit7));
        assert_eq!(code_for_key("SPACE"), Some(Code::Space));
        assert_eq!(code_for_key("PAGEUP"), Some(Code::PageUp));
        // A token with no W3C code maps to None (registration then skips it).
        assert_eq!(code_for_key("NOPE"), None);
    }

    #[test]
    fn ghk_modifiers_translates_each_flag() {
        let all = accel("Ctrl+Alt+Shift+Super+Up");
        let mods = ghk_modifiers(all.modifiers);
        assert!(mods.contains(GhkModifiers::CONTROL));
        assert!(mods.contains(GhkModifiers::ALT));
        assert!(mods.contains(GhkModifiers::SHIFT));
        assert!(mods.contains(GhkModifiers::SUPER));

        let none = ghk_modifiers(accel("F9").modifiers);
        assert!(none.is_empty());
    }

    #[test]
    fn accel_to_hotkey_builds_the_expected_hotkey() {
        let hk = accel_to_hotkey(&accel("Ctrl+Alt+Up")).expect("convertible");
        assert_eq!(hk.key, Code::ArrowUp);
        assert!(hk.mods.contains(GhkModifiers::CONTROL));
        assert!(hk.mods.contains(GhkModifiers::ALT));
        assert!(!hk.mods.contains(GhkModifiers::SHIFT));
    }

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

    #[test]
    fn action_for_maps_actions_to_tray_actions() {
        assert!(matches!(
            action_for(HotkeyAction::BrightnessUp),
            Action::Nudge(n) if n > 0
        ));
        assert!(matches!(
            action_for(HotkeyAction::BrightnessDown),
            Action::Nudge(n) if n < 0
        ));
        assert!(matches!(
            action_for(HotkeyAction::ToggleFlyout),
            Action::Toggle
        ));
    }
}

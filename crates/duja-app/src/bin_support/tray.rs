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

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use crossbeam_channel::Sender;
use tracing::{debug, error, info, warn};

use duja_app::{Engine, EngineCommand, EngineConfig, EngineNotification, Enumeration};
use duja_core::config::Config;
use duja_core::continuum::map_user_level;
use duja_core::dimmer::{DimCommand, Dimmer};
use duja_core::id::StableDisplayId;
use duja_core::model::{DisplayKind, DisplaySnapshot};
use duja_dimmer::PlatformDimmer;
use duja_ui::{FlyoutShell, FlyoutVm, UiCommand};

use crate::bin_support::bounds::BoundsMap;
use crate::bin_support::dimming::{self, DisplayInput};
use crate::bin_support::paths::DujaPaths;
use crate::bin_support::state_store::StateStore;
use crate::bin_support::{backend, gamma, run, settings, startup};

mod geometry;
mod icon;

/// Nominal flyout size (px) for anchor computation; the window's real height is
/// content-driven (see `flyout.slint`), this is the layout envelope.
const FLYOUT_SIZE: (u32, u32) = (320, 480);
/// Gap kept from the work-area edges when placing the flyout.
const FLYOUT_MARGIN: i32 = 12;

thread_local! {
    /// The main-thread application state, reachable from the foreign
    /// (tray/menu/notification) event handlers that hop onto the Slint loop.
    static APP: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// An action requested by a tray/menu interaction, applied on the Slint thread.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// Show the flyout at the tray/cursor anchor.
    Open,
    /// Toggle the flyout's visibility.
    Toggle,
    /// Restore the screen (clear overlays + identity gamma on every display).
    Restore,
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

    // 1. Single-instance guard: a second launch exits 0 (IPC show-flyout is P5).
    let instance = duja_platform::SingleInstance::acquire();
    if instance.already_running() {
        info!("another duja instance is already running; exiting");
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
    APP.with(|slot| {
        *slot.borrow_mut() = Some(AppState {
            shell,
            vm,
            dimmer,
            config,
            gamma_allowed,
            bounds,
            state: StateStore::load(paths.state.clone()),
            crash_marker: paths.crash_marker.clone(),
            engine_tx: engine.sender(),
            gamma,
            displays: Vec::new(),
            applied: BTreeSet::new(),
            flyout_visible: false,
        });
    });
    wire_ui_commands();
    wire_tray_handlers();
    spawn_notification_bridge(notifications);

    info!("duja tray running");
    let loop_result = slint::run_event_loop_until_quit();
    if let Err(e) = loop_result {
        error!(error = %e, "event loop exited with an error");
    }

    // 8. Clean teardown (state was flushed on Quit; this joins the threads).
    engine.shutdown();
    forwarder.shutdown();
    APP.with(|slot| {
        // Dropping the AppState clears overlays via the dimmer's own teardown.
        *slot.borrow_mut() = None;
    });
    drop(tray);
    drop(instance);
    Ok(ExitCode::SUCCESS)
}

/// The main-thread application state driven by every event source.
struct AppState {
    shell: FlyoutShell,
    vm: Rc<RefCell<FlyoutVm>>,
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
    /// Displays whose saved level has already been pushed to the engine.
    applied: BTreeSet<String>,
    flyout_visible: bool,
}

impl AppState {
    /// Apply a tray/menu action.
    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Open => self.show_flyout(),
            Action::Toggle => {
                if self.flyout_visible {
                    self.hide_flyout();
                } else {
                    self.show_flyout();
                }
            }
            Action::Restore => self.restore_screen(),
            Action::Quit => self.begin_quit(),
        }
    }

    /// Show the flyout anchored near the tray/cursor.
    fn show_flyout(&mut self) {
        let (cursor, work) = geometry::cursor_and_work_area();
        let (x, y) = crate::bin_support::positioning::flyout_origin(
            cursor,
            work,
            FLYOUT_SIZE,
            FLYOUT_MARGIN,
        );
        self.shell.show_at(x, y);
        self.flyout_visible = true;
    }

    /// Hide the flyout (process keeps running in the tray).
    fn hide_flyout(&mut self) {
        self.shell.hide();
        self.flyout_visible = false;
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
        }
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

    /// Adopt a fresh enumeration: seed levels, push saved levels to the engine
    /// once, rebuild the flyout rows against *user* levels, and re-apply
    /// overlays.
    fn on_displays_changed(&mut self, snapshots: Vec<DisplaySnapshot>) {
        self.displays = snapshots.iter().map(|s| (s.id.clone(), s.kind)).collect();

        let now = Instant::now();
        for snap in &snapshots {
            // Seed the user level from the persisted state, else the engine's
            // initial hardware-derived reading.
            let user = self
                .state
                .seed_if_absent(snap.id.as_str(), snap.user_level_pct, unix_now());
            // Push the saved level to the engine the first time we see a display.
            if self.applied.insert(snap.id.as_str().to_owned()) {
                let hw = self.hardware_target(snap.kind, snap.id.as_str(), user);
                let _ = self.engine_tx.send(EngineCommand::SetUserLevel {
                    id: snap.id.clone(),
                    pct: hw,
                });
            }
        }

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
        self.render();
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
    fn plan_commands(&self) -> Vec<DimCommand> {
        let inputs: Vec<DisplayInput> = self
            .displays
            .iter()
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
    APP.with(|slot| {
        if let Some(app) = slot.borrow().as_ref() {
            app.shell.on_command(|command| {
                APP.with(|slot| {
                    if let Some(app) = slot.borrow_mut().as_mut() {
                        app.on_ui_command(command);
                    }
                });
            });
        }
    });
}

/// Dispatch an [`Action`] onto the Slint main thread.
fn dispatch(action: Action) {
    let _ = slint::invoke_from_event_loop(move || {
        APP.with(|slot| {
            if let Some(app) = slot.borrow_mut().as_mut() {
                app.handle_action(action);
            }
        });
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

thread_local! {
    /// The menu item ids, captured so the (Send) menu handler can match them.
    static MENU_IDS: RefCell<MenuIds> = RefCell::new(MenuIds::default());
}

/// The tray menu item ids, for matching menu events.
#[derive(Clone, Default)]
struct MenuIds {
    open: tray_icon::menu::MenuId,
    restore: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}

/// Build the tray icon with its right-click menu (Open / Restore screen / Quit).
fn build_tray() -> anyhow::Result<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuItem};
    use tray_icon::{TrayIconBuilder, menu::PredefinedMenuItem};

    let menu = Menu::new();
    let open = MenuItem::new("Open", true, None);
    let restore = MenuItem::new("Restore screen", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[&open, &restore, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| anyhow::anyhow!("failed to build tray menu: {e}"))?;

    MENU_IDS.with(|cell| {
        *cell.borrow_mut() = MenuIds {
            open: open.id().clone(),
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
                    APP.with(|slot| {
                        if let Some(app) = slot.borrow_mut().as_mut() {
                            app.on_notification(notification);
                        }
                    });
                });
            }
        })
        .ok();
}

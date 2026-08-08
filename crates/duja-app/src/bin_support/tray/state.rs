//! The main-thread application state and the bulk of its behaviour: the flyout
//! and settings windows, the engine/dimmer/gamma fan-out, the clone grouping,
//! and every action a tray/menu/hotkey/IPC event resolves to.
//!
//! [`AppState`] is reached **only** through
//! [`with_app`](super::with_app) — see the [`ReentrantCell`](super::ReentrantCell)
//! doc for why that is the single access path.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::Sender;
use tracing::{error, info, warn};

use duja_app::{EngineCommand, EngineNotification};
use duja_core::config::Config;
use duja_core::continuum::{ContinuumConfig, map_user_level};
use duja_core::dimmer::{DimCommand, Dimmer};
use duja_core::id::StableDisplayId;
use duja_core::manager::DEFAULT_USER_LEVEL_PCT;
use duja_core::model::{DimMode, DisplayKind, DisplaySnapshot};
use duja_dimmer::PlatformDimmer;
use duja_platform::Autostart;
use duja_ui::{
    FlyoutShell, FlyoutVm, SettingsCommand, SettingsShell, SettingsVm, ThemeChoice, UiCommand,
};

use crate::bin_support::bounds::BoundsMap;
use crate::bin_support::clone_group::{self, CloneGrouping};
use crate::bin_support::dimming::{self, DisplayInput};
use crate::bin_support::hotkey::{self, Accelerator, HotkeyAction};
use crate::bin_support::level_forward::{EngineLevelSink, LevelForwarder};
use crate::bin_support::state_store::StateStore;
use crate::bin_support::updates;
use crate::bin_support::{gamma, motion, settings, settings_apply};

use super::hotkey_os::{OsHotkeyRegistrar, action_for, log_hotkey_issues, outcomes_by_action};
use super::policy::{
    GAMMA_VERDICT_TTL, TOGGLE_GUARD, ToggleDecision, adopt_enumeration, clamp_flyout_height,
    reflected_level, toggle_decision, verdict_probe_due,
};
use super::wiring::resolved_hotkey_rows;
use super::{
    Action, FLYOUT_LOGICAL_WIDTH, FLYOUT_MARGIN, FLYOUT_MAX_LOGICAL_HEIGHT,
    FLYOUT_MIN_LOGICAL_HEIGHT, SETTINGS_LOGICAL_HEIGHT, SETTINGS_LOGICAL_WIDTH, geometry, open_url,
    spawn_relaunch, unix_now,
};

/// The main-thread application state driven by every event source.
pub(super) struct AppState {
    pub(super) shell: FlyoutShell,
    pub(super) vm: Rc<RefCell<FlyoutVm>>,
    /// The settings window shell and its shared view-model.
    pub(super) settings_shell: SettingsShell,
    pub(super) settings_vm: Rc<RefCell<SettingsVm>>,
    /// The platform launch-at-login backend (`None` if unavailable — the toggle
    /// is then shown disabled).
    pub(super) autostart: Option<Box<dyn Autostart>>,
    /// The user-facing config file, for format-preserving settings writes.
    pub(super) config_path: std::path::PathBuf,
    /// The most recent full snapshots (with capabilities), for the settings
    /// per-monitor sections.
    pub(super) snapshots: Vec<DisplaySnapshot>,
    pub(super) dimmer: Option<PlatformDimmer>,
    pub(super) config: Config,
    /// The live HDR gamma verdict: `false` forces every gamma-mode display onto
    /// the overlay path. Seeded at startup and re-probed on enumeration by
    /// [`AppState::refresh_gamma_verdict`], so a display that goes HDR mid-session
    /// stops receiving a (bypassed, marker-writing) gamma ramp.
    pub(super) gamma_allowed: bool,
    /// When the HDR verdict was last probed, so the DXGI query is throttled off
    /// the slider-drag hot path (see [`GAMMA_VERDICT_TTL`]).
    pub(super) last_gamma_probe: Option<Instant>,
    pub(super) bounds: Arc<Mutex<BoundsMap>>,
    pub(super) state: StateStore,
    pub(super) crash_marker: std::path::PathBuf,
    pub(super) engine_tx: Sender<EngineCommand>,
    /// The slider → engine forwarding seam every user level change goes through.
    ///
    /// Held separately from [`engine_tx`](Self::engine_tx) (which carries the
    /// non-level commands: refresh, polling, input, shutdown) so the level path
    /// is a named seam with an explicit no-throttle contract — a UI-side
    /// throttle on this path was a real shipped defect (P4 gate Finding 1).
    ///
    /// Its tests in [`crate::bin_support::level_forward`] cover only the
    /// forwarder itself, which is *downstream* of both callers and so can never
    /// see a throttle placed above it. Those callers are pinned separately - see
    /// [`set_user_level`](Self::set_user_level), which names the two tests and
    /// why there are two.
    pub(super) levels: LevelForwarder<EngineLevelSink>,
    /// The opt-in gamma sub-floor channel (RAII crash-marker owner + engage/
    /// restore executor). Drives [`DimCommand`]s carrying a gamma factor to the
    /// GPU ramp; identity-restored on quit/restore.
    pub(super) gamma: gamma::GammaBackend,
    /// The current display set from the last enumeration: resolved id, physical
    /// class, and the runtime software-only flag (`kind` is never overwritten to
    /// encode software-only — the flag carries it, #67). Kept per-panel (not
    /// per-group) because the member→group mapping and the fan-out need every
    /// physical panel; the flyout/state/overlays are keyed per group via
    /// [`groups`](Self::groups).
    pub(super) displays: Vec<(StableDisplayId, DisplayKind, bool)>,
    /// Mirrored (Duplicate-mode) panels sharing one GDI surface, collapsed into one
    /// control each (#66). Rebuilt from [`displays`](Self::displays) + the bounds
    /// map on every enumeration, so it is always fresh across hot-plug and
    /// Duplicate↔Extend transitions. The flyout row, the user level, the
    /// user-controlled flag, and the one overlay/gamma command per surface are all
    /// keyed under a group's stable anchor.
    pub(super) groups: CloneGrouping,
    /// Per-**member** unresponsive set, aggregated per group: the merged row greys
    /// only when every member of its group is unresponsive (any live member keeps
    /// the slider interactive). Tracked here (not just in the flyout view-model)
    /// because the view-model sees only the merged anchor rows.
    pub(super) unresponsive: BTreeSet<StableDisplayId>,
    /// Displays the user has explicitly driven this session (slider / hotkey /
    /// IPC). Until a display is in this set, Duja only *adopts* its current
    /// hardware brightness (mirrors it into the UI, writes nothing — item 5); once
    /// the user acts it becomes authoritative, so a later enumeration echo never
    /// clobbers the user's value, and its overlay/gamma may engage.
    pub(super) user_controlled: BTreeSet<String>,
    pub(super) flyout_visible: bool,
    /// When the flyout was last hidden, for the tray-click toggle guard
    /// ([`toggle_decision`]).
    pub(super) last_hidden: Option<Instant>,
    /// The live global-hotkey registrar (OS manager + id→action map), re-applied
    /// whenever the hotkey config changes.
    pub(super) hotkeys: OsHotkeyRegistrar,
    /// The last live-registration result per action, for settings-row feedback
    /// (conflict / OS-rejected).
    pub(super) hotkey_outcomes: BTreeMap<HotkeyAction, hotkey::RegisterResult>,
    /// The tray — owned here (rather than as a `run()` local) so an accent change
    /// can swap its glyph colour live. Dropping `AppState` at teardown drops it,
    /// exactly as the old local did.
    ///
    /// One field where there were three concrete `tray-icon` handles, which is the
    /// seam ADR-0010 asks for: see [`super::surface`] for why the second backend
    /// could not have matched the shape they imposed.
    pub(super) tray: super::surface::PlatformTray,
    /// The newest release surfaced this session (`Some(tag)`), for dedup so the
    /// menu item/toast fire once per version.
    pub(super) update_available: Option<String>,
    /// Whether an update check is currently running on the background thread, so
    /// checks never overlap or hammer the API.
    pub(super) update_check_in_flight: bool,
}

impl AppState {
    /// Apply a tray/menu action.
    pub(super) fn handle_action(&mut self, action: Action) {
        // Piggyback the once-a-day update check on a real user interaction, so
        // it never needs a timer (the zero-idle-wakeup guarantee holds).
        self.maybe_background_update_check();
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
            Action::OpenReleases => open_url(updates::RELEASES_PAGE_URL),
            Action::Restart => self.restart(),
            Action::Quit => self.begin_quit(),
        }
    }

    /// Adjust every known display's brightness by `delta` percentage points
    /// (clamped 0..=100), routing each change through the same user-level path
    /// the flyout slider uses so state, engine and overlays stay consistent.
    fn nudge_all(&mut self, delta: i16) {
        // Iterate group ANCHORS, not physical panels (#66): a mirrored set is one
        // control, so nudging it once — not once per member — avoids applying the
        // step N times to the same shared level.
        let anchors: Vec<StableDisplayId> = self
            .groups
            .groups()
            .iter()
            .map(|group| group.anchor.clone())
            .collect();
        for anchor in anchors {
            let current = i16::from(self.state.level(anchor.as_str()).unwrap_or(100));
            let next = current.saturating_add(delta).clamp(0, 100);
            let pct = u8::try_from(next).unwrap_or(0);
            self.set_user_level(&anchor, pct);
        }
    }

    /// Show the flyout anchored near the tray/cursor, in one shot.
    ///
    /// The window is sized *and* anchored **while still hidden** — in the anchor
    /// units of the target monitor, using the two conversion factors
    /// `duja_platform::TrayAnchor` derives (see `tray::geometry`) — then shown
    /// exactly once, with no resize or move afterwards. A post-show resize (the
    /// former buffer re-assert) made the software renderer occasionally present a
    /// partial/transparent first frame that only repaired on a later click (item
    /// 1); presenting a correctly-sized, correctly-placed window in one shot
    /// removes that race. The anchor still clamps against the window's true size
    /// so it lands flush against the tray edge at any scale (P0 live-QA bug 4);
    /// Slint sizes the buffer natively for the monitor it is shown on (PR #29).
    pub(super) fn show_flyout(&mut self) {
        use crate::bin_support::positioning::{
            anchor_window_size, flyout_height_cap, flyout_origin,
        };
        // Re-resolve the palette before showing. `System` must FOLLOW the OS, not
        // freeze at launch: the settings window already re-resolves on every open
        // (`rebuild_settings` → `resolved_dark`), so a flyout that only ever read
        // the OS once could render the opposite palette to the settings window
        // after the user switched the OS theme mid-session — two windows of one
        // app, side by side, disagreeing.
        //
        // Re-reading on show rather than reacting to an event is deliberate:
        // neither winit/Slint nor `PlatformEvent` carries a theme-change
        // notification, and this is one registry read (or one `NSUserDefaults`
        // read) per flyout open, on a path that is already opening a window.
        self.refresh_system_theme();
        let anchor = geometry::cursor_anchor();
        // Drive the window height from the row count (a no-frame window is not
        // auto-grown to its content preferred size), but never exceed the work
        // area: on a small screen the flyout caps here and its rows scroll
        // instead of overflowing off-screen. Logical px — Slint scales it.
        let cap = flyout_height_cap(
            anchor.work,
            anchor.logical_to_anchor,
            FLYOUT_MARGIN,
            FLYOUT_MAX_LOGICAL_HEIGHT,
        );
        let logical_height = clamp_flyout_height(self.flyout_logical_height(), cap);
        self.shell.set_content_height(logical_height);

        let sized = anchor_window_size(
            FLYOUT_LOGICAL_WIDTH,
            logical_height,
            anchor.logical_to_anchor,
        );
        let origin = flyout_origin(anchor.cursor, anchor.work, sized, FLYOUT_MARGIN);
        let (x, y) = geometry::to_physical_position(origin, anchor.anchor_to_physical);
        self.shell
            .present_at(FLYOUT_LOGICAL_WIDTH, logical_height, x, y);
        self.flyout_visible = true;

        // Reflect external brightness changes while the flyout is open: poll the
        // hardware level so the monitor's own buttons move the slider. Disabled
        // again on hide, keeping the idle engine at zero wakeups.
        let _ = self
            .engine_tx
            .send(EngineCommand::SetLevelPolling { on: true });

        // Arm the external-change glide per the OS animation setting (queried now
        // so an accessibility change is picked up on the next open).
        self.shell
            .set_glide_ms(motion::glide_for(true, motion::os_animations_enabled()));

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
        const CHROME: f32 = 78.0; // padding + header + inter-section gap (no footer)
        const CARD: f32 = 101.0; // one card (name+caption row, then slider+pill row)
        const CARD_GAP: f32 = 8.0;
        let rows = self.vm.borrow().rows().len();
        let body = if rows == 0 {
            100.0 // empty-state panel
        } else {
            let n = f32::from(u16::try_from(rows).unwrap_or(u16::MAX));
            n * CARD + (n - 1.0) * CARD_GAP
        };
        (CHROME + body).clamp(FLYOUT_MIN_LOGICAL_HEIGHT, FLYOUT_MAX_LOGICAL_HEIGHT)
    }

    /// The flyout's content-driven logical height, clamped to the work area of
    /// the monitor under the cursor — the same sizing [`AppState::show_flyout`]
    /// applies. Re-queries the cursor work-area/scale (rather than caching) so a
    /// `DisplaysChanged` while the flyout is open re-asserts the *capped* height:
    /// that notification fires on every `SetUserLevel` (a slider drag) and every
    /// enumeration, and without the cap a drag/refresh would push the window back
    /// to full height and overflow a small/high-DPI work area.
    fn capped_flyout_height(&self) -> f32 {
        use crate::bin_support::positioning::flyout_height_cap;
        let anchor = geometry::cursor_anchor();
        let cap = flyout_height_cap(
            anchor.work,
            anchor.logical_to_anchor,
            FLYOUT_MARGIN,
            FLYOUT_MAX_LOGICAL_HEIGHT,
        );
        clamp_flyout_height(self.flyout_logical_height(), cap)
    }

    /// Hide the flyout (process keeps running in the tray).
    fn hide_flyout(&mut self) {
        self.shell.hide();
        self.flyout_visible = false;
        self.last_hidden = Some(Instant::now());
        // Stop level polling so the idle engine parks with zero wakeups, and force
        // the glide off so a hidden window can never schedule an animation frame.
        let _ = self
            .engine_tx
            .send(EngineCommand::SetLevelPolling { on: false });
        self.shell.set_glide_ms(0);
    }

    /// Dismiss the flyout when it loses focus (the user clicked outside it).
    ///
    /// Routed through the app so [`flyout_visible`](Self::flyout_visible) is kept
    /// in sync — the next tray click then re-opens it (P0 live-QA bug 5).
    pub(super) fn on_focus_lost(&mut self) {
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
        // then the global identity pass — unconditionally, unlike `begin_quit`.
        //
        // NOT for the reason this comment used to give ("anything left over from
        // a prior dirty run"): D-108 established that a leftover is the crash
        // marker's job at launch, and `gamma::tear_down_gamma` is where that
        // argument lives. The reason it stays here is different and stronger.
        // **The user asked by name.** Someone pressing "Restore screen" is asking
        // for exactly the trade `begin_quit` declines to make on their behalf —
        // if a colour-temperature tool's curve is what is wrong with their
        // screen, flattening it is the point.
        self.gamma.restore_all();
        let report = duja_dimmer::restore_all();
        info!(
            restored = report.restored.len(),
            failed = report.failed.len(),
            "restored screen on request"
        );
    }

    /// Restart Duja: spawn a fresh instance that waits for us to release the
    /// single-instance lock, then cleanly quit this one so it takes over.
    ///
    /// The replacement is spawned **before** quitting; if it cannot be spawned we
    /// stay running (a restart must never leave the user with nothing). The clean
    /// [`begin_quit`](Self::begin_quit) that follows restores gamma/overlays and
    /// flushes state, so the fresh instance adopts the same persisted levels — the
    /// user's fix for a stuck session (e.g. a display wrongly stuck software-only)
    /// without losing their settings.
    fn restart(&mut self) {
        match spawn_relaunch() {
            Ok(()) => {
                info!("restart requested: spawned a replacement instance; quitting this one");
                self.begin_quit();
            }
            Err(e) => {
                warn!(error = %e, "restart failed: could not spawn a replacement; staying running");
            }
        }
    }

    /// Clean shutdown: persist state, restore gamma, quit the event loop.
    fn begin_quit(&mut self) {
        let _ = self.state.flush(Instant::now());
        // Restore every display this session engaged. The gamma backend clears the
        // crash marker itself on a CLEAN restore and KEEPS it when a restore
        // genuinely failed — the never-brick net for a ramp that would outlive
        // the process — so the marker must be removed here ONLY when that restore
        // came back clean. (The prior unconditional remove defeated the retention,
        // so a failed restore left no marker and the next launch never recovered.)
        // "Backend", not "guard": Windows gets the retention from
        // `ScreenStateGuard`, Linux writes it out in `LinuxSink::restore_all`, and
        // macOS has no marker at all — but all three keep the same contract here.
        //
        // The *global* identity pass is deliberately NOT unconditional here, and
        // `gamma::tear_down_gamma` carries the whole argument: a clean quit that
        // walks every display writing identity flattens f.lux, redshift or a
        // calibration curve Duja never touched (D-108). `restore_screen` still
        // does it unconditionally, because there the user asked by name.
        let teardown =
            gamma::tear_down_gamma(|| self.gamma.restore_all(), duja_dimmer::restore_all);
        if teardown.own_clean {
            let _ = std::fs::remove_file(&self.crash_marker);
        }
        info!(
            gamma_clean = teardown.own_clean,
            wide_rescue_ran = teardown.wide_rescue_ran,
            "restored screen on quit"
        );
        if let Some(dimmer) = self.dimmer.as_mut() {
            let _ = dimmer.clear();
        }
        if let Err(e) = slint::quit_event_loop() {
            error!(error = %e, "failed to signal event-loop quit");
        }
    }

    /// Handle a UI command emitted by the flyout view-model.
    pub(super) fn on_ui_command(&mut self, command: UiCommand) {
        match command {
            // NB: unlike the other arms, SetLevel deliberately does NOT re-render
            // the flyout. The render for a slider change is owned by the shell's
            // `slider-changed` handler, which sets `instant-sync` for a link-all
            // fan-out so the passive linked sliders snap instead of gliding (BUG 5);
            // re-rendering here would clear that flag before the frame paints. See
            // `set_user_level`.
            //
            // NB: never guard this call with a throttle/debounce. This arm used
            // to be on the untested side of the seam; it is now pinned by
            // `level_path_tests::the_ui_command_arm_forwards_every_sample_too`,
            // which exists *separately* from the one on `set_user_level` because
            // a throttle here would not touch that method. See the test-coverage
            // note on `set_user_level`.
            UiCommand::SetLevel { id, pct } => self.set_user_level(&id, pct),
            UiCommand::Refresh => {
                let _ = self.engine_tx.send(EngineCommand::RefreshNow);
                // Re-arm polling to re-read levels at once (idempotent while the
                // flyout is already polling).
                let _ = self
                    .engine_tx
                    .send(EngineCommand::SetLevelPolling { on: true });
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
        // The toggle just switches the sub-floor dim mode. With the perceptual
        // continuum (ADR-0014) every hardware display already has a software zone
        // below its perceptual anchor even at floor 0, so no floor seeding is
        // needed (the old DEFAULT_SOFTWARE_DIM_FLOOR_PCT hack is gone).
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
            settings::platform_gamma_limits(),
        );
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());
    }

    /// Rebuild the flyout's per-display dimming info (floor + on/off) from the
    /// current config and push it into the flyout view-model.
    fn refresh_flyout_dimming(&self) {
        // One entry per clone group, keyed on the anchor (#66): the merged row's
        // marker/toggle use the group's aggregated software-only flag + the anchor's
        // per-monitor config.
        let info: BTreeMap<StableDisplayId, duja_ui::DimmingInfo> = self
            .groups
            .groups()
            .iter()
            .map(|group| {
                let monitor = settings::monitor_config(&self.config, group.anchor.as_str());
                let cfg = settings::continuum_for(
                    group.kind,
                    group.software_only,
                    &monitor,
                    self.gamma_allowed,
                );
                (
                    group.anchor.clone(),
                    duja_ui::DimmingInfo {
                        hardware_floor: cfg.hardware_floor,
                        min_perceived_pct: cfg.min_perceived_pct,
                        // Reflect the *configured* mode (not the HDR-guarded one) so
                        // the toggle shows what the user chose — except a software-only
                        // group always reads on (the overlay is its only channel, and
                        // `continuum_for` forces its effective mode Off -> Overlay).
                        dimming_on: settings::dimming_on(group.software_only, monitor.dim_mode),
                        // A software-only group's toggle is forced-on + disabled in the
                        // flyout (it is the only dimming channel), so carry the flag.
                        software_only: group.software_only,
                    },
                )
            })
            .collect();
        self.vm.borrow_mut().set_dimming_info(info);
    }

    /// Resolve a fired global hotkey id to its action and apply it.
    ///
    /// Called from the OS hotkey event source, which on Linux installs nothing —
    /// so on that platform no id can arrive and this cannot run.
    // RATIONALE (dead_code): see `hotkey::Modifiers::is_empty`. Deliberately an
    // allow rather than a `cfg`: gating the method would put a platform switch in
    // a file whose job is not platform switching, and `hotkey_none`'s
    // `action_for_id` already documents itself as existing to keep it out of here.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub(super) fn on_hotkey_fired(&mut self, id: u32) {
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
        use crate::bin_support::positioning::{anchor_window_size, center_in};
        self.rebuild_settings();
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());
        // Drive the content height (logical); Slint clamps it to the window's
        // min/max.
        self.settings_shell
            .set_content_height(SETTINGS_LOGICAL_HEIGHT);

        // Size + centre the window on the active monitor in that monitor's anchor
        // units while still hidden, then show once — no post-show resize/move.
        // Centring on the active monitor also avoids the OS default cascade
        // position (P0 live-QA bug 4).
        let anchor = geometry::cursor_anchor();
        let sized = anchor_window_size(
            SETTINGS_LOGICAL_WIDTH,
            SETTINGS_LOGICAL_HEIGHT,
            anchor.logical_to_anchor,
        );
        let centred = center_in(anchor.work, sized);
        let (x, y) = geometry::to_physical_position(centred, anchor.anchor_to_physical);
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
        let accent = settings_apply::accent_to_choice(self.config.general.accent);
        let dark = self.resolved_dark();
        let update_check_on = self.config.general.update_check;

        let hotkeys = resolved_hotkey_rows(&self.config, &self.hotkey_outcomes);
        {
            let mut vm = self.settings_vm.borrow_mut();
            vm.set_general(
                autostart_on,
                autostart_supported,
                theme,
                accent,
                update_check_on,
                dark,
            );
            vm.set_displays(
                &self.snapshots,
                &self.config,
                self.gamma_allowed,
                settings::platform_gamma_limits(),
            );
            vm.set_hotkeys(hotkeys);
        }
    }

    /// Handle a command emitted by the settings view-model.
    pub(super) fn on_settings_command(&mut self, command: SettingsCommand) {
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
            SettingsCommand::SetAccent(_) => self.apply_accent(),
            SettingsCommand::SetUpdateCheck(_) => {
                // Config-only; the VM already reflects the toggle.
            }
            SettingsCommand::CheckUpdates => self.start_update_check(),
            SettingsCommand::OpenReleasesPage => open_url(updates::RELEASES_PAGE_URL),
            SettingsCommand::SetMonitorFloor { id, .. }
            | SettingsCommand::SetMonitorMinPerceived { id, .. }
            | SettingsCommand::SetMonitorDimMode { id, .. } => {
                // Re-drive the display's current level through the new continuum:
                // the hardware target and overlay retarget while the slider thumb
                // stays put (the floor/anchor are write policy, not a rescale).
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
        let accent = settings_apply::accent_to_choice(self.config.general.accent);
        // `autostart`'s &mut borrow ends above (last used for `actual`), so the
        // whole-`self` `resolved_dark` call is free of a borrow conflict here.
        let dark = self.resolved_dark();
        self.settings_vm.borrow_mut().set_general(
            actual,
            supported,
            theme,
            accent,
            self.config.general.update_check,
            dark,
        );
    }

    /// Re-resolve the flyout palette after a theme change and re-render it. Also
    /// refreshes the settings view-model so its window follows the same palette
    /// (the caller re-renders the settings shell after this returns).
    fn apply_theme(&mut self, _choice: ThemeChoice) {
        self.refresh_system_theme();
        self.rebuild_settings();
        self.render();
    }

    /// Repaint everything in the newly-chosen accent: both windows' palettes, both
    /// windows' taskbar icons, and the tray icon.
    ///
    /// The palettes need no special handling — each shell re-resolves the accent
    /// against its theme on the next render — but the icons are raster buffers, so
    /// they are rebuilt and pushed explicitly.
    fn apply_accent(&mut self) {
        let accent = settings_apply::accent_to_choice(self.config.general.accent);
        self.vm.borrow_mut().set_accent(accent);
        self.rebuild_settings();

        if let Err(e) = self.tray.set_accent(accent) {
            warn!(error = %e, "could not swap the tray icon to the new accent");
        }
        // The window icons take the raw colour: they are Slint shells rather than
        // trays, so they are not behind the seam and must not be.
        let rgb = duja_ui::accent::icon_rgb(accent);
        self.shell.set_icon_rgb(rgb);
        self.settings_shell.set_icon_rgb(rgb);

        self.render();
    }

    /// Re-read the OS light/dark preference and push the resolved palette into the
    /// flyout view-model.
    ///
    /// A no-op when the preference is `Light`/`Dark` (the OS is not consulted),
    /// and cheap when it is `System`. Called before every show so the flyout and
    /// the settings window — which re-resolves on every open — cannot end up
    /// displaying opposite palettes.
    ///
    /// **Not covered by any test**, and deliberately said out loud: deleting the
    /// call from `show_flyout` leaves the whole suite green and clippy silent.
    ///
    /// This paragraph used to go on to say why that could not change - that
    /// `AppState` "cannot be built off the Slint main thread" because it owns a
    /// [`super::surface::PlatformTray`] and two live Slint shells. **That is no
    /// longer the reason.** The `fixture` module below (`cfg(test)`, so rustdoc
    /// does not render it) builds an `AppState` on every lane, with a recording
    /// fake behind the tray seam and the headless Slint backend behind both
    /// shells, and the one throttle row that shared this excuse has drained on
    /// it. What is left here is an ordinary uncovered call: nothing
    /// stops a test from building a fixture, calling `show_flyout`, and asserting
    /// the palette was re-resolved. It has not been written, which is a different
    /// statement from the one that used to be here and a much cheaper one to act
    /// on.
    fn refresh_system_theme(&mut self) {
        let theme = settings::ui_theme(
            self.config.general.theme,
            super::os_theme_if_needed(self.config.general.theme),
        );
        self.vm.borrow_mut().set_theme(theme);
    }

    /// The resolved palette (`true` = dark) for the current theme preference — the
    /// same resolution the flyout uses (`settings::ui_theme`), so the settings
    /// window renders the identical light/dark palette rather than a fixed one.
    fn resolved_dark(&self) -> bool {
        matches!(
            settings::ui_theme(
                self.config.general.theme,
                super::os_theme_if_needed(self.config.general.theme),
            ),
            duja_ui::Theme::Dark
        )
    }

    /// Re-apply a display's dimming after a floor/anchor/dim-mode change by
    /// re-driving its current user level through the normal path (recomputes the
    /// hardware target against the new continuum and re-plans overlays/gamma).
    ///
    /// The level is first re-clamped to the reachable range under the new config:
    /// turning software dimming **off** (or raising the floor) lifts the slider's
    /// minimum to the transition, so a level that was below it would otherwise
    /// strand the thumb below the new minimum while the screen jumps up to the
    /// transition brightness. Clamping keeps the thumb and the screen in sync.
    fn reapply_display(&mut self, id: &StableDisplayId) {
        // A mirrored set reapplies as one control: resolve to the anchor, clamp the
        // level under the GROUP continuum, and route back through set_user_level
        // (which fans out to the members) (#66).
        let anchor = self
            .groups
            .anchor_of(id)
            .cloned()
            .unwrap_or_else(|| id.clone());
        let level = self.state.level(anchor.as_str()).unwrap_or(100);
        let clamped = match self.group_meta(&anchor) {
            Some((kind, software_only)) => {
                let cfg = settings::continuum_for(
                    kind,
                    software_only,
                    &settings::monitor_config(&self.config, anchor.as_str()),
                    self.gamma_allowed,
                );
                level.max(settings::min_reachable_pct(cfg))
            }
            None => level,
        };
        self.set_user_level(&anchor, clamped);
    }

    /// Record a user level, forward the hardware write to the engine, and
    /// re-apply the overlay batch.
    ///
    /// Every `SetUserLevel` is forwarded — there is no UI-side throttle. The
    /// engine worker enforces `write_min_gap` with last-wins coalescing, which
    /// bounds the hardware write rate *and* guarantees the final value of a drag
    /// lands (see P4 gate Finding 1: a leading-edge UI throttle used to drop the
    /// final sample, leaving the hardware at an intermediate level).
    ///
    /// # Test coverage of that contract — read this before adding rate limiting
    ///
    /// **This method and [`on_ui_command`](Self::on_ui_command) are covered, and
    /// a throttle added to either one goes red.** That is the opposite of what
    /// this section said for four releases, so it is worth being exact about
    /// which test catches what:
    ///
    /// - `level_path_tests::a_slider_drag_forwards_every_sample_and_the_released_value_last`
    ///   drives six samples through **this** method and asserts both that all six
    ///   reach the engine and that the released one is last;
    /// - `level_path_tests::the_ui_command_arm_forwards_every_sample_too` does the
    ///   same through `on_ui_command`, because a throttle added there would leave
    ///   this method untouched and the first test green. Two sites, two tests.
    ///
    /// Both were proven red by re-inserting a leading-edge guard on the
    /// `self.levels.forward(&writes)` call below: one of the six samples survives.
    /// The ends were already pinned and still are — `duja_ui::shell`'s
    /// `slider_drag_burst_emits_the_released_value_last` drives the real Slint
    /// binding, the engine's worker tests pin last-wins coalescing, and
    /// [`crate::bin_support::level_forward`] pins [`LevelForwarder`] itself, which
    /// is *downstream* of this method and so can never see a throttle placed
    /// above it.
    ///
    /// What made the difference is the `fixture` module, not a new test-writing idea: the
    /// old note here was right that nothing could execute either method, and
    /// wrong only about why that was permanent.
    pub(super) fn set_user_level(&mut self, id: &StableDisplayId, pct: u8) {
        // Route to the group anchor: the flyout row, hotkey nudge, IPC and reflection
        // all address a member id, but a mirrored set is ONE control keyed under its
        // anchor (#66). State, the user-controlled flag and the overlay all live
        // under the anchor; the hardware write fans out to the members.
        let anchor = self
            .groups
            .anchor_of(id)
            .cloned()
            .unwrap_or_else(|| id.clone());
        let now = Instant::now();
        // A genuine user action: this group is now user-controlled, so it writes to
        // hardware here and its overlay/gamma may engage — and a later enumeration
        // will not re-adopt (clobber) this level (item 5).
        self.user_controlled.insert(anchor.as_str().to_owned());
        self.state.record(anchor.as_str(), pct, unix_now());

        // Fan the level out to every member's hardware under the group rule, then
        // forward after the group borrow ends (apply_overlays needs &mut self).
        let writes = self.group_hardware_writes(&anchor, pct);
        self.levels.forward(&writes);
        self.apply_overlays();
        let _ = self.state.maybe_flush(now);
        // Do NOT `self.render()` here. The flyout render for a slider change is
        // owned by the shell's `slider-changed` handler (duja-ui), which sets
        // `instant-sync` for a link-all fan-out so the passive linked sliders snap
        // to their new value instead of gliding (BUG 5). A render here calls
        // `update_from_vm` -> `render_into(link_originated = false)`, which clears
        // `instant-sync` before the fan-out frame paints, so the linked sliders
        // would re-gain the 160 ms glide — and with every test still green (the
        // shell smoke test drives a no-op command handler and cannot see this path).
    }

    /// Handle an engine notification (runs on the Slint thread).
    pub(super) fn on_notification(&mut self, notification: EngineNotification) {
        match notification {
            EngineNotification::DisplaysChanged(snapshots) => self.on_displays_changed(&snapshots),
            EngineNotification::DisplayUnresponsive(id) => self.on_member_responsive(&id, false),
            EngineNotification::DisplayResponsive(id) => self.on_member_responsive(&id, true),
            EngineNotification::LevelRead { id, hw_pct } => self.on_level_read(&id, hw_pct),
            EngineNotification::PlatformWake => self.on_platform_wake(),
        }
    }

    /// Re-assert every gamma ramp after an OS event that may have dropped them.
    ///
    /// ADR-0003 makes re-apply-on-wake a **precondition** for offering gamma at
    /// all on macOS (*"opt-in … only where verified safe (Windows SDR, macOS with
    /// re-apply-on-wake, wlroots)"*), and the macOS gamma sink shipped without it.
    /// Windows needs it too: the same ADR records that its ramp *"is reset by
    /// display events"*.
    ///
    /// Both halves are load-bearing. [`gamma::GammaCoordinator::invalidate`]
    /// alone would change nothing until something else happened to trigger a
    /// batch, and a resume that changes no display produces no snapshot — which
    /// is exactly the case that was broken. So this re-plans as well.
    ///
    /// The HDR verdict is re-probed first. Toggling Windows HDR raises
    /// `WM_DISPLAYCHANGE` but usually does **not** change the display set, so
    /// without this the forced rewrite would push a ramp at a now-HDR display on
    /// a stale `gamma_allowed` — the one thing ADR-0003 says never to do. This is
    /// the only apply site that *forces* a write where the ordinary diff would
    /// have skipped it, which is what makes the stale verdict reachable here and
    /// inert elsewhere. [`GAMMA_VERDICT_TTL`] throttles an event burst to one
    /// probe.
    ///
    /// **Cost.** One ramp write per gamma-mode display, per platform *event* —
    /// and one user-visible resume is several events, since `duja-platform` maps
    /// resume, device-arrival and `WM_DISPLAYCHANGE` onto the same tick and its
    /// own docs call the stream bursty. Deliberately not debounced: the debounce
    /// exists to coalesce enumerations, and waiting to re-assert a ramp would
    /// leave the screen undimmed for the duration.
    ///
    /// Not free even with no gamma display: `apply_overlays` re-plans and issues
    /// the overlay batch, which is a blocking round-trip to the dimmer thread.
    /// That is the same work a single slider sample already does, so it is small
    /// — but it is not nothing.
    fn on_platform_wake(&mut self) {
        self.refresh_gamma_verdict();
        self.gamma.invalidate();
        self.apply_overlays();
    }

    /// Reflect an externally-observed hardware level onto the perceptual slider.
    ///
    /// A poll saw the display's hardware brightness change from outside Duja. The
    /// engine already suppressed our own writes (it only emits `LevelRead` on a
    /// drift from what it last recorded); this second, perceptual gate additionally
    /// suppresses a reading that merely matches the hardware our *current* slider
    /// position already intends — which also covers the pinned-floor/overlay case
    /// (below the floor the hardware sits at the floor and the reading matches it),
    /// so the reflection never yanks the thumb up to the transition. A genuine
    /// external change is reflected via
    /// [`reverse_map`](duja_core::continuum::reverse_map) and updates the slider +
    /// overlays; it **never writes to hardware**.
    fn on_level_read(&mut self, id: &StableDisplayId, hw_pct: u8) {
        // The engine polls each physical panel; map the member that answered to its
        // group anchor and reflect onto the ONE merged row via the group continuum
        // (#66). A software-only group has no hardware channel, so `reflected_level`
        // returns None and it never reflects. (Residual: a mirror whose two panels
        // report divergent backlights can nudge the merged slider between them until
        // the user takes control — a physical-backlight difference, not a bug.)
        let anchor = self
            .groups
            .anchor_of(id)
            .cloned()
            .unwrap_or_else(|| id.clone());
        let Some((kind, software_only)) = self.group_meta(&anchor) else {
            return;
        };
        let cfg = settings::continuum_for(
            kind,
            software_only,
            &settings::monitor_config(&self.config, anchor.as_str()),
            self.gamma_allowed,
        );
        let current = self
            .state
            .level(anchor.as_str())
            .unwrap_or(DEFAULT_USER_LEVEL_PCT);
        let Some(perceived) = reflected_level(current, hw_pct, cfg) else {
            return;
        };
        self.state.record(anchor.as_str(), perceived, unix_now());
        self.vm.borrow_mut().set_level(&anchor, perceived);
        self.render();
        // Re-plan overlays: an external change that crosses the transition must
        // clear/adjust any overlay a user-controlled display was showing.
        self.apply_overlays();
        let _ = self.state.maybe_flush(Instant::now());
    }

    /// Fold a per-member responsive/unresponsive notification into the merged row.
    ///
    /// The engine tracks health per physical panel, but the flyout shows one row per
    /// clone group: grey the merged (anchor) row only when EVERY member is
    /// unresponsive, and keep it live the moment any member answers (#66). An
    /// ungrouped id greys its own row directly.
    fn on_member_responsive(&mut self, id: &StableDisplayId, responsive: bool) {
        if responsive {
            self.unresponsive.remove(id);
        } else {
            self.unresponsive.insert(id.clone());
        }
        // Resolve to the group and aggregate before touching the view-model, so the
        // group/unresponsive borrows end before the `vm` borrow.
        let target = self.groups.group_of(id).map(|group| {
            (
                group.anchor.clone(),
                clone_group::all_unresponsive(&group.members, &self.unresponsive),
            )
        });
        match target {
            Some((anchor, all_down)) => self.vm.borrow_mut().set_unresponsive(&anchor, all_down),
            None => self.vm.borrow_mut().set_unresponsive(id, !responsive),
        }
        self.render();
    }

    /// Re-probe the live HDR gamma verdict into
    /// [`gamma_allowed`](Self::gamma_allowed) so it is not frozen at process
    /// start.
    ///
    /// If a display is SDR at launch and the user later turns Windows HDR on,
    /// this flips the verdict to `false`; every read site (`plan_commands`,
    /// `hardware_target`, `continuum_for`) then routes its gamma-configured
    /// displays onto the overlay path, so `SetDeviceGammaRamp` is never issued
    /// under HDR — where the legacy ramp is bypassed and the write would only
    /// leave an ineffective dim behind a crash marker. When the verdict flips the
    /// other way (HDR lost), the same path simply re-allows gamma.
    ///
    /// The DXGI probe is throttled to at most once per [`GAMMA_VERDICT_TTL`]: the
    /// caller ([`on_displays_changed`](Self::on_displays_changed)) also runs on a
    /// `SetUserLevel` echo (a slider drag), so an unthrottled probe would land a
    /// factory-create on the drag hot path. A single global verdict is kept for
    /// now (per-display HDR is a future refinement).
    fn refresh_gamma_verdict(&mut self) {
        let now = Instant::now();
        if !verdict_probe_due(self.last_gamma_probe, now, GAMMA_VERDICT_TTL) {
            return;
        }
        self.last_gamma_probe = Some(now);
        self.gamma_allowed =
            duja_dimmer::gamma_support_from_hdr(duja_dimmer::is_hdr_active()).allows_gamma();
    }

    /// Rebuild the clone grouping from a fresh enumeration, migrating group control
    /// across an anchor move and pruning stale per-member ids (#66).
    ///
    /// Groups are keyed for state/overlay on their anchor (the lowest-id member),
    /// which is stable over enumeration echoes but **not** over a membership change:
    /// plugging a lower-id clone into a dimmed mirror, or unplugging the current
    /// anchor, re-anchors the group and would orphan the anchor-keyed level +
    /// user-controlled flag — silently snapping the user's dim back. Keyed on the
    /// stable shared GDI device, [`clone_group::migrate_group_control`] transfers the
    /// old anchor's level + control to the new anchor. This runs BEFORE adoption (so
    /// adopt skips the now-controlled anchor) and before the row/plan build (so
    /// `plan_commands` re-emits the overlay at the migrated level). The per-member
    /// `unresponsive`/`user_controlled` sets are then pruned of ids no longer present
    /// — after the migration inserts, so a just-migrated (current-member) anchor is
    /// kept — bounding their growth over a long session.
    fn rebuild_groups(&mut self, snapshots: &[DisplaySnapshot]) {
        // Snapshot each display's device under the bounds lock — the enumerator
        // populated the bounds map BEFORE this DisplaysChanged fired (engine
        // `refresh` runs the enumerator, which writes the map, then reconciles and
        // notifies), so `surface_token_for` is fresh here. A transiently-`None` token
        // degrades gracefully: that panel becomes its own singleton (two rows for one
        // frame) and converges on the next pass, never stranding an overlay.
        //
        // One `None` is permanent rather than transient — a macOS panel whose
        // CGDisplayBounds is degenerate reports no geometry at all — and it degrades
        // the same way, minus the converging: a lone row that cannot be software
        // dimmed. That is deliberate, and dropping the token *with* the bounds is
        // what stops such a panel anchoring a mirror group it can no longer place.
        // See `duja-panel`'s `panel_geometry` and `backend::DisplayGeom`.
        let members: Vec<clone_group::GroupMember> = {
            let guard = self.bounds.lock().ok();
            snapshots
                .iter()
                .map(|s| clone_group::GroupMember {
                    id: s.id.clone(),
                    kind: s.kind,
                    software_only: s.software_only,
                    device: guard.as_ref().and_then(|b| b.surface_token_for(&s.id)),
                    name: s.name.clone(),
                })
                .collect()
        };
        let new_groups = clone_group::group_clones(&members);
        let migrations = clone_group::migrate_group_control(
            &self.groups,
            &new_groups,
            &self.user_controlled,
            |id| {
                self.state
                    .level(id.as_str())
                    .unwrap_or(DEFAULT_USER_LEVEL_PCT)
            },
        );
        self.groups = new_groups;
        for (anchor, level) in migrations {
            self.state.record(anchor.as_str(), level, unix_now());
            self.user_controlled.insert(anchor.as_str().to_owned());
        }
        self.unresponsive
            .retain(|id| members.iter().any(|m| &m.id == id));
        self.user_controlled
            .retain(|id| members.iter().any(|m| m.id.as_str() == id.as_str()));
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
    /// placeholder (see [`adopt_position`](super::policy::adopt_position)). Only a
    /// genuine user action
    /// ([`set_user_level`](Self::set_user_level)) writes to hardware thereafter.
    fn on_displays_changed(&mut self, snapshots: &[DisplaySnapshot]) {
        // Keep the HDR gamma verdict live across the session — this is the app's
        // enumeration path (a hot-plug, or a resume/session-unlock re-enumeration
        // forwarded by the engine as this notification). Throttled internally, so
        // a slider-drag echo that also lands here never hammers the DXGI probe.
        self.refresh_gamma_verdict();
        self.displays = snapshots
            .iter()
            .map(|s| (s.id.clone(), s.kind, s.software_only))
            .collect();
        // Keep the full snapshots (with capabilities) for the settings sections,
        // and refresh the (possibly-open) settings window's per-monitor list.
        self.snapshots = snapshots.to_vec();
        // Rebuild the clone grouping, migrating control across any anchor move and
        // pruning stale per-member ids (#66) — before adoption + the row/plan build.
        self.rebuild_groups(snapshots);
        self.settings_vm.borrow_mut().set_displays(
            &self.snapshots,
            &self.config,
            self.gamma_allowed,
            settings::platform_gamma_limits(),
        );
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());

        let now = Instant::now();
        // Adopt reality: seed the state book from each display's live hardware
        // reading (persisted last-known only as a placeholder fallback), for the
        // displays the user has not taken control of. This writes NOTHING to the
        // hardware — the former `SetUserLevel` push here is what dimmed the monitor
        // to the last-saved level on every launch (item 5).
        // Precompute each display's continuum config before the mutable `state`
        // borrow (a closure capturing `&self` would conflict with `&mut state`).
        // Adopt through the GROUP continuum (#66): a mixed group whose anchor is the
        // hardware member still applies as software-only, so the anchor's initial
        // slider position must be reflected through the group's software-only verdict
        // — not the anchor panel's own — or the adopted level would not match how the
        // group actually dims. `group_meta` yields the group's `(kind, software_only)`
        // (falling back to the panel's own when it is not grouped).
        let cfgs: Vec<ContinuumConfig> = snapshots
            .iter()
            .map(|s| {
                let (kind, software_only) =
                    self.group_meta(&s.id).unwrap_or((s.kind, s.software_only));
                settings::continuum_for(
                    kind,
                    software_only,
                    &settings::monitor_config(&self.config, s.id.as_str()),
                    self.gamma_allowed,
                )
            })
            .collect();
        adopt_enumeration(
            snapshots,
            &self.user_controlled,
            &cfgs,
            &mut self.state,
            unix_now(),
        );

        // Rebuild the flyout rows: ONE merged row per clone group (a mirrored set is
        // one control, #66). Each row is the anchor's snapshot re-labelled with the
        // merged name, carrying the group's aggregated software-only flag and the
        // *user* level recorded under the anchor (not the engine's hardware echo).
        let rows: Vec<DisplaySnapshot> = self
            .groups
            .groups()
            .iter()
            .filter_map(|group| {
                let anchor = snapshots.iter().find(|s| s.id == group.anchor)?;
                Some(DisplaySnapshot {
                    name: group.name.clone(),
                    software_only: group.software_only,
                    user_level_pct: self
                        .state
                        .level(group.anchor.as_str())
                        .unwrap_or(anchor.user_level_pct),
                    ..anchor.clone()
                })
            })
            .collect();
        self.vm.borrow_mut().set_displays(rows);
        // Re-assert each merged row's greyed state from the per-member unresponsive
        // set, so a grouping rebuilt with a CHANGED anchor (its lowest-id member
        // unplugged) still greys correctly — the merged row greys only when every
        // current member is unresponsive.
        let greyed: Vec<(StableDisplayId, bool)> = self
            .groups
            .groups()
            .iter()
            .map(|group| {
                (
                    group.anchor.clone(),
                    clone_group::all_unresponsive(&group.members, &self.unresponsive),
                )
            })
            .collect();
        for (anchor, all_down) in greyed {
            self.vm.borrow_mut().set_unresponsive(&anchor, all_down);
        }
        self.refresh_flyout_dimming();
        self.render();
        // Keep the content-driven height current if the flyout is open (the row
        // count may have changed), re-asserting the logical size so the buffer
        // tracks it.
        if self.flyout_visible {
            // Re-assert the content-driven height, keeping the work-area cap: this
            // fires on every SetUserLevel (a slider drag) and every enumeration, so
            // without the cap a drag/refresh while open would push the capped
            // window back to full height and overflow a small/high-DPI screen.
            let logical_height = self.capped_flyout_height();
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

    /// Compute and apply the full dimming batch — overlays **and** the gamma
    /// channel — for every known display.
    ///
    /// Overlays and gamma are the two halves of one declarative batch: the
    /// overlay backend diffs the alpha channel, while [`gamma::GammaBackend`]
    /// engages/restores the GPU ramp for the (opt-in, SDR-only) gamma channel.
    /// HDR/unknown displays never carry a gamma factor here — `effective_mode`
    /// forces them onto the overlay path — so they can never reach the ramp. Nor
    /// does a factor this platform's OS would refuse: [`dimming::plan_for_platform`]
    /// plans an overlay below `duja_dimmer::min_gamma_factor()`, so the ramp is only
    /// ever asked for what it can deliver.
    ///
    /// Because a display can therefore **switch mechanism** mid-drag, the ordering
    /// of the two halves is load-bearing and is owned by
    /// [`gamma::apply_dimming_batch`] (engage new ramps → overlay diff → restore
    /// stale ramps), whose tests pin it on every lane. Doing the overlay first would
    /// destroy it to completion before the ramp engaged and flash the screen bright.
    ///
    /// [`gamma::apply_dimming_batch`]: crate::bin_support::gamma::apply_dimming_batch
    /// # A permanently unavailable backend is retired, not retried
    ///
    /// [`DimmerError::Unsupported`](duja_core::dimmer::DimmerError::Unsupported) means the backend has decided it can no
    /// longer dim *at all* — the X11 one latches it when the compositing manager
    /// dies, because mapping a fresh window onto a session that cannot blend it
    /// would paint the screen black. That never un-latches, so the dimmer is
    /// dropped rather than asked again: this runs once per slider sample, and a
    /// condition that will never change must not produce a warning per sample.
    /// One line, then software dimming is off and hardware control carries on.
    fn apply_overlays(&mut self) {
        let commands = self.plan_commands();
        let overlays = self
            .dimmer
            .as_mut()
            .map(|dimmer| dimmer as &mut dyn duja_core::dimmer::Dimmer);
        if let Err(e) = self.gamma.apply_batch(&commands, overlays) {
            let retire = retires_dimmer(&e);
            warn!(error = %e, retire, "overlay apply failed");
            if retire {
                self.dimmer = None;
            }
        }
    }

    /// Build the declarative overlay command batch (pure; borrows `&self`).
    ///
    /// Only displays the user has taken control of this session get an overlay/
    /// gamma command; an untouched display is left at reality (no dimming) — Duja
    /// never restores an overlay/gamma on launch, it adopts the current screen
    /// (item 5). The batch is a diff, so an absent display is simply not dimmed.
    fn plan_commands(&self) -> Vec<DimCommand> {
        // One input per user-controlled GROUP, keyed on its anchor (#66): a mirrored
        // set shares one GDI surface, so it must emit exactly one overlay/gamma
        // command — feeding both members (at identical bounds) would stack two
        // overlays on the same pixels. The anchor's bounds are the shared surface.
        let inputs: Vec<DisplayInput> = self
            .groups
            .groups()
            .iter()
            .filter(|group| self.user_controlled.contains(group.anchor.as_str()))
            .map(|group| DisplayInput {
                id: group.anchor.clone(),
                kind: group.kind,
                software_only: group.software_only,
                user_pct: self.state.level(group.anchor.as_str()).unwrap_or(100),
            })
            .collect();
        let guard = self.bounds.lock().ok();
        // `plan_for_platform`, not `plan`: the *choice* of gamma minimum belongs
        // in the cross-platform module where a test can observe it, not at this
        // (untestable) call site. See its doc.
        let plan = dimming::plan_for_platform(
            &inputs,
            |d| {
                settings::continuum_for(
                    d.kind,
                    d.software_only,
                    &settings::monitor_config(&self.config, d.id.as_str()),
                    self.gamma_allowed,
                )
            },
            |id| guard.as_ref().and_then(|b| b.bounds_for(id)),
        );
        plan.commands
    }

    /// The per-member hardware writes a user level on the group anchored at
    /// `anchor` fans out to.
    ///
    /// The group continuum maps the level to a hardware target, then
    /// [`clone_group::fan_out_hardware`] applies the group rule: an all-hardware
    /// group drives every member to that target; a software-only group pins its
    /// hardware-capable members to MAX (100) so the single shared overlay is the
    /// sole uniform dimmer (never a partial hardware level that double-dims). Falls
    /// back to the lone display when `anchor` is not (yet) in a group.
    fn group_hardware_writes(
        &self,
        anchor: &StableDisplayId,
        pct: u8,
    ) -> Vec<(StableDisplayId, u8)> {
        match self.groups.group_of(anchor) {
            Some(group) => {
                let cfg = settings::continuum_for(
                    group.kind,
                    group.software_only,
                    &settings::monitor_config(&self.config, anchor.as_str()),
                    self.gamma_allowed,
                );
                let out = map_user_level(pct, &cfg);
                clone_group::fan_out_hardware(&group.members, out.hardware_pct)
            }
            None => match self.meta_of(anchor) {
                Some((kind, software_only)) => {
                    vec![(
                        anchor.clone(),
                        self.hardware_target(kind, software_only, anchor.as_str(), pct),
                    )]
                }
                None => Vec::new(),
            },
        }
    }

    /// The physical class and aggregated software-only flag of the group a display
    /// id belongs to, falling back to the per-panel [`meta_of`](Self::meta_of) when
    /// the id is not (yet) in a group. Group-level policy (continuum, clamp,
    /// reflection) reads this so it uses the merged software-only verdict, not one
    /// member's.
    fn group_meta(&self, id: &StableDisplayId) -> Option<(DisplayKind, bool)> {
        self.groups
            .group_of(id)
            .map(|group| (group.kind, group.software_only))
            .or_else(|| self.meta_of(id))
    }

    /// The engine hardware target for a user level (continuum-floored).
    fn hardware_target(
        &self,
        kind: DisplayKind,
        software_only: bool,
        id: &str,
        user_pct: u8,
    ) -> u8 {
        let cfg = settings::continuum_for(
            kind,
            software_only,
            &settings::monitor_config(&self.config, id),
            self.gamma_allowed,
        );
        map_user_level(user_pct, &cfg)
            .hardware_pct
            .unwrap_or(user_pct)
    }

    /// The physical class and software-only flag of a known display id.
    fn meta_of(&self, id: &StableDisplayId) -> Option<(DisplayKind, bool)> {
        self.displays
            .iter()
            .find(|(known, _, _)| known == id)
            .map(|(_, kind, software_only)| (*kind, *software_only))
    }
}

/// Whether a failed overlay apply means the backend will never work again.
///
/// [`DimmerError::Unsupported`](duja_core::dimmer::DimmerError::Unsupported) is the backend saying it *cannot* dim, not that
/// it failed to: the X11 one latches it when the compositing manager dies,
/// because from then on a mapped overlay would be an opaque black rectangle
/// rather than a dim one. Nothing un-latches that, so the dimmer is dropped.
///
/// The other two are deliberately **not** retiring, and that is the whole reason
/// this is a function rather than an inline `matches!`. `Os` is one call that
/// failed and `Backend` is a worker that missed its reply budget; both are
/// transient, and treating either as terminal would turn a single hiccup during a
/// drag into software dimming disabled for the rest of the session.
const fn retires_dimmer(error: &duja_core::dimmer::DimmerError) -> bool {
    matches!(error, duja_core::dimmer::DimmerError::Unsupported)
}

/// Building an [`AppState`] in a test, which four debt rows deferred on.
///
/// # Why a fixture rather than one constructor
///
/// `docs/debt.md` D-016, D-040, D-059 and D-065 all defer on the same sentence:
/// *"`AppState` cannot be constructed in a test: it owns two live Slint shells
/// and a concrete `tray_icon::TrayIcon` whose only constructor does
/// `CreateWindowExW` + `Shell_NotifyIconW`."* Both halves were false by the time
/// this landed, and they became false in different ways - the second one is the
/// trap:
///
/// - **The Slint half was never true.** `duja-ui` has been instantiating both
///   shells headless in its own suite since before three of those rows existed,
///   through `i_slint_backend_testing::init_no_event_loop` - under its `smoke`
///   feature, which CI's `--all-features` turns on and a bare `cargo test` does
///   not. The fixture here is deliberately not gated that way: a seam four debt
///   rows waited on should not need a flag to exercise.
/// - **The tray half stopped being true, and then the opposite problem
///   appeared.** `#134` replaced the three `tray-icon` handles with one
///   `PlatformTray`, and D-102's experiment then showed `build_tray`
///   **succeeds** in a test process. That is worse than a refusal, not better: a
///   test that built a real tray would put a real Duja icon in the real
///   notification area and answer differently per session. So the way in is
///   [`PlatformTray::fake`], not a real one.
///
/// # What is real here and what is not
///
/// Three things here reach an OS and all are bounded. `tempfile::tempdir()`
/// creates a real directory, which the `Harness` drops last so the files under
/// it outlive everything that writes them. The Slint shells go through the
/// headless backend. `OsHotkeyRegistrar::new` builds a real
/// `GlobalHotKeyManager` on an interactive session - it does *not* merely
/// degrade to `None`, which an earlier draft of this paragraph implied - but it
/// registers nothing, because `register()` is never called, and it drops with
/// the fixture.
///
/// **A third thing did reach one, and had to be given a seam.** Windows' update
/// toast is a real `ToastNotification` under the `AppUserModelID` the installer
/// stamps on the Start-Menu shortcut. Before `toast`'s `cfg!(test)` diversion
/// existed, these tests put four fabricated "Duja update available"
/// notifications into the operator's Action Center on every `cargo test`. A
/// review found it, not anything that could fail - and the useful part is that
/// the tray had just been given a fake for this exact hazard while the call
/// beside it was walked straight past.
///
/// In particular the **gamma sink is real**, and is made
/// inert the only way that is honest: its `resolve` closure answers `None` for
/// every id, which is the sink's own documented "no device for this display"
/// path. It returns `false` before any OS call, writes no crash marker and
/// touches no ramp. So a test drives the same `GammaBackend` the app does, and
/// the reason nothing lands on a screen is a rule the sink already had rather
/// than a branch added for tests.
#[cfg(test)]
pub(super) mod fixture {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use crossbeam_channel::Receiver;

    use duja_app::EngineCommand;
    use duja_core::config::Config;
    use duja_ui::{FlyoutShell, FlyoutVm, SettingsShell, SettingsVm};

    use crate::bin_support::bounds::BoundsMap;
    use crate::bin_support::clone_group::CloneGrouping;
    use crate::bin_support::gamma;
    use crate::bin_support::level_forward::{EngineLevelSink, LevelForwarder};
    use crate::bin_support::state_store::StateStore;

    use super::super::hotkey_os::OsHotkeyRegistrar;
    use super::super::surface::PlatformTray;
    use super::AppState;

    /// A built fixture: the state, plus the ends of every channel a test watches.
    pub(in crate::bin_support::tray) struct Harness {
        /// The state under test.
        pub(in crate::bin_support::tray) app: AppState,
        /// Everything `AppState` sent the engine, in order.
        ///
        /// Held rather than dropped: a dropped receiver turns every `send` into
        /// an `Err` the app ignores, and the test would then be asserting against
        /// a channel nobody could have delivered on.
        pub(in crate::bin_support::tray) engine_rx: Receiver<EngineCommand>,
        /// Keeps the config/state/marker directory alive for the fixture's life.
        /// Never read; dropping it deletes the files underneath.
        _dir: tempfile::TempDir,
    }

    /// Install the headless Slint backend once **per thread**.
    ///
    /// Per thread, not per process, and that distinction is the whole reason
    /// this is a function rather than a bare call at the top of each test.
    /// `init_no_event_loop` binds the platform to its *calling* thread, and a
    /// shell built on any other one fails with "The Slint platform was
    /// initialized in another thread". That string is what a `Once` here
    /// actually produces, reproduced by swapping the latch back and running the
    /// suite. A review round replaced it with a different message and labelled
    /// the replacement "measured"; it was not, and the correction was the
    /// defect. That is worth leaving written down, because this file argues at
    /// length that a claim reading as verified and not being so is the expensive
    /// kind.
    ///
    /// `cargo test` runs a binary's tests on
    /// several threads of one process, so a `std::sync::Once` here - which is
    /// what the first version of this used - initialises for whichever test ran
    /// first and breaks every subsequent one. `cargo nextest`, which CI runs,
    /// gives each test its own process and would have hidden that entirely: the
    /// suite would have been green on CI and red on a developer's machine, which
    /// is the worse direction for a fixture four debt rows are waiting on.
    ///
    /// A thread-local latch is correct under both runners. Each thread
    /// initialises exactly once, so a test that builds two fixtures does not
    /// double-init and a test on a fresh thread is not locked out of a platform
    /// it never received.
    fn slint_backend() {
        thread_local! {
            static READY: Cell<bool> = const { Cell::new(false) };
        }
        READY.with(|ready| {
            if !ready.replace(true) {
                i_slint_backend_testing::init_no_event_loop();
            }
        });
    }

    /// An [`AppState`] with a recording tray, an inert gamma sink, no overlay
    /// dimmer, and a temporary directory for its config, state and crash marker.
    ///
    /// # Panics
    /// If a Slint shell cannot be built headless or the temporary directory
    /// cannot be made. Both mean the fixture is broken rather than the code under
    /// test, and a silent skip would read as a pass.
    pub(in crate::bin_support::tray) fn harness(config: Config) -> Harness {
        slint_backend();
        let dir = tempfile::tempdir().expect("a temp dir for the fixture");

        let vm = Rc::new(RefCell::new(FlyoutVm::new()));
        let shell = FlyoutShell::new(Rc::clone(&vm)).expect("a headless flyout shell");
        let settings_vm = Rc::new(RefCell::new(SettingsVm::new()));
        let settings_shell =
            SettingsShell::new(Rc::clone(&settings_vm)).expect("a headless settings shell");

        let (engine_tx, engine_rx) = crossbeam_channel::unbounded::<EngineCommand>();

        let app = AppState {
            shell,
            vm,
            settings_shell,
            settings_vm,
            autostart: None,
            config_path: dir.path().join("config.toml"),
            snapshots: Vec::new(),
            // No overlay backend. `apply_overlays` then passes `None` through to
            // `apply_dimming_batch`, which is the same shape as a machine whose
            // dimmer was retired - a supported state, not a crippled fixture.
            dimmer: None,
            config,
            gamma_allowed: true,
            last_gamma_probe: None,
            bounds: Arc::new(Mutex::new(BoundsMap::default())),
            state: StateStore::load(dir.path().join("state.toml")),
            crash_marker: dir.path().join("crash.marker"),
            engine_tx: engine_tx.clone(),
            levels: LevelForwarder::new(EngineLevelSink::new(engine_tx)),
            // Real coordinator, real sink, and a resolver that answers `None` for
            // every id - see this module's header for why that is the honest way
            // to make it inert.
            gamma: gamma::GammaBackend::new(dir.path().join("gamma.marker"), |_| None),
            displays: Vec::new(),
            groups: CloneGrouping::default(),
            unresponsive: BTreeSet::new(),
            user_controlled: BTreeSet::new(),
            flyout_visible: false,
            last_hidden: None,
            hotkeys: OsHotkeyRegistrar::new(),
            hotkey_outcomes: BTreeMap::new(),
            tray: PlatformTray::fake(),
            update_available: None,
            update_check_in_flight: false,
        };
        Harness {
            app,
            engine_rx,
            _dir: dir,
        }
    }

    /// Every `SetUserLevel` sent so far, in order, as `(id, pct)`.
    ///
    /// Drains without blocking, so a test reads "what has been sent by now"
    /// rather than waiting on a channel nothing is going to close.
    pub(in crate::bin_support::tray) fn levels_sent(
        rx: &Receiver<EngineCommand>,
    ) -> Vec<(String, u8)> {
        let mut seen = Vec::new();
        while let Ok(command) = rx.try_recv() {
            if let EngineCommand::SetUserLevel { id, pct } = command {
                seen.push((id.as_str().to_owned(), pct));
            }
        }
        seen
    }
}

/// The app layer between the two ends everything else already pins.
///
/// `docs/debt.md` D-040 is precise about the gap, and the value of these tests
/// is exactly its shape. The throttle-final-value contract - P4 gate Finding 1,
/// where a leading-edge UI throttle dropped the *final* sample of a slider drag
/// and stranded the hardware mid-drag - is pinned at the `duja-ui` end by
/// `slider_drag_burst_emits_the_released_value_last`, and at the engine end by
/// `write_min_gap`'s last-wins coalescing. Between them sat
/// [`AppState::on_ui_command`]'s `SetLevel` arm and
/// [`AppState::set_user_level`], and **a throttle re-added at either one passed
/// the entire suite**. `LevelForwarder`'s own tests cannot see it: the forwarder
/// is downstream of both.
#[cfg(test)]
mod level_path_tests {
    use duja_core::config::Config;
    use duja_core::id::StableDisplayId;
    use duja_core::model::DisplayKind;
    use duja_ui::UiCommand;

    use super::fixture::{harness, levels_sent};

    /// A stable id, built the way the rest of this file's tests build one.
    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).expect("a valid id")
    }

    /// A display the app knows about, so `meta_of` can answer and a level has
    /// somewhere to go.
    fn known_display(app: &mut super::AppState, serial: &str) {
        app.displays
            .push((id(serial), DisplayKind::ExternalDdc, false));
    }

    /// The samples a slider drag produces: several in flight, then the one the
    /// user released on. The released value is deliberately **not** the extreme,
    /// so a throttle that happened to keep the minimum would still red.
    const DRAG: [u8; 6] = [90, 74, 61, 48, 39, 55];

    /// Every sample of a drag reaches the engine, and the released one is last.
    ///
    /// **Proven red at the historical site.** Re-inserting a leading-edge
    /// throttle into [`AppState::set_user_level`] - guarding the
    /// `self.levels.forward(&writes)` call on an elapsed-since-last-write test,
    /// which is the shape P4 shipped - leaves one of the six samples standing:
    ///
    /// ```text
    /// every sample must be forwarded [...]: [("GSM-0001-A", 87)]
    ///   left: 1
    ///  right: 6
    /// ```
    ///
    /// The **count** is the assertion that catches every shape of throttle,
    /// including the historical one: a dropped trailing edge is five samples
    /// where six were sent. The last-value check is defence in depth rather than
    /// an independent guard - an earlier draft of this comment sold it as
    /// catching a case the count could not, and it does not. What it is for is a
    /// future throttle that coalesces *without* changing the count, and it costs
    /// two lines. The released sample is deliberately not the drag's extreme for
    /// the same reason: a throttle that happened to keep the minimum would look
    /// right on value alone.
    #[test]
    fn a_slider_drag_forwards_every_sample_and_the_released_value_last() {
        let mut h = harness(Config::default());
        known_display(&mut h.app, "A");

        for pct in DRAG {
            h.app.set_user_level(&id("A"), pct);
        }

        let sent = levels_sent(&h.engine_rx);
        assert_eq!(
            sent.len(),
            DRAG.len(),
            "every sample must be forwarded - there is no UI-side throttle, and \
             the engine's own `write_min_gap` is what bounds the write rate: {sent:?}"
        );
        // The hardware target is the continuum-mapped value rather than the user
        // percentage, so compare against what the app computes for the released
        // sample rather than against 55. Pinning the raw number here would red
        // whenever the continuum is retuned, which is a different decision with
        // tests of its own.
        let released = h.app.group_hardware_writes(&id("A"), 55);
        let expected: Vec<(String, u8)> = released
            .into_iter()
            .map(|(display, pct)| (display.as_str().to_owned(), pct))
            .collect();
        assert_eq!(
            sent.last().map(|(display, pct)| (display.clone(), *pct)),
            expected.first().cloned(),
            "the value the user released on must be the last one the engine sees"
        );
    }

    /// The same contract through the arm the flyout actually calls.
    ///
    /// [`AppState::on_ui_command`] is the *other* half of D-040's gap and is a
    /// separate site: a throttle added to its `SetLevel` arm would leave
    /// `set_user_level` untouched and the test above green. `#82`'s rule is that
    /// a defect must be re-inserted where it historically occurred, and this
    /// contract has two such places, so it gets two tests rather than one that
    /// reaches the deeper site through the shallower one.
    #[test]
    fn the_ui_command_arm_forwards_every_sample_too() {
        let mut h = harness(Config::default());
        known_display(&mut h.app, "A");

        for pct in DRAG {
            h.app
                .on_ui_command(UiCommand::SetLevel { id: id("A"), pct });
        }

        assert_eq!(
            levels_sent(&h.engine_rx).len(),
            DRAG.len(),
            "`on_ui_command`'s SetLevel arm must not coalesce either"
        );
    }

    /// A level for a display nothing knows about sends nothing, rather than a
    /// write the engine would have to discard.
    ///
    /// Pinned because it is what makes the tests above mean anything: if
    /// `group_hardware_writes` answered for every id, a fixture with no displays
    /// would still look busy and those assertions would be counting noise.
    #[test]
    fn an_unknown_display_forwards_nothing() {
        let mut h = harness(Config::default());
        h.app.set_user_level(&id("ghost"), 40);
        assert!(levels_sent(&h.engine_rx).is_empty());
    }

    /// Driving a level marks the display user-controlled, which is what lets its
    /// overlay engage and stops the next enumeration re-adopting the hardware
    /// value over the top (item 5).
    #[test]
    fn setting_a_level_takes_control_of_the_display() {
        let mut h = harness(Config::default());
        known_display(&mut h.app, "A");
        assert!(h.app.user_controlled.is_empty());

        h.app.set_user_level(&id("A"), 60);

        let key = id("A");
        assert!(h.app.user_controlled.contains(key.as_str()));
        assert_eq!(h.app.state.level(key.as_str()), Some(60));
    }
}

#[cfg(test)]
mod tests {
    use duja_core::dimmer::DimmerError;

    use super::retires_dimmer;

    /// The X11 backend latches `Unsupported` when its compositing manager dies,
    /// and nothing un-latches it. Asking again would warn once per slider sample
    /// forever for a condition that cannot change.
    #[test]
    fn an_unsupported_backend_is_retired() {
        assert!(retires_dimmer(&DimmerError::Unsupported));
    }

    /// The direction that matters more. A single failed X call or one missed
    /// reply budget during a drag must not disable software dimming for the rest
    /// of the session — which is exactly what retiring on these would do.
    #[test]
    fn a_transient_failure_keeps_the_backend() {
        assert!(!retires_dimmer(&DimmerError::Os(
            "one call failed".to_owned()
        )));
        assert!(!retires_dimmer(&DimmerError::Backend));
    }

    /// The app-level apply rule for merged clone groups (#66): the exact
    /// composition `set_user_level` performs on a group — group continuum →
    /// `map_user_level` → `fan_out_hardware`. `AppState` cannot be built off a live
    /// Slint/tray thread, so (as with the other tray-level pure tests) these drive
    /// the underlying functions the method calls rather than the method itself.
    mod clone_group_apply {
        use crate::bin_support::clone_group::{GroupMember, fan_out_hardware, group_clones};
        use crate::bin_support::settings::continuum_for;
        use duja_core::config::{DimMode as ConfigDimMode, MonitorConfig};
        use duja_core::continuum::map_user_level;
        use duja_core::id::StableDisplayId;
        use duja_core::model::DisplayKind;

        fn id(serial: &str) -> StableDisplayId {
            StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
        }

        /// A member on the shared GDI source `\\.\display1` (so the set mirrors).
        fn member(serial: &str, kind: DisplayKind, software_only: bool) -> GroupMember {
            GroupMember {
                id: id(serial),
                kind,
                software_only,
                device: Some(r"\\.\display1".to_owned()),
                name: serial.to_owned(),
            }
        }

        fn monitor(floor: u8) -> MonitorConfig {
            MonitorConfig {
                hw_floor_pct: floor,
                dim_mode: ConfigDimMode::Overlay,
                ..MonitorConfig::default()
            }
        }

        #[test]
        fn software_only_group_pins_hardware_members_to_max_and_is_one_surface() {
            // A mixed mirror: one hardware clone + one software-only clone on one GDI
            // source. The group is software-only, so a slider change pins the hardware
            // clone to MAX (100) — never a partial hardware level — and the ONE shared
            // overlay does all the dimming. (Pre-fix, two per-panel rows each drove
            // their own hardware + overlay, double-dimming the shared pixels.)
            let members = vec![
                member("A", DisplayKind::ExternalDdc, false),
                member("B", DisplayKind::InternalPanel, true),
            ];
            let grouping = group_clones(&members);
            let group = grouping.group_of(&id("A")).expect("A grouped");
            assert!(
                group.software_only,
                "any software-only member ⇒ software-only group"
            );
            let cfg = continuum_for(group.kind, group.software_only, &monitor(20), true);
            let out = map_user_level(30, &cfg);
            assert_eq!(
                out.hardware_pct, None,
                "a software-only group has no hardware channel"
            );
            let writes = fan_out_hardware(&group.members, out.hardware_pct);
            assert_eq!(
                writes,
                vec![(id("A"), 100)],
                "hardware clone pinned to MAX; software clone skipped"
            );
            // Exactly one group ⇒ plan_commands emits one overlay for the surface.
            assert_eq!(grouping.groups().len(), 1);
        }

        #[test]
        fn all_hardware_group_writes_every_member_the_same_target() {
            // Every clone has working hardware: the level maps to a floored hardware
            // target and a slider change sends ONE SetUserLevel per member, all equal.
            let members = vec![
                member("A", DisplayKind::ExternalDdc, false),
                member("B", DisplayKind::ExternalDdc, false),
            ];
            let grouping = group_clones(&members);
            let group = grouping.group_of(&id("A")).expect("A grouped");
            assert!(!group.software_only);
            let cfg = continuum_for(group.kind, group.software_only, &monitor(0), true);
            let out = map_user_level(80, &cfg);
            let writes = fan_out_hardware(&group.members, out.hardware_pct);
            assert_eq!(writes.len(), 2, "one SetUserLevel per member");
            let target = writes.first().map(|(_, hw)| *hw);
            assert!(
                writes.iter().all(|(_, hw)| Some(*hw) == target),
                "same hardware target for the shared content"
            );
            let ids: Vec<StableDisplayId> = writes.iter().map(|(id, _)| id.clone()).collect();
            assert_eq!(ids, vec![id("A"), id("B")]);
        }
    }
}

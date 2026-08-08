//! Assembly: building the tray icon + menu, and registering every foreign event
//! source (flyout, settings, tray/menu, hotkeys, engine notifications) onto the
//! published [`AppState`].
//!
//! Every handler here is foreign — it fires from a tray/menu/OS callback or a
//! side thread — so each one hops onto the Slint loop and reaches the state
//! through [`with_app`], never a direct borrow.

#[cfg(not(target_os = "linux"))]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use duja_app::EngineNotification;
use duja_core::config::Config;
use duja_ui::{AccentChoice, HotkeyRow};

use crate::bin_support::hotkey::{self, Accelerator, HotkeyAction};

use super::hotkey_os::{
    OsHotkeyRegistrar, install_hotkey_event_handler, log_hotkey_issues, outcomes_by_action,
};
#[cfg(not(target_os = "linux"))]
use super::icon;
use super::state::AppState;
use super::surface::{OsTray, PlatformTray};
use super::{Action, with_app, with_app_ref};

/// Wire the flyout's command fan-out to the app state.
fn wire_ui_commands() {
    // Read-only setup borrow (runs once, never re-entrant): register the
    // handler, which routes every command through the re-entrancy-safe
    // [`with_app`] dispatcher.
    with_app_ref(|app| {
        app.shell.on_command(|command| {
            with_app(move |app| app.on_ui_command(command));
        });
        // Click-outside-to-dismiss: hide the flyout when it loses focus,
        // routed through the app so `flyout_visible` stays honest (bug 5).
        app.shell.on_focus_lost(|| {
            with_app(AppState::on_focus_lost);
        });
    });
}

/// Wire the settings window's command fan-out to the app state.
fn wire_settings_commands() {
    with_app_ref(|app| {
        app.settings_shell.on_command(|command| {
            with_app(move |app| app.on_settings_command(command));
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
pub(super) fn resolved_hotkey_rows(
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
            // Both states grey the row. They are separate variants because their
            // *explanations* differ and only one of them is ever true on a given
            // platform — see `RegisterResult::Unsupported`.
            let unavailable = matches!(
                outcomes.get(&action),
                Some(hotkey::RegisterResult::OsRejected | hotkey::RegisterResult::Unsupported)
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

/// Dispatch an [`Action`] onto the Slint main thread.
pub(super) fn dispatch(action: Action) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(move |app| app.handle_action(action));
    });
}

/// Register the tray-icon and menu event handlers (they hop onto the Slint loop
/// via [`dispatch`]).
///
/// No Linux counterpart, and not an omission: a `ksni` menu row *is* its
/// callback, so there is no global event stream to subscribe to and no id table
/// to match a fired item against. The Linux equivalent of this whole function is
/// the `activate` closure inside `ksni_tray`'s `menu`.
#[cfg(not(target_os = "linux"))]
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
        } else if event.id() == &ids.restart {
            Action::Restart
        } else if event.id() == &ids.update {
            Action::OpenReleases
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

#[cfg(not(target_os = "linux"))]
thread_local! {
    /// The menu item ids, captured so the (Send) menu handler can match them.
    static MENU_IDS: RefCell<MenuIds> = RefCell::new(MenuIds::default());
}

/// The tray menu item ids, for matching menu events.
#[cfg(not(target_os = "linux"))]
#[derive(Clone, Default)]
struct MenuIds {
    open: tray_icon::menu::MenuId,
    settings: tray_icon::menu::MenuId,
    restore: tray_icon::menu::MenuId,
    restart: tray_icon::menu::MenuId,
    /// The "Update available" item — its id is recorded even though the item is
    /// not in the menu until a newer release is found, so the handler routes it.
    update: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}

/// Build the tray icon with its right-click menu (Open / Settings / Restore
/// screen / Restart / Quit) plus a held-back "Update available" item.
///
/// The icon is the accent-coloured display silhouette — the same glyph and colour
/// the taskbar button carries (see [`duja_ui::icon`]).
#[cfg(not(target_os = "linux"))]
pub(super) fn build_tray(accent: AccentChoice) -> anyhow::Result<PlatformTray> {
    use tray_icon::menu::{Menu, MenuItem};
    use tray_icon::{TrayIconBuilder, menu::PredefinedMenuItem};

    let menu = Menu::new();
    let open = MenuItem::new("Open", true, None);
    let settings = MenuItem::new("Settings", true, None);
    let restore = MenuItem::new("Restore screen", true, None);
    let restart = MenuItem::new("Restart", true, None);
    let quit = MenuItem::new("Quit", true, None);
    // Built now (so its id is stable and known to the handler) but not appended:
    // it is prepended only when a background check finds a newer release.
    let update_item = MenuItem::new("Update available", true, None);
    menu.append_items(&[
        &open,
        &settings,
        &restore,
        &PredefinedMenuItem::separator(),
        &restart,
        &quit,
    ])
    .map_err(|e| anyhow::anyhow!("failed to build tray menu: {e}"))?;

    MENU_IDS.with(|cell| {
        *cell.borrow_mut() = MenuIds {
            open: open.id().clone(),
            settings: settings.id().clone(),
            restore: restore.id().clone(),
            restart: restart.id().clone(),
            update: update_item.id().clone(),
            quit: quit.id().clone(),
        };
    });

    let builder = TrayIconBuilder::new()
        // Clone shares the same `Rc` inner, so prepends on our kept handle show
        // up in the menu the tray owns.
        .with_menu(Box::new(menu.clone()))
        .with_tooltip("Duja")
        .with_icon(icon::tray_icon(duja_ui::accent::icon_rgb(accent))?);
    let tray = with_left_click_policy(builder)
        .build()
        .map_err(|e| anyhow::anyhow!("failed to create the tray icon: {e}"))?;
    // The three handles go straight into the seam and are never seen apart
    // again: `tray-icon` needs all three to do what `PlatformTray` exposes as
    // three methods, and that asymmetry is the whole reason the seam exists.
    Ok(OsTray::new(tray, menu, update_item).into())
}

/// Start the Linux tray: register a `StatusNotifierItem` and hand back the seam.
///
/// Shorter than the other arm by the whole menu, and that is the backend rather
/// than a feature gap. `ksni` takes a value and asks it for a menu whenever the
/// host wants one, so the items, their labels and their actions are all in
/// [`super::ksni_tray`]'s `menu` — there is nothing to build here and nothing to
/// keep a handle on.
#[cfg(target_os = "linux")]
pub(super) fn build_tray(accent: AccentChoice) -> anyhow::Result<PlatformTray> {
    let inner = super::ksni_tray::LinuxTray::start(duja_ui::accent::icon_rgb(accent))?;
    Ok(OsTray::new(inner).into())
}

/// Linux registers no global hotkeys, and says so rather than trying.
///
/// The plan is still resolved and still logged: a user's `[hotkeys]` section is
/// parsed, its syntax errors and conflicts are reported exactly as elsewhere, and
/// the settings window still lists the actions. What differs is that every row
/// comes back [`hotkey::RegisterResult::Unsupported`] instead of pretending to
/// have asked the OS. See `hotkey_none`'s header for why not `global-hotkey`.
#[cfg(target_os = "linux")]
pub(super) fn init_hotkeys(
    config: &Config,
) -> (
    OsHotkeyRegistrar,
    BTreeMap<HotkeyAction, hotkey::RegisterResult>,
) {
    let mut hotkeys = OsHotkeyRegistrar::new();
    let initial_plan = hotkey::resolve(&config.hotkeys);
    log_hotkey_issues(&initial_plan);
    // Deliberately the *same* `apply_plan` call the other arm makes, rather than
    // a hand-built map of `Unsupported`. The conflict rule (a combo bound to two
    // actions binds neither) is policy that holds on every platform, and a second
    // construction of this map would be a second place for it to be forgotten.
    // `apply_plan` asks the registrar what its refusals mean, and this one says
    // `Unsupported`.
    let outcomes = outcomes_by_action(&hotkey::apply_plan(&mut hotkeys, &initial_plan));
    (hotkeys, outcomes)
}

/// On macOS, stop a left click from opening the context menu, so it can toggle
/// the flyout instead.
///
/// `tray-icon` defaults `menu_on_left_click` to **true**, which on macOS means a
/// left click drops the menu and the `TrayIconEvent::Click` the flyout toggle
/// depends on never usefully arrives — the user would get the Open/Settings/Quit
/// menu where every other Duja platform gives them the brightness sliders. The
/// menu stays reachable by right click, which is the macOS convention for a status
/// item that has a primary action.
#[cfg(target_os = "macos")]
fn with_left_click_policy(builder: tray_icon::TrayIconBuilder) -> tray_icon::TrayIconBuilder {
    builder.with_menu_on_left_click(false)
}

/// Windows keeps `tray-icon`'s default.
///
/// Not because the default is obviously right, but because the shipped Windows
/// behaviour — left click toggles the flyout, right click opens the menu — has
/// been verified on real hardware with this setting untouched, and this PR is not
/// the place to change what a Windows user's left click does.
/// Windows only, now that this is spelled positively rather than as
/// `not(macos)`. That spelling was correct while `tray-icon` was the only
/// backend and Windows was the only other platform; with Linux in the build it
/// named a target where `tray_icon` is not a dependency at all.
#[cfg(windows)]
const fn with_left_click_policy(builder: tray_icon::TrayIconBuilder) -> tray_icon::TrayIconBuilder {
    builder
}

/// Build the global-hotkey registrar and apply the initial plan from `config`
/// on the (main) thread. A failure to create the manager or register a binding
/// only disables that hotkey (logged) — the app runs on. The registrar is
/// returned so the settings window can rebind and re-register it live.
#[cfg(not(target_os = "linux"))]
pub(super) fn init_hotkeys(
    config: &Config,
) -> (
    OsHotkeyRegistrar,
    BTreeMap<HotkeyAction, hotkey::RegisterResult>,
) {
    let mut hotkeys = OsHotkeyRegistrar::new();
    let initial_plan = hotkey::resolve(&config.hotkeys);
    log_hotkey_issues(&initial_plan);
    let outcomes = outcomes_by_action(&hotkey::apply_plan(&mut hotkeys, &initial_plan));
    (hotkeys, outcomes)
}

/// Wire every event source onto the published [`AppState`]: UI/settings/tray
/// handlers, the hotkey handler, the engine-notification bridge, and the first
/// background update check (startup is a one-time event, not idle, so a newer
/// release surfaces promptly on launch without ever needing a timer).
pub(super) fn wire_event_sources(notifications: crossbeam_channel::Receiver<EngineNotification>) {
    wire_ui_commands();
    wire_settings_commands();
    #[cfg(not(target_os = "linux"))]
    wire_tray_handlers();
    install_hotkey_event_handler();
    spawn_notification_bridge(notifications);
    with_app(AppState::maybe_background_update_check);
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
    //! [`D-102`]'s experiment, and nothing else.
    //!
    //! Four debt rows ([`D-016`], [`D-040`], [`D-059`], [`D-065`]) deferred on
    //! one sentence: that `AppState` "cannot be constructed in a test". D-102
    //! re-triaged that sentence and found half of it already false — `#134`
    //! removed the `tray_icon::TrayIcon` field, and `duja-ui` had been building
    //! both Slint shells headless in its own suite all along, behind its `smoke`
    //! feature. What D-102 listed
    //! as **not verified** was the remaining half: whether `build_tray` actually
    //! refuses in a test process, or whether that had only ever been assumed.
    //!
    //! This module answers exactly that and stops. It is deliberately not a step
    //! toward constructing `AppState`: that is a refactor, it wants its own PR,
    //! and D-102's whole point is that the refactor should not be planned before
    //! the measurement exists.
    //!
    //! **Three of those four rows have since drained** - D-040 on the `AppState`
    //! fixture, D-016 and D-065 on the recording gamma channel that followed it -
    //! and D-059 remains, because what it needs is to observe *when* `build_tray`
    //! ran relative to the loop rather than a constructible state.
    //!
    //! **The refactor landed and this module is unchanged by it**,
    //! which is the intended outcome. The way in was a fake behind the tray seam
    //! rather than a real tray, precisely *because* the answer here was "it
    //! succeeds" — so [`crate::bin_support::tray::state::fixture`] never calls
    //! `build_tray`, and this experiment stays what it was: a record of one
    //! measurement, run by hand, asserting nothing. Which rows drained is above.
    //!
    //! [`D-102`]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-102
    //! [`D-016`]: https://github.com/itabajah/duja/blob/main/docs/debt-archive.md#d-016
    //! [`D-040`]: https://github.com/itabajah/duja/blob/main/docs/debt-archive.md#d-040
    //! [`D-059`]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-059
    //! [`D-065`]: https://github.com/itabajah/duja/blob/main/docs/debt-archive.md#d-065

    /// Does `build_tray` succeed inside a **test process**, on a live desktop
    /// session?
    ///
    /// **Read the name literally, because an earlier one did not.** This was
    /// called `..._headless` until review, and that word named the one thing the
    /// run does not measure: it executes on an interactive session, so what it
    /// answers is "does the constructor refuse merely because its process is a
    /// test binary?" — not "does it work with no session at all", which is the
    /// CI question the four rows actually need and which nobody has measured.
    ///
    /// **`#[ignore]`d on purpose, and it must stay that way.** On Windows this
    /// reaches `CreateWindowExW` + `Shell_NotifyIconW`, so on a developer's
    /// interactive desktop a passing run puts a real Duja icon in the real
    /// notification area for as long as the value lives. That is fine to do
    /// deliberately and wrong to do on every `cargo test`; on a CI runner it is a
    /// session-dependent answer, which is the kind that turns green into noise.
    ///
    /// Run it by hand:
    ///
    /// ```text
    /// cargo test -p duja-app --bin duja -- --ignored --nocapture d102
    /// ```
    ///
    /// It asserts nothing about which way the answer goes. The outcome IS the
    /// result — a row in `docs/debt.md` is what changes, not a red bar — and an
    /// assertion here would only pin whichever session the author happened to
    /// run it in.
    #[test]
    #[ignore = "D-102 experiment: touches the real desktop session; run by hand"]
    fn d102_build_tray_in_a_test_process_on_a_live_session() {
        let outcome = super::build_tray(duja_ui::AccentChoice::default());
        let Ok(mut tray) = outcome else {
            let e = outcome.err().map(|e| format!("{e:#}")).unwrap_or_default();
            println!("D-102: build_tray REFUSED in a test process: {e}");
            return;
        };
        println!("D-102: build_tray SUCCEEDED in a test process.");

        // "Constructs" and "is usable" are different claims, and only the second
        // one is what the four rows need — a field `AppState` can hold but not
        // drive is no better than one it cannot build. So exercise all three of
        // the seam's verbs, which is the entire surface `AppState` touches.
        for (verb, result) in [
            (
                "set_accent",
                tray.set_accent(duja_ui::AccentChoice::default()),
            ),
            ("set_tooltip", tray.set_tooltip(Some("D-102 probe"))),
            ("announce_update", tray.announce_update("v0.0.0-probe")),
            // Twice, and the honest reason is narrower than the first comment
            // here claimed. It said the second call "is the one that would trip
            // a double-prepend"; it cannot be. The Windows arm returns early on
            // `update_shown` and never reaches `prepend_items` again, and the
            // Linux arm has no flag at all. What the repeat actually shows is
            // that the *relabel* path — `set_text` on an item already in a live
            // menu — is reachable from a test process, which is a different call
            // into the OS from the prepend and worth knowing separately.
            (
                "announce_update (again)",
                tray.announce_update("v0.0.1-probe"),
            ),
        ] {
            match result {
                Ok(()) => println!("D-102:   {verb} -> ok"),
                Err(e) => println!("D-102:   {verb} -> REFUSED: {e:#}"),
            }
        }

        // Explicit rather than at end of scope. It buys one thing only, and not
        // the two an earlier comment here claimed: it puts the drop *before* the
        // `println!`-free tail so the icon leaves the notification area at a
        // known point rather than an incidental one. Nothing is "measured", and
        // scope-end would free it either way.
        drop(tray);
    }
}

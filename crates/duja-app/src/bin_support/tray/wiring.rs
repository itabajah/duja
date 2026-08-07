//! Assembly: building the tray icon + menu, and registering every foreign event
//! source (flyout, settings, tray/menu, hotkeys, engine notifications) onto the
//! published [`AppState`].
//!
//! Every handler here is foreign — it fires from a tray/menu/OS callback or a
//! side thread — so each one hops onto the Slint loop and reaches the state
//! through [`with_app`], never a direct borrow.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use duja_app::EngineNotification;
use duja_core::config::Config;
use duja_ui::{AccentChoice, HotkeyRow};

use crate::bin_support::hotkey::{self, Accelerator, HotkeyAction};

use super::hotkey_os::{
    OsHotkeyRegistrar, install_hotkey_event_handler, log_hotkey_issues, outcomes_by_action,
};
use super::state::AppState;
use super::surface::PlatformTray;
use super::{Action, icon, with_app, with_app_ref};

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
    Ok(PlatformTray::new(tray, menu, update_item))
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
#[cfg(not(target_os = "macos"))]
const fn with_left_click_policy(builder: tray_icon::TrayIconBuilder) -> tray_icon::TrayIconBuilder {
    builder
}

/// Build the global-hotkey registrar and apply the initial plan from `config`
/// on the (main) thread. A failure to create the manager or register a binding
/// only disables that hotkey (logged) — the app runs on. The registrar is
/// returned so the settings window can rebind and re-register it live.
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

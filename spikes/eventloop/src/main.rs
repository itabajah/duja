// Event-loop cohabitation spike for Duja.
//
// Question under test: can Slint (winit backend) + tray-icon + global-hotkey all
// live on ONE Windows main thread with ZERO idle wakeups (no polling timer)?
//
// Findings baked into this code (see final report for the writeup):
//   * tray-icon and global-hotkey each create a hidden Win32 window on the thread
//     that constructs them. RegisterHotKey(hwnd,..) / Shell_NotifyIcon target that
//     window, so their WM_* messages land in the MAIN THREAD message queue.
//   * Slint's winit backend runs a standard Win32 GetMessage/DispatchMessage pump
//     on the main thread, which dispatches ALL thread messages -> including the two
//     foreign windows'. So no separate pump and no polling timer are required.
//   * We bridge the crates' `set_event_handler` callbacks to Slint via
//     `slint::Weak::upgrade_in_event_loop` (thread-safe, wakes the loop only when a
//     real event fires -> zero idle wakeups).
//   * `run_event_loop_until_quit()` keeps the process alive while the window is
//     hidden (it sets quit_on_last_window_closed = false).
//
// THROWAWAY SPIKE CODE: unwraps freely.

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use slint::ComponentHandle;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIconBuilder, TrayIconEvent,
};

slint::slint! {
    import { Slider, VerticalBox } from "std-widgets.slint";

    export component FlyoutWindow inherits Window {
        title: "Duja Brightness (spike)";
        width: 320px;
        height: 420px;
        in-out property <float> brightness: 50;
        callback esc-pressed();

        forward-focus: scope;
        scope := FocusScope {
            key-pressed(event) => {
                if (event.text == Key.Escape) {
                    root.esc-pressed();
                    return accept;
                }
                return reject;
            }
            VerticalBox {
                alignment: start;
                spacing: 12px;
                Text {
                    text: "Brightness";
                    font-size: 22px;
                    horizontal-alignment: center;
                }
                Text {
                    text: round(root.brightness) + "%";
                    font-size: 16px;
                    horizontal-alignment: center;
                }
                Slider {
                    minimum: 0;
                    maximum: 100;
                    value <=> root.brightness;
                }
                Text {
                    text: "Esc or [x] hides -> stays in tray. Ctrl+Alt+F9 toggles.";
                    font-size: 11px;
                    wrap: word-wrap;
                    horizontal-alignment: center;
                }
            }
        }
    }
}

/// Compile-time renderer label, driven by the cargo feature that is active.
const RENDERER: &str = if cfg!(feature = "skia") {
    "skia"
} else if cfg!(feature = "software") {
    "software"
} else {
    "femtovg"
};

/// Build a 16x16 RGBA tray icon in code (blue square, dark border).
fn make_icon() -> Icon {
    let (w, h) = (16u32, 16u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let border = x == 0 || y == 0 || x == w - 1 || y == h - 1;
            let (r, g, b) = if border { (18, 22, 34) } else { (40, 120, 220) };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Icon::from_rgba(rgba, w, h).unwrap()
}

fn main() {
    println!("SPIKE|START pid={} renderer={}", std::process::id(), RENDERER);

    // --- UI component (creates + selects the winit backend) --------------------
    let ui = FlyoutWindow::new().unwrap();

    // Esc hides the window (process keeps running in tray).
    {
        let w = ui.as_weak();
        ui.on_esc_pressed(move || {
            if let Some(u) = w.upgrade() {
                let _ = u.hide();
                println!("SPIKE|WINDOW_HIDDEN via=esc");
            }
        });
    }
    // The [x] close button hides instead of destroying (HideWindow is also the
    // default, but we make it explicit).
    ui.window()
        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);

    // --- Tray icon + menu (created on main thread) -----------------------------
    let menu = Menu::new();
    let open_item = MenuItem::new("Open", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append_items(&[&open_item, &quit_item]).unwrap();
    let open_id = open_item.id().clone();
    let quit_id = quit_item.id().clone();

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Duja spike")
        .with_icon(make_icon())
        .build();
    let _tray = match tray {
        Ok(t) => {
            println!("SPIKE|TRAY_OK");
            t
        }
        Err(e) => {
            println!("SPIKE|TRAY_ERR {e}");
            std::process::exit(2);
        }
    };

    // --- Global hotkey Ctrl+Alt+F9 (window created on main thread) --------------
    let manager = GlobalHotKeyManager::new().unwrap();
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::F9);
    let hotkey_id = hotkey.id();
    match manager.register(hotkey) {
        Ok(()) => println!("SPIKE|HOTKEY_OK id={hotkey_id} combo=Ctrl+Alt+F9"),
        Err(e) => println!("SPIKE|HOTKEY_ERR {e}"),
    }

    // --- Wire foreign event channels into the Slint loop -----------------------
    // Each handler runs on the thread that produced the event, then hops onto the
    // Slint main thread via upgrade_in_event_loop (which also wakes the loop).

    // Tray MENU: Open -> show, Quit -> exit loop.
    {
        let w = ui.as_weak();
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            println!("SPIKE|MENU_EVENT id={}", ev.id().0);
            if ev.id() == &open_id {
                let _ = w.upgrade_in_event_loop(|u| {
                    let _ = u.show();
                    println!("SPIKE|WINDOW_SHOWN via=menu");
                });
            } else if ev.id() == &quit_id {
                let _ = slint::invoke_from_event_loop(|| {
                    println!("SPIKE|QUIT via=menu");
                    let _ = slint::quit_event_loop();
                });
            }
        }));
    }

    // Tray ICON click: left-click toggles (log only, extra proof of delivery).
    TrayIconEvent::set_event_handler(Some(|ev: TrayIconEvent| {
        println!("SPIKE|TRAY_EVENT {ev:?}");
    }));

    // Global HOTKEY: toggle on Pressed. This callback fires on the MAIN thread
    // (WM_HOTKEY is dispatched by winit's pump); the Released edge, which we
    // ignore, is what global-hotkey detects on a spawned worker thread.
    {
        let w = ui.as_weak();
        GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
            if ev.id() == hotkey_id && ev.state() == HotKeyState::Pressed {
                let _ = w.upgrade_in_event_loop(|u| {
                    if u.window().is_visible() {
                        let _ = u.hide();
                        println!("SPIKE|WINDOW_HIDDEN via=hotkey");
                    } else {
                        let _ = u.show();
                        println!("SPIKE|WINDOW_SHOWN via=hotkey");
                    }
                });
            }
        }));
    }

    // --- Instrumentation knobs (for automated measurement / verification) ------
    if std::env::var_os("SPIKE_SHOW").is_some() {
        ui.show().unwrap();
        println!("SPIKE|WINDOW_SHOWN via=startup");
    }
    if let Ok(ms) = std::env::var("SPIKE_QUIT_AFTER_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                let _ = slint::invoke_from_event_loop(|| {
                    println!("SPIKE|QUIT via=timer");
                    let _ = slint::quit_event_loop();
                });
            });
        }
    }

    println!("SPIKE|LOOP_ENTER");
    // Keeps running with the window hidden (quit_on_last_window_closed = false).
    slint::run_event_loop_until_quit().unwrap();
    println!("SPIKE|LOOP_EXIT clean");

    // Keep foreign resources alive until the very end.
    drop(_tray);
    drop(manager);
}

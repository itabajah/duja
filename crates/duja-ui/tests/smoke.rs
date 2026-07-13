//! Headless instantiation smoke test for the Slint flyout.
//!
//! This proves the generated component compiles and instantiates through the
//! public [`FlyoutShell`] surface. It needs the experimental
//! `i-slint-backend-testing` backend, so it lives behind the `smoke` feature
//! (enabled by `--all-features`) and is `#[ignore]`d: the disconnected session
//! and headless CI cannot present a window, and the plan says not to block on
//! it. Run it explicitly with:
//!
//! ```text
//! cargo test -p duja-ui --features smoke -- --ignored
//! ```

#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use duja_core::config::Config;
use duja_core::id::StableDisplayId;
use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};
use duja_ui::{
    FlyoutShell, FlyoutVm, SettingsCommand, SettingsShell, SettingsVm, Theme, UiCommand,
};

fn snapshot(serial: &str, level: u8) -> DisplaySnapshot {
    DisplaySnapshot {
        id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap(),
        name: format!("Monitor {serial}"),
        kind: DisplayKind::ExternalDdc,
        user_level_pct: level,
        capabilities: Capabilities::default(),
    }
}

#[test]
#[ignore = "needs a Slint backend; run with --ignored under a display server"]
fn flyout_shell_instantiates_and_renders() {
    i_slint_backend_testing::init_no_event_loop();

    let mut vm = FlyoutVm::new();
    vm.set_theme(Theme::Light);
    vm.set_displays(vec![snapshot("A", 40), snapshot("B", 70)]);
    let vm = Rc::new(RefCell::new(vm));

    let shell = FlyoutShell::new(vm.clone()).expect("shell instantiates");

    let commands = Rc::new(RefCell::new(Vec::<UiCommand>::new()));
    {
        let commands = commands.clone();
        shell.on_command(move |command| commands.borrow_mut().push(command));
    }

    // Re-render after an external VM mutation (the wave-2 update path).
    vm.borrow_mut().set_displays(vec![snapshot("A", 55)]);
    shell.update_from_vm(&vm.borrow());

    shell.hide();
}

#[test]
#[ignore = "needs a Slint backend; run with --ignored under a display server"]
fn settings_shell_instantiates_and_renders() {
    i_slint_backend_testing::init_no_event_loop();

    let mut vm = SettingsVm::new();
    let config = Config::default();
    vm.set_general(true, true, duja_ui::ThemeChoice::Auto, true, true);
    vm.set_displays(&[snapshot("A", 40), snapshot("B", 70)], &config, false);
    let vm = Rc::new(RefCell::new(vm));

    let shell = SettingsShell::new(vm.clone()).expect("settings shell instantiates");

    let commands = Rc::new(RefCell::new(Vec::<SettingsCommand>::new()));
    {
        let commands = commands.clone();
        shell.on_command(move |command| commands.borrow_mut().push(command));
    }

    // Re-render after an external mutation (e.g. an update-check result).
    vm.borrow_mut()
        .set_update_status(duja_ui::UpdateStatus::UpToDate);
    shell.update_from_vm(&vm.borrow());

    // One-shot present at the design size (origin 0,0) — the same entry the app
    // uses; there is no separate `show` anymore.
    shell.present_at(440.0, 600.0, 0, 0);
    shell.hide();
}

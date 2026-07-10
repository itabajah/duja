//! The settings shell: the Slint seam for the settings window.
//!
//! [`SettingsShell`] is to [`SettingsVm`] what [`FlyoutShell`](crate::FlyoutShell)
//! is to the flyout view-model — the thin, Slint-facing skin. It owns the
//! generated `SettingsWindow`, renders the pure view-model into it
//! ([`update_from_vm`](SettingsShell::update_from_vm)), and wires each widget
//! event to a view-model method, forwarding the resulting [`SettingsCommand`]s
//! ([`on_command`](SettingsShell::on_command)). No settings logic lives here.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::command::SettingsCommand;
use crate::generated::{SettingsHotkeyData, SettingsMonitorData, SettingsWindow};
use crate::settings_vm::{MonitorSection, SettingsVm, UpdateStatus};

/// Owns the Slint settings component and bridges it to a [`SettingsVm`].
pub struct SettingsShell {
    ui: SettingsWindow,
    vm: Rc<RefCell<SettingsVm>>,
    monitors: Rc<VecModel<SettingsMonitorData>>,
    hotkeys: Rc<VecModel<SettingsHotkeyData>>,
}

impl SettingsShell {
    /// Instantiate the settings window and bind it to `vm`.
    ///
    /// The window starts hidden; the close button and Esc hide rather than
    /// destroy so the process survives in the tray. The initial VM state is
    /// rendered immediately.
    ///
    /// # Errors
    /// Returns the Slint [`PlatformError`](slint::PlatformError) if the backend
    /// cannot create the window (e.g. no display server available).
    pub fn new(vm: Rc<RefCell<SettingsVm>>) -> Result<Self, slint::PlatformError> {
        let ui = SettingsWindow::new()?;
        let monitors = Rc::new(VecModel::<SettingsMonitorData>::default());
        let hotkeys = Rc::new(VecModel::<SettingsHotkeyData>::default());
        ui.set_monitors(ModelRc::from(monitors.clone()));
        ui.set_hotkeys(ModelRc::from(hotkeys.clone()));
        ui.window()
            .on_close_requested(|| slint::CloseRequestResponse::HideWindow);

        let shell = SettingsShell {
            ui,
            vm,
            monitors,
            hotkeys,
        };
        shell.update_from_vm(&shell.vm.borrow());
        Ok(shell)
    }

    /// Render `vm`'s state into the Slint component. Call after any external
    /// mutation of the shared view-model (e.g. an update-check result arriving).
    pub fn update_from_vm(&self, vm: &SettingsVm) {
        render_into(&self.ui, &self.monitors, &self.hotkeys, vm);
    }

    /// Wire every widget event to the shared view-model, forwarding the emitted
    /// [`SettingsCommand`]s (if any) to `handler`.
    pub fn on_command(&self, handler: impl FnMut(SettingsCommand) + 'static) {
        let handler = Rc::new(RefCell::new(handler));
        self.wire_general(&handler);
        self.wire_monitors(&handler);

        // Esc hides the window (process keeps running in the tray).
        let weak = self.ui.as_weak();
        self.ui.on_esc_pressed(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
        });
    }

    /// Wire the general-section widgets (autostart, theme, update check).
    fn wire_general<H: FnMut(SettingsCommand) + 'static>(&self, handler: &Rc<RefCell<H>>) {
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_autostart_toggled(move |on| {
                if let Some(command) = vm.borrow_mut().toggle_autostart(on) {
                    (handler.borrow_mut())(command);
                }
            });
        }
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_theme_selected(move |index| {
                if let Some(command) = vm.borrow_mut().select_theme(to_index(index)) {
                    (handler.borrow_mut())(command);
                }
            });
        }
        {
            let vm = self.vm.clone();
            let render = self.render_closure();
            let handler = handler.clone();
            self.ui.on_update_check_toggled(move |on| {
                let command = vm.borrow_mut().toggle_update_check(on);
                render(&vm.borrow());
                (handler.borrow_mut())(command);
            });
        }
        {
            let vm = self.vm.clone();
            let render = self.render_closure();
            let handler = handler.clone();
            self.ui.on_check_updates_clicked(move || {
                if let Some(command) = vm.borrow_mut().request_update_check() {
                    render(&vm.borrow());
                    (handler.borrow_mut())(command);
                }
            });
        }
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_open_releases_clicked(move || {
                (handler.borrow_mut())(vm.borrow().open_releases_page());
            });
        }
    }

    /// Wire the per-monitor widgets (floor, dim mode, input source).
    fn wire_monitors<H: FnMut(SettingsCommand) + 'static>(&self, handler: &Rc<RefCell<H>>) {
        {
            let vm = self.vm.clone();
            let render = self.render_closure();
            let handler = handler.clone();
            self.ui.on_monitor_floor_changed(move |idx, pct| {
                if let Some(command) = vm
                    .borrow_mut()
                    .set_monitor_floor(to_index(idx), clamp_pct(pct))
                {
                    render(&vm.borrow());
                    (handler.borrow_mut())(command);
                }
            });
        }
        {
            let vm = self.vm.clone();
            let render = self.render_closure();
            let handler = handler.clone();
            self.ui.on_monitor_dim_mode_selected(move |idx, option| {
                if let Some(command) = vm
                    .borrow_mut()
                    .select_monitor_dim_mode(to_index(idx), to_index(option))
                {
                    (handler.borrow_mut())(command);
                } else {
                    // A rejected gamma choice: re-render so the selector snaps
                    // back to the actual mode.
                    render(&vm.borrow());
                }
            });
        }
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_monitor_input_selected(move |idx, option| {
                if let Some(command) = vm
                    .borrow()
                    .select_monitor_input(to_index(idx), to_index(option))
                {
                    (handler.borrow_mut())(command);
                }
            });
        }
    }

    /// A reusable "render the VM into this window" closure that survives being
    /// moved into an event callback (holds weak UI + the two models).
    ///
    /// `use<>` opts the returned closure out of capturing `&self`'s lifetime
    /// (edition-2024 RPIT captures it by default) — it owns only the cloned
    /// weak handle and models, so it is freely movable into a `'static` callback.
    fn render_closure(&self) -> impl Fn(&SettingsVm) + use<> {
        let weak = self.ui.as_weak();
        let monitors = self.monitors.clone();
        let hotkeys = self.hotkeys.clone();
        move |vm: &SettingsVm| {
            if let Some(ui) = weak.upgrade() {
                render_into(&ui, &monitors, &hotkeys, vm);
            }
        }
    }

    /// Show the settings window (a soft failure is swallowed, like the flyout).
    pub fn show(&self) {
        let _ = self.ui.show();
    }

    /// Hide the settings window without destroying it.
    pub fn hide(&self) {
        let _ = self.ui.hide();
    }
}

/// Copy the view-model's state into the settings component's properties.
fn render_into(
    ui: &SettingsWindow,
    monitors: &VecModel<SettingsMonitorData>,
    hotkeys: &VecModel<SettingsHotkeyData>,
    vm: &SettingsVm,
) {
    ui.set_autostart_on(vm.autostart_on());
    ui.set_autostart_supported(vm.autostart_supported());
    ui.set_theme_index(i32::try_from(vm.theme_index()).unwrap_or(0));
    ui.set_update_check_on(vm.update_check_on());
    ui.set_update_status(SharedString::from(status_line(vm.update_status())));
    ui.set_update_available(vm.update_available());

    let monitor_data: Vec<SettingsMonitorData> =
        vm.monitors().iter().map(monitor_to_data).collect();
    monitors.set_vec(monitor_data);

    let hotkey_data: Vec<SettingsHotkeyData> = vm
        .hotkeys()
        .iter()
        .map(|row| SettingsHotkeyData {
            action: SharedString::from(row.action_label.as_str()),
            binding: SharedString::from(row.binding.as_str()),
            conflicted: row.conflicted,
        })
        .collect();
    hotkeys.set_vec(hotkey_data);
}

/// Map one [`MonitorSection`] to its Slint counterpart.
fn monitor_to_data(section: &MonitorSection) -> SettingsMonitorData {
    let inputs: Vec<SharedString> = section
        .inputs
        .iter()
        .map(|choice| SharedString::from(choice.label.as_str()))
        .collect();
    SettingsMonitorData {
        name: SharedString::from(section.name.as_str()),
        floor_pct: i32::from(section.floor_pct),
        dim_mode_index: i32::try_from(section.dim_mode_index()).unwrap_or(0),
        gamma_available: section.gamma_available,
        has_inputs: !inputs.is_empty(),
        inputs: ModelRc::from(Rc::new(VecModel::from(inputs)) as Rc<VecModel<SharedString>>),
    }
}

/// The (English) result line for an [`UpdateStatus`].
///
/// Dynamic, so it does not pass through `@tr`; a fully-localized status line is
/// a follow-up (documented). The static chrome around it *is* translated.
fn status_line(status: &UpdateStatus) -> &str {
    match status {
        UpdateStatus::Disabled => "Update check is off",
        UpdateStatus::Idle => "Not checked yet",
        UpdateStatus::Checking => "Checking…",
        UpdateStatus::UpToDate => "Up to date",
        UpdateStatus::Available { .. } => "Update available — open the releases page",
        UpdateStatus::Failed => "Couldn't check for updates",
    }
}

/// Convert a Slint `i32` widget index to a `usize`, mapping a (shouldn't-happen)
/// negative value to an out-of-range index the view-model then ignores.
fn to_index(index: i32) -> usize {
    usize::try_from(index).unwrap_or(usize::MAX)
}

/// Clamp and round a Slider's `f32` value into a `0..=100` percent (floor slider
/// is capped further to the view-model's max).
fn clamp_pct(value: f32) -> u8 {
    let clamped = value.clamp(0.0, 100.0).round();
    // RATIONALE: `clamped` is in 0.0..=100.0 and integral after round(), so the
    // cast neither truncates a meaningful fraction, loses a sign, nor overflows
    // u8 — clippy's cast lints cannot see the numeric bounds.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pct = clamped as u8;
    pct
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_pct_bounds_and_rounds() {
        assert_eq!(clamp_pct(-5.0), 0);
        assert_eq!(clamp_pct(24.6), 25);
        assert_eq!(clamp_pct(250.0), 100);
        assert_eq!(clamp_pct(f32::NAN), 0);
    }

    #[test]
    fn status_line_covers_every_variant() {
        assert!(!status_line(&UpdateStatus::Disabled).is_empty());
        assert!(!status_line(&UpdateStatus::Idle).is_empty());
        assert!(!status_line(&UpdateStatus::Checking).is_empty());
        assert!(!status_line(&UpdateStatus::UpToDate).is_empty());
        assert!(
            !status_line(&UpdateStatus::Available {
                version: "v1".to_owned()
            })
            .is_empty()
        );
        assert!(!status_line(&UpdateStatus::Failed).is_empty());
    }

    // Instantiating the Slint window needs a real backend/event loop, which is
    // unavailable in this disconnected session and in headless CI. The smoke
    // test that exercises it lives behind `#[ignore]` in tests/smoke.rs.
}

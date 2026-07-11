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
        self.wire_hotkeys(&handler);

        // Esc and the close button both hide the window (stays in the tray).
        {
            let weak = self.ui.as_weak();
            self.ui.on_esc_pressed(move || {
                if let Some(ui) = weak.upgrade() {
                    let _ = ui.hide();
                }
            });
        }
        {
            let weak = self.ui.as_weak();
            self.ui.on_close_requested(move || {
                if let Some(ui) = weak.upgrade() {
                    let _ = ui.hide();
                }
            });
        }
    }

    /// Wire the editable hotkey rows (record a chord / clear a binding).
    fn wire_hotkeys<H: FnMut(SettingsCommand) + 'static>(&self, handler: &Rc<RefCell<H>>) {
        {
            let vm = self.vm.clone();
            let render = self.render_closure();
            let weak = self.ui.as_weak();
            let handler = handler.clone();
            self.ui
                .on_hotkey_key_captured(move |idx, ctrl, alt, shift, meta, token| {
                    let row = to_index(idx);
                    let key = if token.is_empty() {
                        None
                    } else {
                        Some(token.as_str())
                    };
                    let mods = crate::settings_vm::CaptureModifiers {
                        ctrl,
                        alt,
                        shift,
                        meta,
                    };
                    let command = vm.borrow().capture_hotkey(row, mods, key);
                    // A complete chord ends recording and dispatches; a
                    // modifiers-only (pending) chord keeps the recorder armed.
                    if let Some(command) = command {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_recording_index(-1);
                        }
                        render(&vm.borrow());
                        (handler.borrow_mut())(command);
                    }
                });
        }
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_hotkey_clear_clicked(move |idx| {
                let command = vm.borrow().clear_hotkey(to_index(idx));
                if let Some(command) = command {
                    (handler.borrow_mut())(command);
                }
            });
        }
    }

    /// Wire the general-section widgets (autostart, theme, update check).
    fn wire_general<H: FnMut(SettingsCommand) + 'static>(&self, handler: &Rc<RefCell<H>>) {
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_autostart_toggled(move |on| {
                // Bind first so the VM's `borrow_mut` is released before the
                // handler runs — the app re-renders from the same VM inside the
                // handler and a still-held borrow would double-borrow it (P0
                // bugs 1 & 2).
                let command = vm.borrow_mut().toggle_autostart(on);
                if let Some(command) = command {
                    (handler.borrow_mut())(command);
                }
            });
        }
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_theme_selected(move |index| {
                let command = vm.borrow_mut().select_theme(to_index(index));
                if let Some(command) = command {
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
                apply_command(&vm, SettingsVm::request_update_check, &render, &handler);
            });
        }
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_open_releases_clicked(move || {
                let command = vm.borrow().open_releases_page();
                (handler.borrow_mut())(command);
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
                apply_command(
                    &vm,
                    |v| v.set_monitor_floor(to_index(idx), clamp_pct(pct)),
                    &render,
                    &handler,
                );
            });
        }
        {
            let vm = self.vm.clone();
            let render = self.render_closure();
            let handler = handler.clone();
            self.ui.on_monitor_dim_mode_selected(move |idx, option| {
                let command = vm
                    .borrow_mut()
                    .select_monitor_dim_mode(to_index(idx), to_index(option));
                if let Some(command) = command {
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
                let command = vm
                    .borrow()
                    .select_monitor_input(to_index(idx), to_index(option));
                if let Some(command) = command {
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

    /// Move the settings window to physical pixel `(x, y)`.
    ///
    /// Used to centre the window on the active monitor instead of letting the OS
    /// drop it at a default cascade spot (P0 live-QA bug 4). Physical pixels pass
    /// through `set_position` unscaled.
    pub fn set_position(&self, x: i32, y: i32) {
        self.ui
            .window()
            .set_position(slint::PhysicalPosition::new(x, y));
    }

    /// The settings window's current size in **physical** pixels.
    #[must_use]
    pub fn physical_size(&self) -> (u32, u32) {
        let size = self.ui.window().size();
        (size.width, size.height)
    }

    /// The window's device scale factor (physical / logical pixels).
    #[must_use]
    pub fn scale_factor(&self) -> f32 {
        self.ui.window().scale_factor()
    }

    /// Set the settings window's DPI-compensated usable content width and its
    /// content height (both logical px), mirroring the flyout — the framed
    /// window's buffer is also undersized on fractional-scale monitors.
    pub fn set_geometry(&self, usable_width: f32, content_height: f32) {
        self.ui.set_usable_width(usable_width);
        self.ui.set_content_height(content_height);
    }

    /// Bring the settings window to the foreground (best-effort focus).
    ///
    /// A normal window — *not* topmost — so it opens above the caller but does not
    /// float over everything. No-op off the winit backend or if the OS refuses
    /// the foreground change.
    pub fn focus(&self) {
        use i_slint_backend_winit::WinitWindowAccessor;
        self.ui.window().with_winit_window(|w| {
            w.focus_window();
        });
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
    // Diff in place (never `set_vec`) so a per-monitor slider/combo the user is
    // interacting with is not destroyed by an unrelated re-render (P0 bug 3).
    crate::model_sync::sync(monitors, monitor_data);

    let hotkey_data: Vec<SettingsHotkeyData> = vm
        .hotkeys()
        .iter()
        .map(|row| SettingsHotkeyData {
            action: SharedString::from(row.action_label.as_str()),
            binding: SharedString::from(row.binding.as_str()),
            conflicted: row.conflicted,
            unavailable: row.unavailable,
        })
        .collect();
    crate::model_sync::sync(hotkeys, hotkey_data);
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

/// Run a view-model mutation, then (only if it produced a command) re-render and
/// dispatch it — with the VM's `borrow_mut` **released before** the re-render.
///
/// This is the structural cure for P0 live-QA bugs 1 & 2: the widget callbacks
/// used to hold `vm.borrow_mut()` (an `if let` scrutinee temporary lives through
/// the whole arm in edition 2024) across `render(&vm.borrow())` and the app's
/// `update_from_vm(&vm.borrow())`, double-borrowing the same `RefCell` and
/// panicking straight into Slint's FFI (→ abort). Binding the mutation's result
/// to a local drops the mutable borrow first, so the subsequent shared borrows
/// are safe.
fn apply_command<H, R>(
    vm: &RefCell<SettingsVm>,
    mutate: impl FnOnce(&mut SettingsVm) -> Option<SettingsCommand>,
    render: &R,
    handler: &RefCell<H>,
) where
    H: FnMut(SettingsCommand),
    R: Fn(&SettingsVm),
{
    let command = mutate(&mut vm.borrow_mut());
    if let Some(command) = command {
        render(&vm.borrow());
        (handler.borrow_mut())(command);
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
    use duja_core::config::Config;
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};

    fn snapshot(serial: &str) -> DisplaySnapshot {
        DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap(),
            name: format!("Monitor {serial}"),
            kind: DisplayKind::ExternalDdc,
            user_level_pct: 50,
            capabilities: Capabilities::default(),
        }
    }

    fn vm_with_one_monitor() -> SettingsVm {
        let mut vm = SettingsVm::new();
        vm.set_displays(&[snapshot("A")], &Config::default(), true);
        vm
    }

    // --- P0 bugs 1 & 2: the settings callbacks must not double-borrow the VM ---
    //
    // The regression is that a widget callback held `vm.borrow_mut()` across a
    // re-render that *reads* the same VM (`render(&vm.borrow())` here, and the
    // app's `update_from_vm(&vm.borrow())` in production). `apply_command` is
    // exercised with exactly that shape: a `render` that borrows the VM. Before
    // the fix (borrow held across the arm) this panics with a `BorrowError`;
    // after it, the mutable borrow is released first and it runs cleanly.

    #[test]
    fn apply_command_releases_borrow_before_render_and_dispatch() {
        let vm = RefCell::new(vm_with_one_monitor());
        let rendered = std::cell::Cell::new(false);
        let render = |v: &SettingsVm| {
            // Reads the VM exactly as `update_from_vm` does.
            let _ = v.monitors();
            rendered.set(true);
        };
        let dispatched = RefCell::new(Vec::new());
        let handler = RefCell::new(|c: SettingsCommand| dispatched.borrow_mut().push(c));

        // A floor change produces a command → render + dispatch must both run.
        apply_command(&vm, |v| v.set_monitor_floor(0, 10), &render, &handler);

        assert!(rendered.get(), "re-render must run without a double borrow");
        assert_eq!(dispatched.borrow().len(), 1);
        assert!(matches!(
            dispatched.borrow().first(),
            Some(SettingsCommand::SetMonitorFloor { pct: 10, .. })
        ));
    }

    #[test]
    fn apply_command_noop_when_mutation_yields_nothing() {
        let vm = RefCell::new(SettingsVm::new()); // no monitors → out-of-range
        let render = |_v: &SettingsVm| panic!("must not render when no command");
        let handler = RefCell::new(|_c: SettingsCommand| panic!("must not dispatch"));
        apply_command(&vm, |v| v.set_monitor_floor(0, 10), &render, &handler);
    }

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

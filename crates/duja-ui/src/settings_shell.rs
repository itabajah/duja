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
    /// The design logical size the buffer keeper enforces (see [`crate::dpi`]).
    desired: crate::dpi::DesiredSize,
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

        // Install the fractional-DPI buffer keeper (no focus-loss dismissal for
        // the settings window). `desired` seeds to the initial size; `present_at`
        // keeps it current, and — because the settings window is user-resizable —
        // `track_resize: true` updates it as the user drags the window's edges.
        let desired: crate::dpi::DesiredSize = Rc::new(std::cell::Cell::new((560.0, 700.0)));
        let focus_lost: crate::dpi::FocusLostCb = Rc::new(RefCell::new(None));
        crate::dpi::install_window_hook(ui.window(), desired.clone(), focus_lost, true);

        let shell = SettingsShell {
            ui,
            vm,
            monitors,
            hotkeys,
            desired,
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

        // Frameless header drag: start a winit system move so the OS drives the
        // window under the pointer (correct at any DPI — no manual set-position).
        {
            let weak = self.ui.as_weak();
            self.ui.on_start_drag(move || {
                if let Some(ui) = weak.upgrade() {
                    use i_slint_backend_winit::WinitWindowAccessor;
                    ui.window().with_winit_window(|w| {
                        let _ = w.drag_window();
                    });
                }
            });
        }

        // Frameless resize grips: start a winit system resize in the direction the
        // grip encodes (the `.slint` edge/corner TouchAreas pass 0..=7). The OS
        // then drives the resize until release — no per-frame set-size.
        {
            let weak = self.ui.as_weak();
            self.ui.on_start_resize(move |dir| {
                if let Some(ui) = weak.upgrade() {
                    use i_slint_backend_winit::WinitWindowAccessor;
                    use i_slint_backend_winit::winit::window::ResizeDirection;
                    let direction = match dir {
                        0 => ResizeDirection::North,
                        1 => ResizeDirection::South,
                        2 => ResizeDirection::East,
                        3 => ResizeDirection::West,
                        4 => ResizeDirection::NorthEast,
                        5 => ResizeDirection::NorthWest,
                        6 => ResizeDirection::SouthEast,
                        7 => ResizeDirection::SouthWest,
                        // The `.slint` grips only ever emit 0..=7; ignore anything
                        // else rather than starting a stray corner resize.
                        _ => return,
                    };
                    ui.window().with_winit_window(|w| {
                        let _ = w.drag_resize_window(direction);
                    });
                }
            });
        }

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
            self.ui.on_monitor_min_perceived_changed(move |idx, pct| {
                apply_command(
                    &vm,
                    |v| v.set_monitor_min_perceived(to_index(idx), clamp_pct(pct)),
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

    /// Move the settings window to physical `(x, y)` while hidden, then present it
    /// once — the same one-shot present as the flyout (item 1). Slint sizes the
    /// buffer natively for the monitor; nothing resizes/moves it after `show()`, so
    /// the software renderer never presents a partial first frame. A soft failure
    /// is swallowed, like the flyout.
    pub fn present_at(&self, logical_width: f32, logical_height: f32, x: i32, y: i32) {
        self.desired.set((logical_width, logical_height));
        self.set_position(x, y);
        let _ = self.ui.show();
        // A no-frame *resizable* window opens at its content's preferred size and
        // ignores the `.slint` preferred-width/height, so force the initial inner
        // size to the intended design size. Safe on the show path here (unlike the
        // flyout) because the `present-nonce` flip below repaints the whole window,
        // so this show-time resize cannot leave a partial first frame.
        crate::dpi::enforce_physical_buffer(self.ui.window(), logical_width, logical_height);
        // The settings window is user-resizable (custom frameless grips drive
        // `drag_resize_window`); assert it now that the winit window exists. The
        // `.slint` min-width/height bound how far it can shrink. No-op off winit.
        {
            use i_slint_backend_winit::WinitWindowAccessor;
            use i_slint_backend_winit::winit::dpi::LogicalSize;
            self.ui.window().with_winit_window(|w| {
                w.set_resizable(true);
                // Enforce the same shrink floor as the `.slint` min-width/height at
                // the OS level, so an OS-driven grip resize can't drag the window
                // below the size its controls need (belt-and-suspenders to Slint's
                // own min-constraint propagation).
                w.set_min_inner_size(Some(LogicalSize::new(380.0_f64, 360.0_f64)));
                // Give the taskbar button a real icon (see `crate::window_icon`).
                w.set_window_icon(crate::window_icon::app_icon());
            });
        }
        // Flip the repaint anchor so the whole window is marked dirty and the
        // software renderer presents a complete frame (see the flyout's
        // `present_at` for the full root cause).
        self.ui.set_present_nonce(!self.ui.get_present_nonce());
    }

    /// Move the settings window to physical pixel `(x, y)` (physical pixels pass
    /// through `set_position` unscaled).
    pub fn set_position(&self, x: i32, y: i32) {
        self.ui
            .window()
            .set_position(slint::PhysicalPosition::new(x, y));
    }

    /// Set the settings window's desired content height (logical px). Like the
    /// flyout, the app drives the height so the window grows to its content, and
    /// keeps the DPI hook's target current for cross-monitor scale changes.
    pub fn set_content_height(&self, content_height: f32) {
        self.ui.set_content_height(content_height);
        let (w, _) = self.desired.get();
        self.desired.set((w, content_height));
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
            // Force a complete first frame after showing (see the flyout's
            // `surface`): avoids an occasional partially-painted open.
            w.request_redraw();
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
    // The resolved palette (`Palette.dark <=> dark` in settings.slint). Without
    // this the settings window stayed pinned to the default dark palette and
    // ignored the user's Light/Dark choice, even as the selector moved.
    ui.set_dark(vm.dark());
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
        min_perceived_pct: i32::from(section.min_perceived_pct),
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

/// Binding-layer regression tests driving the real settings `.slint` widgets
/// through the headless `i-slint-backend-testing` backend — catching wiring bugs
/// the pure view-model tests cannot see (they live in the `.slint` ↔ shell seam).
///
/// Gated behind the `smoke` feature (which pulls the testing backend) so the
/// default test build stays backend-free; run under `--all-features`.
#[cfg(all(test, feature = "smoke"))]
mod binding_tests {
    use super::*;
    use crate::command::ThemeChoice;
    use duja_core::config::Config;
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};
    use i_slint_backend_testing::ElementHandle;

    fn snapshot(serial: &str) -> DisplaySnapshot {
        DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap(),
            name: format!("Monitor {serial}"),
            kind: DisplayKind::ExternalDdc,
            user_level_pct: 50,
            capabilities: Capabilities::default(),
        }
    }

    // The perceptual-anchor calibration slider must render in each per-monitor
    // section — proving the SettingsMonitorData `min-perceived-pct` field, the
    // FieldRow, and the `value: monitor.min-perceived-pct` binding all compiled and
    // bound (a pure `SettingsVm` test cannot see the `.slint` seam). Proven red
    // before the field + FieldRow existed. Its emit/clamp logic is covered by the
    // pure `set_monitor_min_perceived_clamps_and_emits` test.
    #[test]
    fn settings_min_perceived_slider_is_rendered_per_monitor() {
        i_slint_backend_testing::init_no_event_loop();

        let mut vm = SettingsVm::new();
        vm.set_displays(&[snapshot("A"), snapshot("B")], &Config::default(), true);
        let vm = Rc::new(RefCell::new(vm));
        let shell = SettingsShell::new(vm).expect("settings shell instantiates");

        // Each per-monitor section contributes two elements carrying this label:
        // the FieldRow's caption Text and the Slider itself. Two monitors ⇒ four —
        // proving the calibration control renders once per display.
        let matches =
            ElementHandle::find_by_accessible_label(&shell.ui, "Brightness at hardware minimum")
                .count();
        assert_eq!(
            matches, 4,
            "each per-monitor section must render its calibration slider"
        );
    }

    // The settings window must follow the resolved theme. Before the fix,
    // `render_into` never called `set_dark`, so the window stayed pinned to
    // `Palette.dark`'s default (`true`) regardless of the user's Light/Dark choice
    // — the selector moved but the palette did not (STATUS.md's settings QA
    // promises "palette matches the flyout"). This drives the real `dark` property
    // through the `.slint` binding, which a pure `SettingsVm` test cannot. Proven
    // red against the pre-fix shell: `get_dark()` stayed `true` under a light
    // resolution.
    #[test]
    fn settings_palette_follows_the_resolved_theme() {
        i_slint_backend_testing::init_no_event_loop();

        let mut vm = SettingsVm::new();
        // A light resolution: raw preference Light, resolved palette dark = false.
        vm.set_general(true, true, ThemeChoice::Light, false, false);
        let vm = Rc::new(RefCell::new(vm));
        let shell = SettingsShell::new(vm.clone()).expect("settings shell instantiates");

        assert!(
            !shell.ui.get_dark(),
            "settings palette must follow the resolved light theme (set_dark missing in render_into)"
        );

        // Flip to a dark resolution and re-render: the palette must track it.
        vm.borrow_mut()
            .set_general(true, true, ThemeChoice::Dark, false, true);
        shell.update_from_vm(&vm.borrow());
        assert!(
            shell.ui.get_dark(),
            "settings palette must follow the resolved dark theme"
        );
    }

    // First-paint fix, settings twin: like the flyout, every present must force a
    // complete software-renderer frame via the full-window `present-nonce` anchor
    // (see the flyout's `present_flips_the_repaint_nonce_on_every_show` for the
    // root cause). Proven red against a `present_at` that does not flip the nonce.
    #[test]
    fn present_flips_the_repaint_nonce_on_every_show() {
        i_slint_backend_testing::init_no_event_loop();

        let vm = Rc::new(RefCell::new(SettingsVm::new()));
        let shell = SettingsShell::new(vm).expect("settings shell instantiates");

        let initial = shell.ui.get_present_nonce();
        shell.present_at(560.0, 700.0, 0, 0);
        assert_ne!(
            shell.ui.get_present_nonce(),
            initial,
            "present_at must flip the repaint nonce so the whole window is dirtied"
        );

        shell.present_at(560.0, 700.0, 0, 0);
        assert_eq!(
            shell.ui.get_present_nonce(),
            initial,
            "a second present must flip the nonce back (each show repaints fully)"
        );
    }
}

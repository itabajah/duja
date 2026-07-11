//! The flyout shell: the *only* module that touches Slint types.
//!
//! [`FlyoutShell`] owns the generated Slint component and is the thin seam
//! between the pure [`FlyoutVm`] and the rendered window. It does two things
//! and nothing else:
//!
//! - **VM state → Slint model**: [`update_from_vm`](FlyoutShell::update_from_vm)
//!   copies the view-model's rows/flags/theme into the component's properties.
//! - **Slint callbacks → VM calls**: [`on_command`](FlyoutShell::on_command)
//!   wires each widget event to a view-model method and forwards the resulting
//!   [`UiCommand`]s to the caller's handler.
//!
//! The shell holds the view-model in an `Rc<RefCell<…>>` so the event
//! callbacks (which outlive any single call) can mutate it and re-render. Wave
//! 2 drives external updates by mutating the *same* shared view-model (e.g.
//! `vm.borrow_mut().set_displays(..)` from an engine notification) and then
//! calling [`update_from_vm`](FlyoutShell::update_from_vm).
//!
//! Positioning and the tray icon are **not** here — that is app assembly (wave
//! 2). The shell exposes only [`show_at`](FlyoutShell::show_at) and
//! [`hide`](FlyoutShell::hide) for wave 2 to place the window.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::command::UiCommand;
use crate::flyout_vm::{FlyoutRow, FlyoutVm, Theme};

use crate::generated::{FlyoutRowData, FlyoutWindow};

/// Owns the Slint flyout component and bridges it to a [`FlyoutVm`].
pub struct FlyoutShell {
    ui: FlyoutWindow,
    vm: Rc<RefCell<FlyoutVm>>,
    rows: Rc<VecModel<FlyoutRowData>>,
}

impl FlyoutShell {
    /// Instantiate the flyout window and bind it to `vm`.
    ///
    /// The window starts hidden (Slint shows only on an explicit
    /// [`show_at`](Self::show_at)); the close button hides rather than destroys
    /// so the process survives in the tray. The initial VM state is rendered
    /// immediately.
    ///
    /// # Errors
    /// Returns the Slint [`PlatformError`](slint::PlatformError) if the backend
    /// fails to create the window (e.g. no display server available).
    pub fn new(vm: Rc<RefCell<FlyoutVm>>) -> Result<Self, slint::PlatformError> {
        let ui = FlyoutWindow::new()?;
        let rows = Rc::new(VecModel::<FlyoutRowData>::default());
        ui.set_rows(ModelRc::from(rows.clone()));
        ui.window()
            .on_close_requested(|| slint::CloseRequestResponse::HideWindow);

        let shell = FlyoutShell { ui, vm, rows };
        shell.update_from_vm(&shell.vm.borrow());
        Ok(shell)
    }

    /// Render `vm`'s state into the Slint component.
    ///
    /// Pure copy-out: rows, the link-all flag, the empty-state flag, and the
    /// theme. Call after any external mutation of the shared view-model.
    pub fn update_from_vm(&self, vm: &FlyoutVm) {
        render_into(&self.ui, &self.rows, vm);
    }

    /// Register the command handler and wire every widget event to the shared
    /// view-model.
    ///
    /// Slider drags, the link toggle and the refresh button call the matching
    /// [`FlyoutVm`] method; the emitted [`UiCommand`]s (if any) are passed to
    /// `handler`. Esc hides the window; the settings gear emits
    /// [`UiCommand::OpenSettings`] for the app to open the settings window.
    pub fn on_command(&self, handler: impl FnMut(UiCommand) + 'static) {
        let handler = Rc::new(RefCell::new(handler));

        // Slider drag: apply to the VM, re-render, forward the fan-out.
        {
            let vm = self.vm.clone();
            let rows = self.rows.clone();
            let weak = self.ui.as_weak();
            let handler = handler.clone();
            self.ui.on_slider_changed(move |idx, pct| {
                let index = usize::try_from(idx).unwrap_or(usize::MAX);
                let commands = vm.borrow_mut().slider_changed(index, clamp_pct(pct));
                if let Some(ui) = weak.upgrade() {
                    render_into(&ui, &rows, &vm.borrow());
                }
                let mut handler = handler.borrow_mut();
                for command in commands {
                    handler(command);
                }
            });
        }

        // Link toggle: pure VM state, no command.
        {
            let vm = self.vm.clone();
            self.ui.on_link_toggled(move |on| {
                vm.borrow_mut().link_toggled(on);
            });
        }

        // Refresh affordance.
        {
            let vm = self.vm.clone();
            let handler = handler.clone();
            self.ui.on_refresh_requested(move || {
                let command = vm.borrow().refresh_requested();
                (handler.borrow_mut())(command);
            });
        }

        // Esc hides the flyout (process keeps running in the tray).
        {
            let weak = self.ui.as_weak();
            self.ui.on_esc_pressed(move || {
                if let Some(ui) = weak.upgrade() {
                    let _ = ui.hide();
                }
            });
        }

        // Settings gear: emit OpenSettings for the app to open the window.
        {
            let handler = handler.clone();
            self.ui.on_settings_clicked(move || {
                (handler.borrow_mut())(UiCommand::OpenSettings);
            });
        }
    }

    /// Position the flyout at physical pixel `(x, y)` and show it.
    ///
    /// Wave 2 computes the anchor from the tray icon rect; the shell only
    /// places and shows. A failed show is swallowed — a flyout that cannot be
    /// presented is a soft failure, not a crash.
    pub fn show_at(&self, x: i32, y: i32) {
        self.set_position(x, y);
        let _ = self.ui.show();
    }

    /// Move the flyout to physical pixel `(x, y)` without changing visibility.
    ///
    /// The anchor is computed against the flyout's *nominal* size; the window's
    /// real height is content-driven and only known once shown, so wave 2 shows
    /// first, reads [`physical_size`](Self::physical_size), then calls this to
    /// land the window against the tray edge (P0 live-QA bug 4).
    pub fn set_position(&self, x: i32, y: i32) {
        // Physical coordinates: `set_position` takes physical screen pixels and
        // passes the `Physical` variant straight through (no scale applied), so
        // Win32 physical anchors land unscaled on a Per-Monitor-V2 process.
        self.ui
            .window()
            .set_position(slint::PhysicalPosition::new(x, y));
    }

    /// The flyout window's current size in **physical** pixels.
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

    /// Hide the flyout without destroying it (it stays alive in the tray).
    pub fn hide(&self) {
        let _ = self.ui.hide();
    }

    /// Invoke `handler` whenever the flyout window loses focus (the user clicked
    /// outside it / activated another window).
    ///
    /// Standard tray-flyout dismissal: `slint` exposes no window-deactivate
    /// callback, so this taps the raw winit `WindowEvent::Focused(false)` via the
    /// backend accessor. The event still fires for a borderless (`no-frame`)
    /// top-level window. The handler routes back through the app so the flyout's
    /// visibility state stays consistent (P0 live-QA bug 5).
    pub fn on_focus_lost(&self, mut handler: impl FnMut() + 'static) {
        use i_slint_backend_winit::winit::event::WindowEvent;
        use i_slint_backend_winit::{EventResult, WinitWindowAccessor};
        self.ui
            .window()
            .on_winit_window_event(move |_window, event| {
                if matches!(event, WindowEvent::Focused(false)) {
                    handler();
                }
                // Let Slint keep processing the event normally.
                EventResult::Propagate
            });
    }
}

/// Copy the view-model's state into the Slint component's properties.
fn render_into(ui: &FlyoutWindow, rows: &VecModel<FlyoutRowData>, vm: &FlyoutVm) {
    let data: Vec<FlyoutRowData> = vm.rows().iter().map(row_to_data).collect();
    // Diff the rows model in place (never `set_vec`, which resets the repeater
    // and destroys the element a user is mid-drag on — P0 live-QA bug 3).
    crate::model_sync::sync(rows, data);
    ui.set_link_all(vm.link_all());
    ui.set_no_displays(vm.no_displays());
    ui.set_dark(matches!(vm.theme(), Theme::Dark));
}

/// Map one pure [`FlyoutRow`] to its Slint counterpart.
fn row_to_data(row: &FlyoutRow) -> FlyoutRowData {
    FlyoutRowData {
        name: SharedString::from(row.display_name.as_str()),
        percent: i32::from(row.level_pct),
        kind: SharedString::from(row.kind_label.as_str()),
        greyed: row.greyed,
        slider_enabled: row.slider_enabled,
    }
}

/// Clamp and round a Slider's `f32` value into a `0..=100` percent.
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
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};

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
    fn clamp_pct_bounds_and_rounds() {
        assert_eq!(clamp_pct(-5.0), 0);
        assert_eq!(clamp_pct(0.4), 0);
        assert_eq!(clamp_pct(49.6), 50);
        assert_eq!(clamp_pct(100.0), 100);
        assert_eq!(clamp_pct(250.0), 100);
        assert_eq!(clamp_pct(f32::NAN), 0);
    }

    #[test]
    fn row_to_data_copies_every_field() {
        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snapshot("A", 40)]);
        vm.set_unresponsive(
            &StableDisplayId::from_parts("GSM", 0x0001, Some("A")).unwrap(),
            true,
        );
        let row = vm.rows().first().unwrap();
        let data = row_to_data(row);
        assert_eq!(data.name.as_str(), "Monitor A");
        assert_eq!(data.percent, 40);
        assert_eq!(data.kind.as_str(), "External");
        assert!(data.greyed);
        assert!(!data.slider_enabled);
    }

    // Instantiating the Slint window needs a real backend/event loop, which is
    // unavailable in this disconnected session and in headless CI. The smoke
    // test that exercises it lives behind `#[ignore]` in tests/smoke.rs.
}

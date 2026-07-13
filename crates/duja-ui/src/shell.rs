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
//! 2). The shell exposes only [`present_at`](FlyoutShell::present_at) and
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
    /// The design logical size the buffer keeper enforces (see [`crate::dpi`]).
    desired: crate::dpi::DesiredSize,
    /// The click-outside dismissal callback, invoked by the shared winit hook.
    focus_lost: crate::dpi::FocusLostCb,
}

impl FlyoutShell {
    /// Instantiate the flyout window and bind it to `vm`.
    ///
    /// The window starts hidden (Slint shows only on an explicit
    /// [`present_at`](Self::present_at)); the close button hides rather than
    /// destroys so the process survives in the tray. The initial VM state is
    /// rendered immediately.
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

        // Install the single winit event hook: fractional-DPI buffer keeper +
        // focus-loss forwarder. `desired` seeds to the design size; the app
        // updates it via `enforce_logical_size`.
        let desired: crate::dpi::DesiredSize = Rc::new(std::cell::Cell::new((320.0, 260.0)));
        let focus_lost: crate::dpi::FocusLostCb = Rc::new(RefCell::new(None));
        crate::dpi::install_window_hook(ui.window(), desired.clone(), focus_lost.clone());

        let shell = FlyoutShell {
            ui,
            vm,
            rows,
            desired,
            focus_lost,
        };
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

        // Dimming toggle: apply to the VM, re-render, forward the command.
        {
            let vm = self.vm.clone();
            let rows = self.rows.clone();
            let weak = self.ui.as_weak();
            let handler = handler.clone();
            self.ui.on_dimming_toggled(move |idx, on| {
                let index = usize::try_from(idx).unwrap_or(usize::MAX);
                let command = vm.borrow_mut().toggle_dimming(index, on);
                if let Some(ui) = weak.upgrade() {
                    render_into(&ui, &rows, &vm.borrow());
                }
                if let Some(command) = command {
                    (handler.borrow_mut())(command);
                }
            });
        }

        // Link toggle: pure VM state, no command — but re-render so the pill's
        // `checked` (bound one-way to `link-all`) reflects the new state. Without
        // the re-render the toggle stayed frozen and re-sent the same value on
        // every click (P0 live-QA: "Link all does nothing").
        {
            let vm = self.vm.clone();
            let rows = self.rows.clone();
            let weak = self.ui.as_weak();
            self.ui.on_link_toggled(move |on| {
                vm.borrow_mut().link_toggled(on);
                if let Some(ui) = weak.upgrade() {
                    render_into(&ui, &rows, &vm.borrow());
                }
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

        // Esc and the close button both hide the flyout (process stays in tray).
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

        // Settings gear: emit OpenSettings for the app to open the window.
        {
            let handler = handler.clone();
            self.ui.on_settings_clicked(move || {
                (handler.borrow_mut())(UiCommand::OpenSettings);
            });
        }
    }

    /// Set the flyout's desired window height (logical px). A no-frame top-level
    /// window is not auto-grown to its content preferred height after the rows
    /// populate asynchronously, so the app drives it from the row count.
    ///
    /// Also keeps the DPI hook's target height current (the width is fixed) so a
    /// genuine cross-monitor scale change re-asserts the correct physical buffer.
    pub fn set_content_height(&self, logical_height: f32) {
        self.ui.set_content_height(logical_height);
        let (w, _) = self.desired.get();
        self.desired.set((w, logical_height));
    }

    /// Force the window's physical buffer to `logical × scale`, sizing it from
    /// the winit-reported (OS-true) scale factor rather than Slint's.
    ///
    /// Called when the flyout's content is resized **while it is already visible**
    /// — a hot-plug changing the row count (see the tray's re-enumeration path).
    /// It is deliberately *not* on the show path: a show-time resize aged the
    /// software renderer into a partial first frame (see
    /// [`present_at`](Self::present_at)), so presentation now happens in one shot
    /// with no resize after `show()`.
    ///
    /// The failure it guards: a borderless (`no-frame`) window whose physical
    /// buffer was allocated at one scale but is then laid out at another keeps the
    /// stale buffer and clips the right/bottom edge.
    /// `slint::Window::set_size(LogicalSize)` cannot cure it (it multiplies by
    /// Slint's *own*, possibly stale, scale); driving winit's `request_inner_size`
    /// with an explicit **physical** size grows the OS buffer directly. Idempotent;
    /// on an integer scale it requests the design size unchanged.
    pub fn enforce_logical_size(&self, logical_width: f32, logical_height: f32) {
        // Record the target so the scale-change hook can re-assert it, then make
        // an immediate best-effort pass now.
        self.desired.set((logical_width, logical_height));
        crate::dpi::enforce_physical_buffer(self.ui.window(), logical_width, logical_height);
    }

    /// Move the flyout to physical `(x, y)` while hidden, then present it once.
    ///
    /// The window is placed *before* `show()` and nothing resizes or moves it
    /// after — Slint sizes the buffer natively for the monitor it is shown on
    /// (PR #29), and the DPI hook re-asserts on a genuine cross-monitor scale
    /// change. An explicit show-time buffer resize (the former enforce) triggered
    /// an early hidden render that aged the software renderer's buffer, so the
    /// post-show render went partial-and-empty and never presented — leaving a
    /// transparent first frame that only repaired on a later click (item 1).
    /// Placing before a single `show()`, with no resize after, removes that race.
    /// A failed show is swallowed — an unpresentable flyout is a soft failure.
    pub fn present_at(&self, logical_width: f32, logical_height: f32, x: i32, y: i32) {
        self.desired.set((logical_width, logical_height));
        self.set_position(x, y);
        let _ = self.ui.show();
    }

    /// Move the flyout to physical pixel `(x, y)` without changing visibility.
    ///
    /// Physical coordinates: `set_position` takes physical screen pixels and
    /// passes the `Physical` variant straight through (no scale applied), so
    /// Win32 physical anchors land unscaled on a Per-Monitor-V2 process.
    pub fn set_position(&self, x: i32, y: i32) {
        self.ui
            .window()
            .set_position(slint::PhysicalPosition::new(x, y));
    }

    /// Raise the flyout above other windows and (best-effort) give it focus.
    ///
    /// A tray flyout must never be buried, so while visible it is kept
    /// always-on-top (`topmost`); it is also focused so Esc/keyboard work at once
    /// and focus-loss dismissal has a coherent baseline. Focus may be refused by
    /// the OS foreground lock when not shown from a user gesture (e.g. an IPC
    /// `ShowFlyout`); the topmost level still applies. No-op off the winit backend.
    pub fn surface(&self, topmost: bool) {
        use i_slint_backend_winit::WinitWindowAccessor;
        use i_slint_backend_winit::winit::window::WindowLevel;
        self.ui.window().with_winit_window(|w| {
            w.set_window_level(if topmost {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            });
            w.focus_window();
            // Force a full repaint after (re)showing: a fresh show occasionally
            // presented a partially-painted first frame until the next input
            // (P0 live-QA bug 4/partial-render). An explicit redraw request makes
            // the first presented frame complete.
            w.request_redraw();
        });
    }

    /// Hide the flyout without destroying it (it stays alive in the tray).
    pub fn hide(&self) {
        let _ = self.ui.hide();
    }

    /// Invoke `handler` whenever the flyout window loses focus (the user clicked
    /// outside it / activated another window).
    ///
    /// Standard tray-flyout dismissal: `slint` exposes no window-deactivate
    /// callback, so the shared winit hook (installed in [`new`](Self::new)) taps
    /// the raw `WindowEvent::Focused(false)` and calls this handler. The event
    /// still fires for a borderless (`no-frame`) top-level window. The handler
    /// routes back through the app so the flyout's visibility state stays
    /// consistent (P0 live-QA bug 5). A single winit hook serves both this and
    /// the fractional-DPI buffer keeper.
    pub fn on_focus_lost(&self, handler: impl FnMut() + 'static) {
        *self.focus_lost.borrow_mut() = Some(Box::new(handler));
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
        dimming_on: row.dimming_on,
        has_floor: row.has_hardware_floor(),
        marker_fraction: row.transition_fraction(),
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

/// Binding-layer regression tests that drive the *real* `.slint` widgets through
/// the headless `i-slint-backend-testing` backend, catching bugs the pure
/// view-model tests cannot see (they live in the `.slint` ↔ shell wiring).
///
/// Gated behind the `smoke` feature (which pulls the testing backend) so the
/// default test build stays backend-free; run under `--all-features`.
#[cfg(all(test, feature = "smoke"))]
mod binding_tests {
    use super::*;
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};
    use i_slint_backend_testing::ElementHandle;

    fn snapshot(serial: &str, level: u8) -> DisplaySnapshot {
        DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap(),
            name: format!("Monitor {serial}"),
            kind: DisplayKind::ExternalDdc,
            user_level_pct: level,
            capabilities: Capabilities::default(),
        }
    }

    // Defect 5: the flyout "Link all" toggle did nothing. The footer pill bound
    // `checked <=> link-all` two-way, but nothing ever wrote `checked` and the
    // shell did not re-render, so the pill stayed frozen and every click re-sent
    // the same value — it could never turn on/off. This drives the actual widget
    // through the .slint binding (a pure `FlyoutVm` test cannot: `link_toggled`
    // was always called correctly). Proven red against the pre-fix markup/shell.
    #[test]
    fn link_all_toggle_round_trips_through_the_binding() {
        i_slint_backend_testing::init_no_event_loop();

        let mut vm = FlyoutVm::new();
        vm.set_displays(vec![snapshot("A", 40), snapshot("B", 70)]);
        let vm = Rc::new(RefCell::new(vm));
        let shell = FlyoutShell::new(vm.clone()).expect("shell instantiates");
        // The link handler is registered inside `on_command`.
        shell.on_command(|_cmd| {});

        let switch = || {
            ElementHandle::find_by_accessible_label(&shell.ui, "Link all displays")
                .next()
                .expect("the Link-all switch exists")
        };

        // Initially off, in both the VM and the rendered pill.
        assert!(!vm.borrow().link_all());
        assert_eq!(switch().accessible_checked(), Some(false));

        // Click it: the VM turns on AND the pill reflects it (needs the re-render).
        switch().invoke_accessible_default_action();
        assert!(vm.borrow().link_all(), "VM link-all did not turn on");
        assert_eq!(
            switch().accessible_checked(),
            Some(true),
            "the pill did not reflect the on state (frozen toggle / missing re-render)"
        );

        // Click again: it must turn back off — the pre-fix frozen `checked` kept
        // re-sending `true`, so this is the assertion the old code fails hardest.
        switch().invoke_accessible_default_action();
        assert!(
            !vm.borrow().link_all(),
            "VM link-all did not turn back off (the toggle was stuck on)"
        );
        assert_eq!(switch().accessible_checked(), Some(false));
    }
}

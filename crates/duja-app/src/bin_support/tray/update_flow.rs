//! The update-check flow: the manual and once-a-day background checks, the
//! background worker that folds a result back onto the Slint loop, and the
//! tray-menu/tooltip/toast surface for a newly-discovered release.
//!
//! These are [`AppState`] methods living in their own file — the same `impl`,
//! split by concern rather than by type.

use std::time::Instant;

use duja_ui::UpdateStatus;
use tracing::warn;

use crate::bin_support::toast;
use crate::bin_support::updates::{self, HttpsTransport, UpdateOutcome};

use super::policy::due_for_check;
use super::state::AppState;
use super::{unix_now, with_app};

/// The background update-check interval: at most once a day.
pub(super) const UPDATE_CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

impl AppState {
    /// The manual update check (settings "Check now"): always runs regardless of
    /// the `update_check` toggle — invoking it is itself the opt-in — but not
    /// while another check is already in flight.
    pub(super) fn start_update_check(&mut self) {
        if self.update_check_in_flight {
            return;
        }
        self.spawn_update_check(false);
    }

    /// The once-a-day background check, gated so it never hammers the API or
    /// spams the user: only while the check is enabled, no check is in flight,
    /// no update is already surfaced, and a day has passed since the last check.
    ///
    /// Called from real interactions ([`AppState::handle_action`]) and once at
    /// startup — never on a timer, so the process still sleeps when idle.
    pub(super) fn maybe_background_update_check(&mut self) {
        if !self.config.general.update_check
            || self.update_check_in_flight
            || self.update_available.is_some()
            || !due_for_check(
                unix_now(),
                self.state.last_update_check(),
                UPDATE_CHECK_INTERVAL_SECS,
            )
        {
            return;
        }
        self.spawn_update_check(true);
    }

    /// Run the check on a background thread (never blocks the UI thread), record
    /// the timestamp so a failure also waits a day, and fold the result back
    /// onto the Slint loop. `background` selects how the outcome is surfaced.
    fn spawn_update_check(&mut self, background: bool) {
        self.update_check_in_flight = true;
        self.state.record_update_check(unix_now());
        let _ = self.state.maybe_flush(Instant::now());
        let spawned = std::thread::Builder::new()
            .name("duja-update-check".to_owned())
            .spawn(move || {
                let outcome = updates::check_for_update(&HttpsTransport, env!("CARGO_PKG_VERSION"));
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(move |app| app.on_update_outcome(outcome, background));
                });
            });
        if spawned.is_err() {
            // The worker never ran, so nothing will clear the guard via
            // `on_update_outcome` — reset it now so checks aren't wedged off.
            self.update_check_in_flight = false;
        }
    }

    /// Fold a completed update check back into the UI. Always reflects the
    /// status into the settings window (it may be open); a *background*
    /// `UpdateAvailable` additionally surfaces the tray item, tooltip, and toast.
    fn on_update_outcome(&mut self, outcome: UpdateOutcome, background: bool) {
        self.update_check_in_flight = false;
        self.settings_vm
            .borrow_mut()
            .set_update_status(update_status_from(outcome.clone()));
        self.settings_shell
            .update_from_vm(&self.settings_vm.borrow());
        if background && let UpdateOutcome::UpdateAvailable { version } = outcome {
            self.surface_update_available(&version);
        }
    }

    /// Surface a newly-discovered release: add the "Update available" item to the
    /// top of the tray menu (once), refresh its label, set the tray tooltip, and
    /// raise a best-effort toast. Deduplicated so the same version acts once.
    fn surface_update_available(&mut self, version: &str) {
        use tray_icon::menu::PredefinedMenuItem;

        if self.update_available.as_deref() == Some(version) {
            return;
        }
        let first = self.update_available.is_none();
        self.update_available = Some(version.to_owned());
        self.update_item
            .set_text(format!("Update available — {version}"));
        if first {
            // Prepend the item + a separator above Open/Settings/… exactly once;
            // a later version change only updates the label and re-toasts.
            let sep = PredefinedMenuItem::separator();
            if let Err(e) = self.menu.prepend_items(&[&self.update_item, &sep]) {
                warn!(error = %e, "failed to add the update menu item");
            }
        }
        if let Err(e) = self.tray.set_tooltip(Some("Duja — update available")) {
            warn!(error = %e, "failed to set the update tooltip");
        }
        toast::notify_update_available(version);
    }
}

/// Map an update-check [`UpdateOutcome`] onto the settings [`UpdateStatus`].
fn update_status_from(outcome: UpdateOutcome) -> UpdateStatus {
    match outcome {
        UpdateOutcome::UpToDate => UpdateStatus::UpToDate,
        UpdateOutcome::UpdateAvailable { version } => UpdateStatus::Available { version },
        UpdateOutcome::Failed(_) => UpdateStatus::Failed,
    }
}

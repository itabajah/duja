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

use super::policy::{UPDATE_CHECK_INTERVAL_SECS, due_for_check};
use super::state::AppState;
use super::{unix_now, with_app};

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
        if self.update_available.as_deref() == Some(version) {
            return;
        }
        self.update_available = Some(version.to_owned());
        // De-duplication by *version* stays here; "show the item exactly once"
        // moved into the tray, because that part is a property of the backend
        // rather than of updates — `tray-icon` must not prepend twice and `ksni`
        // rebuilds its menu from state and cannot.
        if let Err(e) = self.tray.announce_update(version) {
            warn!(error = %e, "failed to add the update menu item");
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

#[cfg(test)]
mod tests {
    //! Two kinds of test live here now, and the second kind is new.
    //!
    //! [`update_status_from`] is a free function and was always reachable. The
    //! rest of this module is [`AppState`] methods, which measured **0 %** of
    //! regions on 2026-08-08 because nothing could build an `AppState` - the
    //! sentence `docs/debt.md` D-102 re-triaged. The fixture that answers it is
    //! [`crate::bin_support::tray::state::fixture`], and what it is pointed at
    //! first is the de-duplication policy, because a comment in this file claims
    //! that policy lives *here* rather than in the tray and nothing checked that
    //! it did.

    use duja_core::config::Config;

    use super::super::state::fixture::harness;
    use super::update_status_from;
    use crate::bin_support::toast::recorder as toasts;
    use crate::bin_support::updates::UpdateOutcome;
    use duja_ui::UpdateStatus;

    /// Every toast this thread has been asked to show, and a clean slate first.
    ///
    /// The clear is not hygiene. Windows' toast arm is a **real**
    /// `ToastNotification` under the `AppUserModelID` the installer stamps on the
    /// Start-Menu shortcut, so before `toast`'s seam existed these tests put four
    /// fabricated "Duja update available" notifications into the operator's
    /// Action Center on every `cargo test`. The recorder is what stands in for
    /// that now, and asserting on it is what keeps the seam from quietly
    /// regressing to the thing it replaced.
    fn toasts_after(exercise: impl FnOnce()) -> Vec<String> {
        toasts::clear();
        exercise();
        toasts::shown()
    }

    /// The same version surfaces once, however many times it is announced.
    ///
    /// `surface_update_available`'s comment says de-duplication *by version*
    /// "stays here" while "show the item exactly once" moved into the tray,
    /// because the second is a backend obligation and the first is not. That is
    /// a real split and it was unchecked in both directions: the tray's
    /// `update_shown` flag would mask a missing guard here on Windows and macOS,
    /// and on Linux there is no flag at all, so a lost guard would re-announce
    /// on every check for as long as the release stayed newest.
    ///
    /// Goes red if the early return is removed: two calls, two announcements.
    #[test]
    fn the_same_version_is_announced_to_the_tray_only_once() {
        let mut h = harness(Config::default());

        let toasted = toasts_after(|| {
            h.app.surface_update_available("v9.9.9");
            h.app.surface_update_available("v9.9.9");
        });

        assert_eq!(
            toasted,
            ["v9.9.9"],
            "the toast is behind the same guard, and a duplicate one is what a              user would actually notice"
        );
        let (_, tooltips, updates) = h.app.tray.recorded();
        assert_eq!(updates, ["v9.9.9"], "one announcement, not two");
        assert_eq!(
            tooltips,
            [Some("Duja — update available".to_owned())],
            "and one tooltip write, for the same reason"
        );
    }

    /// De-duplication is **by version**, so a newer release announces again.
    ///
    /// The direction that matters more, and the one a `bool` "already
    /// announced" flag would have got wrong: a user who leaves Duja running
    /// across two releases must be told about the second.
    #[test]
    fn a_newer_version_announces_again() {
        let mut h = harness(Config::default());

        h.app.surface_update_available("v9.9.9");
        h.app.surface_update_available("v9.9.10");

        let (_, _, updates) = h.app.tray.recorded();
        assert_eq!(updates, ["v9.9.9", "v9.9.10"]);
        assert_eq!(h.app.update_available.as_deref(), Some("v9.9.10"));
    }

    /// A *foreground* check reflects into the settings window and does **not**
    /// touch the tray; only a background one surfaces the menu item and toast.
    ///
    /// The distinction is the whole reason `on_update_outcome` takes a
    /// `background` flag, and dropping it would put a tray item and a toast in
    /// front of a user who had just clicked "Check now" and was already looking
    /// at the answer.
    #[test]
    fn a_foreground_check_reflects_but_does_not_surface() {
        let mut h = harness(Config::default());
        toasts::clear();

        h.app.on_update_outcome(
            UpdateOutcome::UpdateAvailable {
                version: "v9.9.9".to_owned(),
            },
            false,
        );

        let (_, tooltips, updates) = h.app.tray.recorded();
        assert!(updates.is_empty(), "the tray must stay quiet: {updates:?}");
        assert!(tooltips.is_empty(), "{tooltips:?}");
        assert!(
            toasts::shown().is_empty(),
            "and so must the desktop: a user who just clicked Check now is              already looking at the answer"
        );
        assert_eq!(
            h.app.settings_vm.borrow().update_status(),
            &UpdateStatus::Available {
                version: "v9.9.9".to_owned()
            },
            "but the settings window is told either way - it may be open"
        );
    }

    /// And the background one does both.
    #[test]
    fn a_background_check_surfaces_to_the_tray() {
        let mut h = harness(Config::default());

        h.app.on_update_outcome(
            UpdateOutcome::UpdateAvailable {
                version: "v9.9.9".to_owned(),
            },
            true,
        );

        let (_, _, updates) = h.app.tray.recorded();
        assert_eq!(updates, ["v9.9.9"]);
    }

    #[test]
    fn every_outcome_maps_to_its_own_status() {
        assert_eq!(
            update_status_from(UpdateOutcome::UpToDate),
            UpdateStatus::UpToDate
        );
        assert_eq!(
            update_status_from(UpdateOutcome::UpdateAvailable {
                version: "v9.9.9".to_owned()
            }),
            UpdateStatus::Available {
                version: "v9.9.9".to_owned()
            }
        );
    }

    #[test]
    fn a_failure_reports_failed_rather_than_up_to_date() {
        // The arm worth pinning, though not for the reason first written here.
        // That comment said the two "both render as 'no update', so transposing
        // them is invisible in the settings window" - and review disproved it:
        // `settings_shell.rs` renders "Up to date" and "Couldn't check for
        // updates", which are plainly different lines. What is true is the
        // consequence: a user whose check failed is told they are current, which
        // is the one wrong answer this mapping can give that a user will act on.
        // What genuinely collapses the two is `SettingsVm::has_update()`, a
        // different surface with its own tests.
        assert_eq!(
            update_status_from(UpdateOutcome::Failed("connection refused".to_owned())),
            UpdateStatus::Failed
        );
        // The reason string is deliberately dropped rather than surfaced: the
        // settings window shows a neutral line, per `UpdateOutcome::Failed`'s own
        // doc. Pinned so a future "helpful" change to show it has to change this
        // test and read that doc on the way past.
        assert_eq!(
            update_status_from(UpdateOutcome::Failed(String::new())),
            UpdateStatus::Failed
        );
    }

    #[test]
    fn the_version_string_is_carried_through_unaltered() {
        // The settings window prints this string, so trimming or re-formatting
        // it here changes what a user reads.
        //
        // It does NOT desynchronise the tray menu, which is what this comment
        // claimed until review: `on_update_outcome` passes the tray its version
        // from `outcome` directly, never from this function's output, so the two
        // paths are independent. Worth keeping the correction rather than the
        // tidier wrong reason, because "these two surfaces share a string" is
        // exactly the kind of belief that makes a later refactor merge them.
        for v in ["v1.0.0", "0.1.6", "v2.0.0-rc.1"] {
            assert_eq!(
                update_status_from(UpdateOutcome::UpdateAvailable {
                    version: v.to_owned()
                }),
                UpdateStatus::Available {
                    version: v.to_owned()
                }
            );
        }
    }
}

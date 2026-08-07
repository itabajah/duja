//! The hotkey registrar on a platform where Duja registers none.
//!
//! Stands in for [`super::hotkey_os`] on Linux, under the same module name (see
//! the `#[path]` in `tray.rs`), so `wiring.rs` and `state.rs` import one set of
//! names on every target.
//!
//! # Why there are no Linux hotkeys
//!
//! `global-hotkey`'s Linux backend is X11-only. Duja's Linux support is not:
//! [ADR-0011]'s whole premise is that the session type is a runtime property, and
//! wave 4 shipped a Wayland dimming path precisely because a Wayland session is
//! not an edge case. A registrar that worked on X11 and silently did nothing on
//! Wayland would be the failure mode this project calls "vanished" — a setting
//! the UI shows as bound, that never fires, with nothing said about it.
//!
//! So nothing is registered and the settings window is told. The rows render
//! greyed, exactly as an OS-rejected binding does, but by way of
//! [`hotkey::RegisterResult::Unsupported`] rather than `OsRejected`, because the
//! two differ in what they tell the user to do next: `OsRejected` invites them to
//! pick another combo, which here would send them round a loop with no exit.
//!
//! # What would replace this
//!
//! Not `global-hotkey`. A Wayland session has no global grab for a client to
//! take, by design — the compositor owns shortcuts, and the portal route is
//! `org.freedesktop.portal.GlobalShortcuts`, which is a *different* mechanism
//! with a user-consent step rather than a wider `global-hotkey`. That is a
//! feature with a design, not a dependency swap, and `docs/debt.md` carries it.
//!
//! [ADR-0011]: https://github.com/itabajah/duja/blob/main/docs/adr/0011-linux-software-dimming.md

use std::collections::BTreeMap;

use tracing::warn;

use crate::bin_support::hotkey::{self, Accelerator, HotkeyAction};

use super::Action;
use super::policy::HOTKEY_BRIGHTNESS_STEP;

/// A registrar that registers nothing, and reports that rather than a refusal.
///
/// Deliberately holds no state: there is nothing to clear, no id to map back, and
/// no partial success to remember.
pub(super) struct OsHotkeyRegistrar;

impl OsHotkeyRegistrar {
    /// Build the registrar. Infallible — there is no manager to create.
    pub(super) const fn new() -> Self {
        Self
    }

    /// Resolve a fired hotkey id to its action.
    ///
    /// Always `None`: no id was ever handed out, so none can arrive. Kept because
    /// `AppState::on_hotkey_fired` is not `cfg`-gated, and gating it would put a
    /// second platform switch in a file whose job is not platform switching.
    #[allow(clippy::unused_self)] // RATIONALE: matches the real registrar's shape.
    pub(super) const fn action_for_id(&self, _id: u32) -> Option<HotkeyAction> {
        None
    }
}

impl hotkey::HotkeyRegistrar for OsHotkeyRegistrar {
    fn clear(&mut self) {}

    fn register(&mut self, _accel: &Accelerator, _action: HotkeyAction) -> bool {
        false
    }

    fn rejection(&self) -> hotkey::RegisterResult {
        hotkey::RegisterResult::Unsupported
    }
}

/// Log the parse errors and conflicts in a resolved [`hotkey::HotkeyPlan`].
///
/// The same diagnostics as the real backend, plus one line saying why none of
/// them will be registered. A user who wrote a `[hotkeys]` section and saw only
/// silence would reasonably conclude their syntax was wrong.
pub(super) fn log_hotkey_issues(plan: &hotkey::HotkeyPlan) {
    for err in &plan.errors {
        warn!(key = %err.key, binding = %err.raw, reason = %err.reason, "ignoring invalid hotkey binding");
    }
    for conflict in &plan.conflicts {
        let actions: Vec<&str> = conflict.actions.iter().map(|a| a.config_key()).collect();
        warn!(combo = %conflict.accel, ?actions, "hotkey combo bound to multiple actions; skipping all");
    }
    if !plan.bindings.is_empty() {
        warn!(
            bindings = plan.bindings.len(),
            "global hotkeys are not registered on Linux; the settings window shows them as unavailable"
        );
    }
}

/// Index a batch of [`hotkey::RegisterOutcome`]s by action for settings-row
/// feedback (the last outcome per action wins).
pub(super) fn outcomes_by_action(
    outcomes: &[hotkey::RegisterOutcome],
) -> BTreeMap<HotkeyAction, hotkey::RegisterResult> {
    outcomes.iter().map(|o| (o.action, o.result)).collect()
}

/// Install the hotkey event handler. Nothing to install: no source can fire.
pub(super) const fn install_hotkey_event_handler() {}

/// Map a [`HotkeyAction`] onto the tray [`Action`] it triggers.
///
/// Unreachable through [`OsHotkeyRegistrar::action_for_id`], which never answers
/// `Some`. Kept in step with the real backend rather than deleted, because the
/// portal route named in this module's header would make it reachable again and a
/// mapping that had drifted in the meantime is worse than one that is unused.
pub(super) const fn action_for(action: HotkeyAction) -> Action {
    match action {
        HotkeyAction::BrightnessUp => Action::Nudge(HOTKEY_BRIGHTNESS_STEP),
        HotkeyAction::BrightnessDown => Action::Nudge(-HOTKEY_BRIGHTNESS_STEP),
        HotkeyAction::ToggleFlyout => Action::Toggle,
    }
}

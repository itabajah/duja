//! The OS side of the global-hotkey table: the [`hotkey::HotkeyRegistrar`]
//! implementation that actually talks to `global_hotkey`, the event handler that
//! routes a fired hotkey onto the Slint loop, and the pure Duja → `global_hotkey`
//! accelerator conversion.
//!
//! The *policy* (parsing, conflict detection, planning) lives in
//! [`hotkey`]; only the OS-touching half is here, behind that seam.

use std::collections::BTreeMap;

use global_hotkey::hotkey::{Code, HotKey, Modifiers as GhkModifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tracing::{debug, warn};

use crate::bin_support::hotkey::{self, Accelerator, HotkeyAction, Modifiers as AccelModifiers};

use super::policy::HOTKEY_BRIGHTNESS_STEP;
use super::{Action, with_app};

/// The live global-hotkey registrar: owns the OS manager, the currently
/// registered [`HotKey`]s (so they can be unregistered on a re-plan), and the
/// id → action map the event handler resolves against.
///
/// Implements the pure [`hotkey::HotkeyRegistrar`] seam so [`hotkey::apply_plan`]
/// drives it; the OS-touching parts live here, behind that seam.
pub(super) struct OsHotkeyRegistrar {
    /// The OS manager, kept alive so registrations stay live. `None` if the
    /// manager could not be created (hotkeys then silently unavailable).
    manager: Option<GlobalHotKeyManager>,
    /// The hotkeys currently registered with the OS (for `unregister_all`).
    registered: Vec<HotKey>,
    /// Which action each live hotkey id fires.
    map: BTreeMap<u32, HotkeyAction>,
}

impl OsHotkeyRegistrar {
    /// Create the registrar, eagerly building the OS manager on this (main)
    /// thread. A manager failure is logged and leaves hotkeys unavailable.
    pub(super) fn new() -> Self {
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => Some(manager),
            Err(e) => {
                warn!(error = %e, "global hotkey manager unavailable; hotkeys disabled");
                None
            }
        };
        OsHotkeyRegistrar {
            manager,
            registered: Vec::new(),
            map: BTreeMap::new(),
        }
    }

    /// The action a live hotkey id fires, if any.
    pub(super) fn action_for_id(&self, id: u32) -> Option<HotkeyAction> {
        self.map.get(&id).copied()
    }
}

impl hotkey::HotkeyRegistrar for OsHotkeyRegistrar {
    fn clear(&mut self) {
        if let Some(manager) = &self.manager
            && !self.registered.is_empty()
            && let Err(e) = manager.unregister_all(&self.registered)
        {
            warn!(error = %e, "failed to unregister previous hotkeys");
        }
        self.registered.clear();
        self.map.clear();
    }

    fn register(&mut self, accel: &Accelerator, action: HotkeyAction) -> bool {
        let Some(manager) = &self.manager else {
            return false;
        };
        let Some(hk) = accel_to_hotkey(accel) else {
            warn!(accel = %accel, "hotkey key not supported by the OS backend; skipping");
            return false;
        };
        if accel.modifiers.is_empty() {
            warn!(accel = %accel, "modifierless global hotkey may capture the key system-wide");
        }
        let id = hk.id();
        match manager.register(hk) {
            Ok(()) => {
                self.registered.push(hk);
                self.map.insert(id, action);
                debug!(accel = %accel, action = action.config_key(), "registered hotkey");
                true
            }
            Err(e) => {
                warn!(accel = %accel, error = %e, "failed to register hotkey (already owned?); skipping");
                false
            }
        }
    }
}

/// Log the parse errors and conflicts in a resolved [`hotkey::HotkeyPlan`].
pub(super) fn log_hotkey_issues(plan: &hotkey::HotkeyPlan) {
    for err in &plan.errors {
        warn!(key = %err.key, binding = %err.raw, reason = %err.reason, "ignoring invalid hotkey binding");
    }
    for conflict in &plan.conflicts {
        let actions: Vec<&str> = conflict.actions.iter().map(|a| a.config_key()).collect();
        warn!(combo = %conflict.accel, ?actions, "hotkey combo bound to multiple actions; skipping all");
    }
}

/// Index a batch of [`hotkey::RegisterOutcome`]s by action for settings-row
/// feedback (the last outcome per action wins).
pub(super) fn outcomes_by_action(
    outcomes: &[hotkey::RegisterOutcome],
) -> BTreeMap<HotkeyAction, hotkey::RegisterResult> {
    outcomes.iter().map(|o| (o.action, o.result)).collect()
}

/// Install the global-hotkey event handler. A pressed hotkey is resolved to its
/// action against the live registrar (in the app state) on the Slint loop, so a
/// live re-registration is picked up without re-installing the handler.
pub(super) fn install_hotkey_event_handler() {
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        // Fire on the press edge only (the release edge arrives on global-hotkey's
        // worker thread); hop onto the Slint loop via `with_app`.
        if event.state() == HotKeyState::Pressed {
            let id = event.id();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(move |app| app.on_hotkey_fired(id));
            });
        }
    }));
}

/// Map a [`HotkeyAction`] onto the tray [`Action`] it triggers.
pub(super) fn action_for(action: HotkeyAction) -> Action {
    match action {
        HotkeyAction::BrightnessUp => Action::Nudge(HOTKEY_BRIGHTNESS_STEP),
        HotkeyAction::BrightnessDown => Action::Nudge(-HOTKEY_BRIGHTNESS_STEP),
        HotkeyAction::ToggleFlyout => Action::Toggle,
    }
}

/// Convert a parsed [`Accelerator`] into a `global_hotkey` [`HotKey`], or `None`
/// if the key has no `global_hotkey` [`Code`].
fn accel_to_hotkey(accel: &Accelerator) -> Option<HotKey> {
    let code = code_for_key(accel.key.as_str())?;
    Some(HotKey::new(Some(ghk_modifiers(accel.modifiers)), code))
}

/// Translate Duja's modifier set into `global_hotkey`'s.
fn ghk_modifiers(mods: AccelModifiers) -> GhkModifiers {
    let mut out = GhkModifiers::empty();
    if mods.contains(AccelModifiers::CONTROL) {
        out |= GhkModifiers::CONTROL;
    }
    if mods.contains(AccelModifiers::ALT) {
        out |= GhkModifiers::ALT;
    }
    if mods.contains(AccelModifiers::SHIFT) {
        out |= GhkModifiers::SHIFT;
    }
    if mods.contains(AccelModifiers::SUPER) {
        out |= GhkModifiers::SUPER;
    }
    out
}

/// Map a canonical key token (see [`hotkey`]) onto a `global_hotkey` [`Code`]
/// via its W3C `KeyboardEvent.code` name.
fn code_for_key(token: &str) -> Option<Code> {
    use std::str::FromStr as _;
    let w3c = match token {
        "UP" => "ArrowUp".to_owned(),
        "DOWN" => "ArrowDown".to_owned(),
        "LEFT" => "ArrowLeft".to_owned(),
        "RIGHT" => "ArrowRight".to_owned(),
        "SPACE" => "Space".to_owned(),
        "ENTER" => "Enter".to_owned(),
        "TAB" => "Tab".to_owned(),
        "ESCAPE" => "Escape".to_owned(),
        "HOME" => "Home".to_owned(),
        "END" => "End".to_owned(),
        "PAGEUP" => "PageUp".to_owned(),
        "PAGEDOWN" => "PageDown".to_owned(),
        "INSERT" => "Insert".to_owned(),
        "DELETE" => "Delete".to_owned(),
        "BACKSPACE" => "Backspace".to_owned(),
        other => {
            if let Some(digits) = other.strip_prefix('F')
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
            {
                other.to_owned() // F1..=F24
            } else {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_uppercase() => format!("Key{c}"),
                    (Some(c), None) if c.is_ascii_digit() => format!("Digit{c}"),
                    _ => return None,
                }
            }
        }
    };
    Code::from_str(&w3c).ok()
}

#[cfg(test)]
mod tests {
    //! Coverage for the pure accelerator → `global_hotkey` conversion boundary
    //! and the action mapping. The actual OS delivery of a `WM_HOTKEY` to the
    //! registered handler is NOT unit-tested here (global-hotkey's test story is
    //! weak and synthesising `WM_HOTKEY` does not reliably reach its handler); it
    //! is covered by the P1 `spike/eventloop` proof and manual hardware QA.
    use super::{Accelerator, Action, Code, GhkModifiers, HotkeyAction};
    use super::{accel_to_hotkey, action_for, code_for_key, ghk_modifiers};

    fn accel(s: &str) -> Accelerator {
        Accelerator::parse(s).expect("valid accelerator")
    }

    #[test]
    fn code_for_key_maps_every_supported_key_family() {
        assert_eq!(code_for_key("UP"), Some(Code::ArrowUp));
        assert_eq!(code_for_key("DOWN"), Some(Code::ArrowDown));
        assert_eq!(code_for_key("F9"), Some(Code::F9));
        assert_eq!(code_for_key("F24"), Some(Code::F24));
        assert_eq!(code_for_key("A"), Some(Code::KeyA));
        assert_eq!(code_for_key("7"), Some(Code::Digit7));
        assert_eq!(code_for_key("SPACE"), Some(Code::Space));
        assert_eq!(code_for_key("PAGEUP"), Some(Code::PageUp));
        // A token with no W3C code maps to None (registration then skips it).
        assert_eq!(code_for_key("NOPE"), None);
    }

    #[test]
    fn ghk_modifiers_translates_each_flag() {
        let all = accel("Ctrl+Alt+Shift+Super+Up");
        let mods = ghk_modifiers(all.modifiers);
        assert!(mods.contains(GhkModifiers::CONTROL));
        assert!(mods.contains(GhkModifiers::ALT));
        assert!(mods.contains(GhkModifiers::SHIFT));
        assert!(mods.contains(GhkModifiers::SUPER));

        let none = ghk_modifiers(accel("F9").modifiers);
        assert!(none.is_empty());
    }

    #[test]
    fn accel_to_hotkey_builds_the_expected_hotkey() {
        let hk = accel_to_hotkey(&accel("Ctrl+Alt+Up")).expect("convertible");
        assert_eq!(hk.key, Code::ArrowUp);
        assert!(hk.mods.contains(GhkModifiers::CONTROL));
        assert!(hk.mods.contains(GhkModifiers::ALT));
        assert!(!hk.mods.contains(GhkModifiers::SHIFT));
    }

    #[test]
    fn action_for_maps_actions_to_tray_actions() {
        assert!(matches!(
            action_for(HotkeyAction::BrightnessUp),
            Action::Nudge(n) if n > 0
        ));
        assert!(matches!(
            action_for(HotkeyAction::BrightnessDown),
            Action::Nudge(n) if n < 0
        ));
        assert!(matches!(
            action_for(HotkeyAction::ToggleFlyout),
            Action::Toggle
        ));
    }
}

//! Pure parsing and conflict-detection for Duja's global hotkeys.
//!
//! This module is **std-only and platform-independent** so its unit tests run on
//! every CI OS: it turns the `[hotkeys]` config table (action → accelerator
//! string) into a validated, typed [`HotkeyPlan`], and never touches the OS.
//! The Windows tray assembly (the Windows-only `bin_support::tray`) converts each parsed
//! [`Accelerator`] into a `global_hotkey::hotkey::HotKey` at the boundary and
//! registers it.
//!
//! # Accelerator grammar
//!
//! An accelerator is a `+`-separated list of tokens: zero or more modifiers
//! (`Ctrl`/`Control`, `Alt`, `Shift`, `Super`/`Win`/`Cmd`/`Meta`) followed by
//! exactly one key (an arrow, an `F1`–`F24`, a letter `A`–`Z`, a digit `0`–`9`,
//! or a small set of named keys). Parsing is case-insensitive and
//! order-insensitive across modifiers; a bare modifier with no key, an empty
//! string, an unknown token, or two keys are rejected with a typed
//! [`AccelError`].

// RATIONALE: cross-platform pure module compiled on every OS; the tray consumer
// that converts + registers accelerators is Windows-only, so on other targets
// the public surface is unused. This mirrors the sibling `settings`/`dimming`
// modules' dead-code allow.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::BTreeMap;
use std::fmt;

/// A keyboard-modifier set, order-independent (a small bitmask).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub struct Modifiers(u8);

impl Modifiers {
    /// The Control modifier.
    pub const CONTROL: Modifiers = Modifiers(1 << 0);
    /// The Alt / Option modifier.
    pub const ALT: Modifiers = Modifiers(1 << 1);
    /// The Shift modifier.
    pub const SHIFT: Modifiers = Modifiers(1 << 2);
    /// The Super / Windows / Command / Meta modifier.
    pub const SUPER: Modifiers = Modifiers(1 << 3);

    /// Whether no modifier is set.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `other`'s bits are all set in `self`.
    #[must_use]
    pub fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }

    fn insert(&mut self, other: Modifiers) {
        self.0 |= other.0;
    }
}

/// The non-modifier key of an accelerator, stored as a normalized canonical
/// token (e.g. `"UP"`, `"F9"`, `"A"`, `"7"`, `"SPACE"`).
///
/// The canonical form is what [`Accelerator`] compares and prints; the Windows
/// boundary maps it to a `global_hotkey::hotkey::Code`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(String);

impl Key {
    /// The canonical token for this key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A parsed accelerator: a modifier set plus one key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Accelerator {
    /// The modifiers that must be held.
    pub modifiers: Modifiers,
    /// The triggering key.
    pub key: Key,
}

impl Accelerator {
    /// Parse an accelerator string (e.g. `"Ctrl+Alt+Up"`).
    ///
    /// # Errors
    /// Returns an [`AccelError`] for an empty string, an unknown token, a
    /// missing key (bare modifiers), or more than one non-modifier key.
    pub fn parse(input: &str) -> Result<Self, AccelError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(AccelError::Empty);
        }

        let mut modifiers = Modifiers::default();
        let mut key: Option<Key> = None;

        for raw in trimmed.split('+') {
            let token = raw.trim();
            if token.is_empty() {
                // A stray `+` (leading, trailing, or doubled).
                return Err(AccelError::UnknownToken(raw.to_owned()));
            }
            if let Some(m) = parse_modifier(token) {
                modifiers.insert(m);
                continue;
            }
            match normalize_key(token) {
                Some(canonical) => {
                    if key.is_some() {
                        return Err(AccelError::MultipleKeys);
                    }
                    key = Some(Key(canonical));
                }
                None => return Err(AccelError::UnknownToken(token.to_owned())),
            }
        }

        match key {
            Some(key) => Ok(Accelerator { modifiers, key }),
            None => Err(AccelError::NoKey),
        }
    }
}

impl fmt::Display for Accelerator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (flag, name) in [
            (Modifiers::CONTROL, "Ctrl"),
            (Modifiers::ALT, "Alt"),
            (Modifiers::SHIFT, "Shift"),
            (Modifiers::SUPER, "Super"),
        ] {
            if self.modifiers.contains(flag) {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(self.key.as_str())
    }
}

/// A failure parsing an accelerator string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccelError {
    /// The accelerator string was empty (or all whitespace).
    #[error("empty accelerator")]
    Empty,
    /// The accelerator had modifiers but no triggering key.
    #[error("accelerator has no key (only modifiers)")]
    NoKey,
    /// The accelerator named more than one non-modifier key.
    #[error("accelerator has more than one key")]
    MultipleKeys,
    /// A token was neither a known modifier nor a known key.
    #[error("unknown accelerator token `{0}`")]
    UnknownToken(String),
}

/// The Duja action a hotkey binding drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HotkeyAction {
    /// Raise the brightness of every display by the configured step.
    BrightnessUp,
    /// Lower the brightness of every display by the configured step.
    BrightnessDown,
    /// Toggle the flyout's visibility.
    ToggleFlyout,
}

impl HotkeyAction {
    /// Every action, for exhaustive iteration.
    pub const ALL: [HotkeyAction; 3] = [
        HotkeyAction::BrightnessUp,
        HotkeyAction::BrightnessDown,
        HotkeyAction::ToggleFlyout,
    ];

    /// The config-table key for this action (e.g. `"brightness_up"`).
    #[must_use]
    pub fn config_key(self) -> &'static str {
        match self {
            HotkeyAction::BrightnessUp => "brightness_up",
            HotkeyAction::BrightnessDown => "brightness_down",
            HotkeyAction::ToggleFlyout => "toggle_flyout",
        }
    }

    /// Parse a config-table key into an action. Both underscore and hyphen
    /// spellings are accepted (`brightness-up` == `brightness_up`).
    #[must_use]
    pub fn from_config_key(key: &str) -> Option<HotkeyAction> {
        let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
        HotkeyAction::ALL
            .into_iter()
            .find(|action| action.config_key() == normalized)
    }
}

/// One resolved binding: an action, its accelerator, and the raw config string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The action this binding drives.
    pub action: HotkeyAction,
    /// The parsed accelerator.
    pub accel: Accelerator,
    /// The original accelerator string from the config (for logging).
    pub raw: String,
}

/// A per-combo conflict: two or more actions bound to the same accelerator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The canonical accelerator the actions share.
    pub accel: Accelerator,
    /// The clashing actions, in a stable order.
    pub actions: Vec<HotkeyAction>,
}

/// A per-entry parse failure: the config key and why it did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingError {
    /// The config-table key (as written).
    pub key: String,
    /// The raw accelerator string.
    pub raw: String,
    /// Why parsing failed, or that the key names no known action.
    pub reason: BindingErrorKind,
}

/// Why a `[hotkeys]` entry could not become a [`Binding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingErrorKind {
    /// The key was not one of the recognised action names.
    UnknownAction,
    /// The accelerator string failed to parse.
    BadAccelerator(AccelError),
}

impl fmt::Display for BindingErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindingErrorKind::UnknownAction => f.write_str("unknown action"),
            BindingErrorKind::BadAccelerator(err) => write!(f, "{err}"),
        }
    }
}

/// The outcome of resolving the whole `[hotkeys]` table: the valid bindings, the
/// entries that failed to parse, and any accelerator collisions between actions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotkeyPlan {
    /// Successfully parsed bindings, sorted by action.
    pub bindings: Vec<Binding>,
    /// Entries that named no known action or held an unparseable accelerator.
    pub errors: Vec<BindingError>,
    /// Accelerators bound to more than one action.
    pub conflicts: Vec<Conflict>,
}

/// Resolve a `[hotkeys]` config table (action key → accelerator string) into a
/// [`HotkeyPlan`], collecting every parse error and every combo collision rather
/// than failing on the first.
///
/// Bindings involved in a conflict are still returned; the caller decides
/// whether to register them (the tray logs a WARN and skips one side).
#[must_use]
pub fn resolve(hotkeys: &BTreeMap<String, String>) -> HotkeyPlan {
    let mut plan = HotkeyPlan::default();

    for (key, raw) in hotkeys {
        let Some(action) = HotkeyAction::from_config_key(key) else {
            plan.errors.push(BindingError {
                key: key.clone(),
                raw: raw.clone(),
                reason: BindingErrorKind::UnknownAction,
            });
            continue;
        };
        match Accelerator::parse(raw) {
            Ok(accel) => plan.bindings.push(Binding {
                action,
                accel,
                raw: raw.clone(),
            }),
            Err(err) => plan.errors.push(BindingError {
                key: key.clone(),
                raw: raw.clone(),
                reason: BindingErrorKind::BadAccelerator(err),
            }),
        }
    }

    plan.bindings.sort_by_key(|b| b.action);
    plan.conflicts = detect_conflicts(&plan.bindings);
    plan
}

/// Group the bindings by their accelerator and report every accelerator held by
/// two or more actions.
#[must_use]
pub fn detect_conflicts(bindings: &[Binding]) -> Vec<Conflict> {
    let mut by_combo: BTreeMap<Accelerator, Vec<HotkeyAction>> = BTreeMap::new();
    for binding in bindings {
        by_combo
            .entry(binding.accel.clone())
            .or_default()
            .push(binding.action);
    }
    by_combo
        .into_iter()
        .filter(|(_, actions)| actions.len() > 1)
        .map(|(accel, mut actions)| {
            actions.sort_unstable();
            Conflict { accel, actions }
        })
        .collect()
}

/// Recognise a modifier token (case-insensitive), or `None`.
fn parse_modifier(token: &str) -> Option<Modifiers> {
    match token.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctl" => Some(Modifiers::CONTROL),
        "alt" | "option" | "opt" => Some(Modifiers::ALT),
        "shift" => Some(Modifiers::SHIFT),
        "super" | "win" | "windows" | "cmd" | "command" | "meta" => Some(Modifiers::SUPER),
        _ => None,
    }
}

/// Normalize a key token to its canonical form, or `None` if it is not a key
/// Duja supports as a hotkey trigger.
fn normalize_key(token: &str) -> Option<String> {
    let upper = token.to_ascii_uppercase();
    // Arrows (accept a couple of common spellings).
    let arrow = match upper.as_str() {
        "UP" | "ARROWUP" => Some("UP"),
        "DOWN" | "ARROWDOWN" => Some("DOWN"),
        "LEFT" | "ARROWLEFT" => Some("LEFT"),
        "RIGHT" | "ARROWRIGHT" => Some("RIGHT"),
        _ => None,
    };
    if let Some(a) = arrow {
        return Some(a.to_owned());
    }
    // Named keys.
    if matches!(
        upper.as_str(),
        "SPACE"
            | "ENTER"
            | "RETURN"
            | "TAB"
            | "ESCAPE"
            | "ESC"
            | "HOME"
            | "END"
            | "PAGEUP"
            | "PAGEDOWN"
            | "INSERT"
            | "DELETE"
            | "BACKSPACE"
    ) {
        return Some(canonical_named(&upper));
    }
    // Function keys F1..=F24.
    if let Some(num) = upper.strip_prefix('F')
        && let Ok(n) = num.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return Some(format!("F{n}"));
    }
    // A single letter A..=Z or digit 0..=9.
    if let Some(ch) = single_byte(&upper)
        && (ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        return Some(upper);
    }
    None
}

/// The sole byte of `s`, or `None` if `s` is not exactly one byte long.
fn single_byte(s: &str) -> Option<u8> {
    let mut bytes = s.bytes();
    match (bytes.next(), bytes.next()) {
        (Some(b), None) => Some(b),
        _ => None,
    }
}

/// Collapse alias spellings of named keys to one canonical token.
fn canonical_named(upper: &str) -> String {
    match upper {
        "RETURN" => "ENTER",
        "ESC" => "ESCAPE",
        other => other,
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accel(s: &str) -> Accelerator {
        Accelerator::parse(s).expect("valid accelerator")
    }

    #[test]
    fn parses_modifiers_and_key() {
        let a = accel("Ctrl+Alt+Up");
        assert!(a.modifiers.contains(Modifiers::CONTROL));
        assert!(a.modifiers.contains(Modifiers::ALT));
        assert!(!a.modifiers.contains(Modifiers::SHIFT));
        assert_eq!(a.key.as_str(), "UP");
    }

    #[test]
    fn parsing_is_case_insensitive() {
        assert_eq!(accel("ctrl+alt+up"), accel("CTRL+ALT+UP"));
        assert_eq!(accel("Control+Alt+Up"), accel("ctrl+ALT+up"));
    }

    #[test]
    fn modifier_order_does_not_matter() {
        assert_eq!(accel("Ctrl+Alt+Up"), accel("Alt+Ctrl+Up"));
        assert_eq!(accel("Shift+Ctrl+F9"), accel("Ctrl+Shift+F9"));
    }

    #[test]
    fn whitespace_around_tokens_is_tolerated() {
        assert_eq!(accel("  Ctrl + Alt + Up "), accel("Ctrl+Alt+Up"));
    }

    #[test]
    fn super_aliases_all_map_together() {
        for token in ["Super", "Win", "Windows", "Cmd", "Command", "Meta"] {
            let a = accel(&format!("{token}+Up"));
            assert!(a.modifiers.contains(Modifiers::SUPER), "{token}");
        }
    }

    #[test]
    fn function_letter_and_digit_keys_parse() {
        assert_eq!(accel("Ctrl+F9").key.as_str(), "F9");
        assert_eq!(accel("Ctrl+F24").key.as_str(), "F24");
        assert_eq!(accel("Ctrl+a").key.as_str(), "A");
        assert_eq!(accel("Alt+7").key.as_str(), "7");
    }

    #[test]
    fn named_key_aliases_canonicalize() {
        assert_eq!(accel("Ctrl+Return"), accel("Ctrl+Enter"));
        assert_eq!(accel("Ctrl+Esc"), accel("Ctrl+Escape"));
        assert_eq!(accel("Alt+ArrowUp"), accel("Alt+Up"));
    }

    #[test]
    fn empty_is_rejected() {
        assert_eq!(Accelerator::parse(""), Err(AccelError::Empty));
        assert_eq!(Accelerator::parse("   "), Err(AccelError::Empty));
    }

    #[test]
    fn bare_modifiers_are_rejected() {
        assert_eq!(Accelerator::parse("Ctrl"), Err(AccelError::NoKey));
        assert_eq!(Accelerator::parse("Ctrl+Alt"), Err(AccelError::NoKey));
        assert_eq!(Accelerator::parse("Shift+Super"), Err(AccelError::NoKey));
    }

    #[test]
    fn unknown_tokens_are_rejected() {
        assert!(matches!(
            Accelerator::parse("Ctrl+Splat"),
            Err(AccelError::UnknownToken(t)) if t == "Splat"
        ));
        assert!(matches!(
            Accelerator::parse("F99+Ctrl"),
            Err(AccelError::UnknownToken(_))
        ));
        // Stray separators.
        assert!(matches!(
            Accelerator::parse("Ctrl++Up"),
            Err(AccelError::UnknownToken(_))
        ));
        assert!(matches!(
            Accelerator::parse("Ctrl+Up+"),
            Err(AccelError::UnknownToken(_))
        ));
    }

    #[test]
    fn two_keys_are_rejected() {
        assert_eq!(
            Accelerator::parse("Ctrl+Up+Down"),
            Err(AccelError::MultipleKeys)
        );
        assert_eq!(Accelerator::parse("A+B"), Err(AccelError::MultipleKeys));
    }

    #[test]
    fn a_lone_key_needs_no_modifier() {
        // Modifierless accelerators are structurally valid (registration may warn
        // separately); parsing accepts them.
        let a = accel("F9");
        assert!(a.modifiers.is_empty());
        assert_eq!(a.key.as_str(), "F9");
    }

    #[test]
    fn display_round_trips_canonically() {
        // Modifiers print in canonical order; the key prints in its canonical
        // (upper-case) token form.
        assert_eq!(accel("alt+ctrl+up").to_string(), "Ctrl+Alt+UP");
        assert_eq!(accel("F9").to_string(), "F9");
    }

    #[test]
    fn action_config_keys_round_trip() {
        for action in HotkeyAction::ALL {
            assert_eq!(
                HotkeyAction::from_config_key(action.config_key()),
                Some(action)
            );
        }
        // Hyphen spelling is accepted.
        assert_eq!(
            HotkeyAction::from_config_key("brightness-up"),
            Some(HotkeyAction::BrightnessUp)
        );
        assert_eq!(HotkeyAction::from_config_key("nonsense"), None);
    }

    fn table(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn resolve_collects_valid_bindings_sorted_by_action() {
        let plan = resolve(&table(&[
            ("toggle_flyout", "Ctrl+Alt+B"),
            ("brightness_up", "Ctrl+Alt+Up"),
            ("brightness_down", "Ctrl+Alt+Down"),
        ]));
        assert!(plan.errors.is_empty());
        assert!(plan.conflicts.is_empty());
        let actions: Vec<HotkeyAction> = plan.bindings.iter().map(|b| b.action).collect();
        assert_eq!(
            actions,
            vec![
                HotkeyAction::BrightnessUp,
                HotkeyAction::BrightnessDown,
                HotkeyAction::ToggleFlyout,
            ]
        );
    }

    #[test]
    fn resolve_reports_unknown_action_and_bad_accelerator() {
        let plan = resolve(&table(&[
            ("brightness_up", "Ctrl+Alt+Up"),
            ("what_is_this", "Ctrl+Alt+X"),
            ("brightness_down", "Ctrl+Alt+"),
        ]));
        assert_eq!(plan.bindings.len(), 1);
        assert_eq!(plan.errors.len(), 2);
        assert!(
            plan.errors.iter().any(|e| {
                e.key == "what_is_this" && e.reason == BindingErrorKind::UnknownAction
            })
        );
        assert!(plan.errors.iter().any(|e| {
            matches!(e.reason, BindingErrorKind::BadAccelerator(_)) && e.key == "brightness_down"
        }));
    }

    #[test]
    fn resolve_detects_conflicts_naming_the_clashing_actions() {
        // Same combo (order/case-insensitively) bound to two actions.
        let plan = resolve(&table(&[
            ("brightness_up", "Ctrl+Alt+Up"),
            ("toggle_flyout", "alt+ctrl+up"),
        ]));
        assert_eq!(plan.conflicts.len(), 1);
        let conflict = plan.conflicts.first().expect("one conflict");
        assert_eq!(conflict.accel, accel("Ctrl+Alt+Up"));
        assert_eq!(
            conflict.actions,
            vec![HotkeyAction::BrightnessUp, HotkeyAction::ToggleFlyout]
        );
    }

    #[test]
    fn distinct_combos_do_not_conflict() {
        let plan = resolve(&table(&[
            ("brightness_up", "Ctrl+Alt+Up"),
            ("brightness_down", "Ctrl+Alt+Down"),
        ]));
        assert!(plan.conflicts.is_empty());
    }
}

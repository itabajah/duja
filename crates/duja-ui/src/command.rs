//! The command vocabulary a view-model emits back to the shell.
//!
//! A [`UiCommand`] is the **only** thing that crosses out of a view-model in
//! response to a user action: plain data, no Slint and no engine types. Wave 2
//! (`duja-app` assembly) maps each variant onto an
//! `EngineCommand` — `SetLevel` ⇒ `EngineCommand::SetUserLevel`, `Refresh` ⇒
//! `EngineCommand::RefreshNow` — so the UI never depends on the engine crate.

use duja_core::id::StableDisplayId;

/// A user-driven intent produced by a view-model, for the shell to forward.
///
/// The set is deliberately tiny: everything the P4 flyout can *do* to the
/// engine is either "set one display's level" or "re-enumerate now". State that
/// only affects presentation (link toggle, theme) never becomes a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    /// Set the unified user brightness level (0..=100) for one display.
    ///
    /// The percent is already clamped to `0..=100` by the emitting view-model.
    SetLevel {
        /// The display to adjust.
        id: StableDisplayId,
        /// Desired level in percent, guaranteed `0..=100`.
        pct: u8,
    },
    /// Run one enumeration pass immediately (the settings/refresh affordance).
    Refresh,
}

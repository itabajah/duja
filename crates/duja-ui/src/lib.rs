//! Slint UI components and their view-models.
//!
//! **Hard boundary (test architecture, plan §4.4):** all UI logic lives in
//! plain-Rust view-models — [`FlyoutVm`], [`SettingsVm`] — display snapshots
//! in, [`UiCommand`]s out, **zero Slint types in their signatures** — so the
//! logic is fully unit-testable. The `.slint` files and the [`FlyoutShell`] are
//! a thin rendering skin; Slint types never leak past the [`shell`] module.
//!
//! ## Module map
//!
//! - [`flyout_vm`] — the flyout view-model: ordered [`FlyoutRow`]s, link-all,
//!   theme, and the slider/refresh actions.
//! - [`settings_vm`] — the P4 settings-panel skeleton.
//! - [`command`] — [`UiCommand`], the only thing a view-model emits.
//! - [`shell`] — [`FlyoutShell`], the Slint-facing seam (the sole Slint island).
//!
//! ## Wave-2 wiring (`duja-app` assembly)
//!
//! Wave 2 owns an `Rc<RefCell<FlyoutVm>>`, maps engine notifications onto
//! `set_displays` / `set_unresponsive`, calls `FlyoutShell::update_from_vm`,
//! and maps each [`UiCommand`] onto an `EngineCommand`
//! (`SetLevel` ⇒ `SetUserLevel`, `Refresh` ⇒ `RefreshNow`). It also owns the
//! tray icon and the flyout positioning (`show_at` / `hide`). Every slider
//! change is forwarded — there is no UI-side throttle; the engine worker's
//! `write_min_gap` last-wins coalescer bounds the hardware write rate and
//! guarantees a drag's final value lands (P4 gate Finding 1).
//!
//! ## Idle budget
//!
//! Zero Slint timers/animations while the flyout is hidden (perf budget: zero
//! idle wakeups) — there are no `Timer` elements in the markup or this crate.

// RATIONALE: `deny`, not `forbid`, for unsafe. Slint's generated component code
// (`include_modules!` in `shell`) emits an internal `#[allow(unsafe_code)]` for
// its vtable statics; a crate-level `forbid` would reject that allow and make
// the codegen impossible to compile. `deny` still forbids unsafe in every
// hand-written module here — opting out would require an explicit local `allow`
// we never write and review would catch — so our code stays unsafe-free while
// the generated island is permitted.
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod command;
pub mod flyout_vm;
pub mod settings_vm;
pub mod shell;

pub use command::UiCommand;
pub use flyout_vm::{FlyoutRow, FlyoutVm, Theme};
pub use settings_vm::{SettingControl, SettingKey, SettingsRow, SettingsVm};
pub use shell::FlyoutShell;

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_against_core() {
        assert_eq!(version(), duja_core::version());
    }
}

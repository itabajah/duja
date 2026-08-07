//! The tray seam: everything `AppState` needs from a tray, with no library named.
//!
//! ADR-0010 asks for this before the Linux arm lands, and the reason is narrow.
//! `AppState` held a `tray_icon::TrayIcon`, a `tray_icon::menu::Menu` and a
//! `tray_icon::menu::MenuItem` as three separate fields, so every one of its
//! callers was written against `tray-icon`'s *imperative* menu model — build the
//! items up front, keep live handles, mutate them in place. `ksni` has no such
//! model: a `StatusNotifierItem` hands the host a menu **tree** rebuilt from a
//! callback whenever the host asks, so there is no handle to prepend to and no
//! item whose text can be set. A seam expressed in `tray-icon`'s verbs would have
//! forced the Linux backend to fake handles it does not have.
//!
//! So the seam is the three things `AppState` actually wants, phrased as
//! outcomes rather than as menu edits:
//!
//! - the icon should now be this accent colour,
//! - the tooltip should now say this (or nothing),
//! - an update is available, and the tray should say so.
//!
//! The last one folds a prepend-once, set-the-label pair into a single
//! **idempotent** call, because "once" is the part each backend does differently:
//! `tray-icon` must not prepend twice, `ksni` re-renders from state and cannot.
//! De-duplication *by version* stays in `AppState`, where it belongs — that is
//! policy about when to toast, not a property of any tray.
//!
//! `PlatformTray` is a concrete type per target rather than a `dyn` trait, which
//! mirrors `PlatformDimmer` in `duja-dimmer` for the same reason: exactly one
//! implementation is reachable in a given build, so a vtable would buy nothing
//! and cost a name that means "any of them" in a codebase where it never is.

use anyhow::Result;
use duja_ui::accent::AccentChoice;

use super::icon;

/// The label a tray must render when an update is available.
///
/// At module scope rather than inlined into [`PlatformTray::announce_update`],
/// because the ksni arm builds its menu from a callback and needs the identical
/// string: two format strings in two `cfg` arms is the shape that drifts.
///
/// Its test runs **wherever this module compiles** — the Windows and macOS lanes
/// today, and the ubuntu one once `bin_support`'s gate on `mod tray` widens to
/// include Linux. Not "on every lane", which an earlier version of this sentence
/// said: `mod tray` is `cfg(any(windows, target_os = "macos"))`, so on Linux this
/// file is not compiled at all and the ubuntu lane cannot contain the test. The
/// sibling `geometry.rs` states the same fact correctly, and getting it backwards
/// here would tell whoever lands the ksni arm that the shared label string is
/// already guarded on the lane that will first exercise it.
fn update_label(version: &str) -> String {
    format!("Update available — {version}")
}

/// The tray, as the rest of the app is allowed to see it.
///
/// No `cfg` guard: `bin_support::tray` is itself gated, so this file compiles
/// only where a tray is built. Repeating the gate here would be a second place to
/// update when the Linux arm lands, and the kind that is easy to miss.
pub(crate) struct PlatformTray {
    /// The tray icon itself, held so an accent change can swap its glyph live.
    icon: tray_icon::TrayIcon,
    /// A live handle to the menu — the same `Rc` inner the tray owns — so the
    /// update item can be prepended at runtime.
    menu: tray_icon::menu::Menu,
    /// The pre-built "Update available" item, held out of the menu until a check
    /// finds a newer release. Built up front so its id is stable and known to the
    /// event handler before it is ever shown.
    update_item: tray_icon::menu::MenuItem,
    /// Whether [`PlatformTray::announce_update`] has already put the item in the
    /// menu. The backend owns this rather than the caller, because "prepend
    /// exactly once" is a `tray-icon` obligation and not a fact about updates.
    update_shown: bool,
}

impl PlatformTray {
    /// Wrap the pieces `build_tray` produced.
    pub(super) const fn new(
        icon: tray_icon::TrayIcon,
        menu: tray_icon::menu::Menu,
        update_item: tray_icon::menu::MenuItem,
    ) -> Self {
        Self {
            icon,
            menu,
            update_item,
            update_shown: false,
        }
    }

    /// Repaint the tray glyph in `accent`'s colour.
    ///
    /// # Errors
    /// Returns a message if the icon buffer cannot be built or the platform
    /// refuses it. Both are cosmetic: the tray keeps the glyph it already has.
    pub(crate) fn set_accent(&mut self, accent: AccentChoice) -> Result<()> {
        let built = icon::tray_icon(duja_ui::accent::icon_rgb(accent))?;
        self.icon
            .set_icon(Some(built))
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Set (or clear) the tray tooltip.
    ///
    /// # Errors
    /// Returns a message if the platform refuses it.
    pub(crate) fn set_tooltip(&mut self, text: Option<&str>) -> Result<()> {
        self.icon
            .set_tooltip(text)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Say that `version` is available: label the update item and, the first time
    /// only, put it at the top of the menu above a separator.
    ///
    /// Idempotent by design — calling it again with a newer version relabels
    /// without prepending a second copy.
    ///
    /// # Errors
    /// Returns a message if the item cannot be added to the menu. The label is set
    /// first and unconditionally, so a failure here leaves the tray consistent
    /// with a tooltip that still announces the update.
    pub(crate) fn announce_update(&mut self, version: &str) -> Result<()> {
        self.update_item.set_text(update_label(version));
        if self.update_shown {
            return Ok(());
        }
        let separator = tray_icon::menu::PredefinedMenuItem::separator();
        self.menu
            .prepend_items(&[&self.update_item, &separator])
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        self.update_shown = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! `PlatformTray` itself cannot be constructed in a test on any lane — every
    //! backend needs a live desktop session — so what is pinned here is the seam's
    //! *shape*, which is the part a second implementation has to match.

    use super::update_label;

    #[test]
    fn the_update_label_carries_the_version_and_an_em_dash() {
        // The em dash is not decoration: the tray menu is the one surface where
        // this string is read next to "Open" and "Settings", and a hyphen there
        // reads as a different item rather than a qualified one. Pinned because
        // the second backend formats the same string from its own code path.
        assert_eq!(update_label("v0.1.5"), "Update available — v0.1.5");
        assert!(update_label("v2.0.0").ends_with("v2.0.0"));
    }
}

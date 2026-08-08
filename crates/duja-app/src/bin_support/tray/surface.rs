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
//! # One implementation per build, and the one exception
//!
//! The OS backend is a concrete type per target rather than a `dyn` trait, which
//! mirrors `PlatformDimmer` in `duja-dimmer` for the same reason: exactly one
//! implementation is reachable in a **shipping** build, so a vtable would buy
//! nothing and cost a name that means "any of them" in a codebase where it never
//! is.
//!
//! That sentence used to say "a given build" and it stopped being true here, so
//! it is amended rather than quietly outgrown. A **test** build reaches two:
//! [`OsTray`] and `FakeTray`, behind [`Backend`]. (`FakeTray` is unlinked on
//! purpose: it is `cfg(test)`, so rustdoc does not compile it and an intra-doc
//! link to it fails the `-D warnings` doc build.) `docs/debt.md` D-102
//! established why a fake is needed rather than optional - `build_tray`
//! *succeeds* in a test process on a live session, so an `AppState` test that
//! built a real tray would put a real Duja icon in the real notification area
//! and would answer differently per session, which on CI is the difference
//! between a gate and noise. Success created the need for a fake rather than
//! removing it.
//!
//! It is still not a `dyn` trait, and the reason survives the amendment intact:
//! the second implementation exists only where `cfg(test)` is on, so nothing
//! shipped ever dispatches, and the enum keeps every backend's methods
//! *inherent* - a trait would have to name the union of three signatures, two of
//! which are infallible for backend-specific reasons this module documents at
//! length.

use anyhow::Result;
use duja_ui::accent::AccentChoice;

#[cfg(not(target_os = "linux"))]
use super::icon;

/// The label a tray must render when an update is available.
///
/// At module scope rather than inlined into `announce_update`, because the ksni
/// arm builds its menu from a callback and needs the identical string: two format
/// strings in two `cfg` arms is the shape that drifts.
///
/// Its test now runs on **all three lanes**, which it did not when this function
/// was written — `bin_support`'s gate on `mod tray` was
/// `cfg(any(windows, target_os = "macos"))`, so the ubuntu lane did not compile
/// this file at all. P7 wave 5 removed that gate along with the reason for it,
/// and the earlier wording is worth keeping in mind rather than just deleting:
/// it was correct when written and became wrong without being touched, which is
/// what a claim about *where a test runs* does whenever a module gate moves.
pub(super) fn update_label(version: &str) -> String {
    format!("Update available — {version}")
}

/// The tray, as the rest of the app is allowed to see it.
///
/// One struct per target rather than one struct with `cfg` fields, because the
/// two backends share no state at all: `tray-icon` holds three live handles plus
/// a "have I prepended yet" flag, `ksni` holds one service handle and keeps the
/// rest in the value it moved onto its own thread.
#[cfg(not(target_os = "linux"))]
pub(crate) struct OsTray {
    /// The tray icon itself, held so an accent change can swap its glyph live.
    icon: tray_icon::TrayIcon,
    /// A live handle to the menu — the same `Rc` inner the tray owns — so the
    /// update item can be prepended at runtime.
    menu: tray_icon::menu::Menu,
    /// The pre-built "Update available" item, held out of the menu until a check
    /// finds a newer release. Built up front so its id is stable and known to the
    /// event handler before it is ever shown.
    update_item: tray_icon::menu::MenuItem,
    /// Whether [`Self::announce_update`] has already put the item in the menu.
    /// The backend owns this rather than the caller, because "prepend exactly
    /// once" is a `tray-icon` obligation and not a fact about updates.
    update_shown: bool,
}

#[cfg(not(target_os = "linux"))]
impl OsTray {
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

/// The tray on Linux: a `StatusNotifierItem`, described rather than mutated.
///
/// The methods are infallible where the other arm's are not, and that is the
/// backend's shape rather than a shortcut taken here. Every change goes through
/// `ksni::blocking::Handle::update`, whose only failure is that the service has
/// already stopped — at which point there is no tray for a `warn!` to be about,
/// and the caller would be logging about something the user cannot see. The
/// `Result` stays in the signature so `AppState` needs no `cfg`; what would be
/// wrong is inventing an error for it to carry.
#[cfg(target_os = "linux")]
pub(crate) struct OsTray {
    /// The running service.
    inner: super::ksni_tray::LinuxTray,
}

#[cfg(target_os = "linux")]
impl OsTray {
    /// Wrap a started service.
    pub(super) const fn new(inner: super::ksni_tray::LinuxTray) -> Self {
        Self { inner }
    }

    /// Repaint the tray glyph in `accent`'s colour.
    ///
    /// # Errors
    /// Never on this backend; see the type's own doc for why the signature keeps
    /// the `Result` anyway.
    #[allow(clippy::unnecessary_wraps)] // RATIONALE: seam parity, see above.
    pub(crate) fn set_accent(&mut self, accent: AccentChoice) -> Result<()> {
        self.inner.set_icon(duja_ui::accent::icon_rgb(accent));
        Ok(())
    }

    /// Set (or clear) the tray tooltip.
    ///
    /// A deliberate no-op. A `StatusNotifierItem`'s tooltip is *rendered from the
    /// item's state* on every host refresh, exactly as its menu is, so
    /// [`Self::announce_update`] already puts the update into it and any string
    /// written here would be overwritten at the host's next `ToolTip` call. An
    /// implementation that forwarded the text would therefore work until the host
    /// refreshed and then silently revert, which is worse than doing nothing.
    ///
    /// # Errors
    /// Never.
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)] // RATIONALE: seam parity.
    pub(crate) fn set_tooltip(&mut self, _text: Option<&str>) -> Result<()> {
        Ok(())
    }

    /// Say that `version` is available.
    ///
    /// Idempotent for free rather than by a flag: the menu and tooltip are
    /// projections of one `Option<String>`, so setting it twice renders the same
    /// tray. This is the asymmetry the seam exists for — the other arm needs
    /// `update_shown` to avoid prepending a second row.
    ///
    /// # Errors
    /// Never.
    #[allow(clippy::unnecessary_wraps)] // RATIONALE: seam parity.
    pub(crate) fn announce_update(&mut self, version: &str) -> Result<()> {
        self.inner.announce_update(version);
        Ok(())
    }
}

/// Which tray is behind the seam.
///
/// One variant in a shipping build; two under `cfg(test)`. See the module header
/// for why the second one exists and why this is an enum rather than a trait.
enum Backend {
    /// The real tray for this target.
    Os(OsTray),
    /// A stand-in that records what it was asked to do and touches no OS.
    #[cfg(test)]
    Fake(FakeTray),
}

/// The tray, as the rest of the app is allowed to see it.
///
/// The three verbs are the whole surface `AppState` touches. Everything about
/// *how* a tray achieves them - three live `tray-icon` handles and a
/// prepend-once flag on Windows and macOS, one `ksni` service handle on Linux,
/// three `Vec`s in a test - stays behind this type.
pub(crate) struct PlatformTray {
    /// The tray this seam is standing in front of.
    backend: Backend,
}

impl PlatformTray {
    /// Repaint the tray glyph in `accent`'s colour.
    ///
    /// # Errors
    /// Whatever the backend returns. On every backend a failure here is
    /// cosmetic: the tray keeps the glyph it already has.
    pub(crate) fn set_accent(&mut self, accent: AccentChoice) -> Result<()> {
        match &mut self.backend {
            Backend::Os(os) => os.set_accent(accent),
            #[cfg(test)]
            Backend::Fake(fake) => {
                fake.accents.push(accent);
                Ok(())
            }
        }
    }

    /// Set (or clear) the tray tooltip.
    ///
    /// # Errors
    /// Whatever the backend returns; the Linux arm never fails and documents
    /// why at [`OsTray::set_tooltip`].
    pub(crate) fn set_tooltip(&mut self, text: Option<&str>) -> Result<()> {
        match &mut self.backend {
            Backend::Os(os) => os.set_tooltip(text),
            #[cfg(test)]
            Backend::Fake(fake) => {
                fake.tooltips.push(text.map(str::to_owned));
                Ok(())
            }
        }
    }

    /// Say that `version` is available, idempotently.
    ///
    /// # Errors
    /// Whatever the backend returns.
    pub(crate) fn announce_update(&mut self, version: &str) -> Result<()> {
        match &mut self.backend {
            Backend::Os(os) => os.announce_update(version),
            #[cfg(test)]
            Backend::Fake(fake) => {
                fake.updates.push(version.to_owned());
                Ok(())
            }
        }
    }
}

/// Wrap the real tray this target built.
impl From<OsTray> for PlatformTray {
    fn from(os: OsTray) -> Self {
        PlatformTray {
            backend: Backend::Os(os),
        }
    }
}

/// A tray that records rather than renders.
///
/// It exists so [`super::state::AppState`] can be built in a test at all, which
/// is the sentence four debt rows deferred on. It records rather than merely
/// accepting, because a tray that silently swallowed every verb would let a
/// caller stop calling one and leave every test green - which is the failure
/// mode those rows are about in the first place.
#[cfg(test)]
#[derive(Default)]
pub(super) struct FakeTray {
    /// Every accent this tray was asked to paint, oldest first.
    accents: Vec<AccentChoice>,
    /// Every tooltip it was asked to show, oldest first; `None` is a clear.
    tooltips: Vec<Option<String>>,
    /// Every version it was asked to announce, oldest first. Not de-duplicated:
    /// `announce_update` is idempotent as an *outcome*, and a test that wants to
    /// prove the caller de-duplicates needs to see the calls it actually made.
    updates: Vec<String>,
}

#[cfg(test)]
impl PlatformTray {
    /// A tray that records and touches no OS.
    pub(super) fn fake() -> Self {
        PlatformTray {
            backend: Backend::Fake(FakeTray::default()),
        }
    }

    /// What the fake was asked to do, in order: accents, tooltips, versions.
    ///
    /// # Panics
    /// If this is not a fake. A test that reaches for the recording of a real
    /// tray has confused its fixture for the thing it stands in for, and a
    /// silent empty answer would read as "nothing was called".
    pub(super) fn recorded(&self) -> (&[AccentChoice], &[Option<String>], &[String]) {
        match &self.backend {
            Backend::Fake(fake) => (&fake.accents, &fake.tooltips, &fake.updates),
            Backend::Os(_) => panic!("recorded() on a real tray"),
        }
    }
}

#[cfg(test)]
mod tests {
    //! What is pinned here is the seam's *shape*, which is the part a second
    //! implementation has to match.
    //!
    //! This used to say `PlatformTray` "cannot be constructed in a test on any
    //! lane — every backend needs a live desktop session". The first half is
    //! now known false: D-102's experiment (`wiring.rs`) builds one in a test
    //! process and drives all three verbs. The second half is what is actually
    //! true, and it is why the tests here are still shape-only — the constructor
    //! needs a live *session*, not merely a non-test process, so a test that
    //! built one would pass on a developer's desktop and answer differently on
    //! CI. A fake is the way out, and D-102 carries that.

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

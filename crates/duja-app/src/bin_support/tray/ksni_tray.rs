//! The Linux tray: a freedesktop `StatusNotifierItem` over D-Bus, via `ksni`.
//!
//! [ADR-0010] chose `ksni` over `tray-icon`'s Linux backend because that backend
//! needs a GTK event loop and Duja's main thread runs Slint's winit loop. What
//! that buys and costs is recorded there; what matters here is the shape it
//! imposes, which is not the shape the other two platforms have.
//!
//! # A tray you describe, not a tray you mutate
//!
//! `tray-icon` hands back live handles and you mutate them. `ksni` inverts it:
//! you give it a value implementing [`ksni::Tray`], it moves that value onto its
//! own thread, and whenever the host asks what the item looks like it calls the
//! methods again. So there is no menu handle to prepend to — [`DujaTray`]'s
//! `menu` renders the whole menu from `self` every time, and "the update item appears"
//! is a *field* becoming `Some`, not a call that adds a row.
//!
//! Changes therefore go through [`ksni::blocking::Handle::update`], which takes a
//! closure, applies it to the value on ksni's thread, and re-renders. That is why
//! [`super::surface::PlatformTray`] exists at all: three imperative `tray-icon`
//! handles and one declarative ksni value cannot be the same seam.
//!
//! # Threads
//!
//! The value lives on ksni's thread and every method here runs there, so nothing
//! in this file may touch `AppState` or Slint directly. Menu activations go
//! through [`super::wiring::dispatch`], which is `slint::invoke_from_event_loop`
//! — `Send`, and the same hop the Windows and macOS handlers already make from
//! their own callback threads. The difference is only that theirs is a library
//! callback and this is a D-Bus method call.
//!
//! [ADR-0010]: https://github.com/itabajah/duja/blob/main/docs/adr/0010-linux-tray-ksni.md

use ksni::blocking::TrayMethods as _;

use super::Action;
use super::linux_icon::{ICON_SIZE, rgba_to_argb32};
use super::wiring::dispatch;

/// Build the accent glyph in the layout the spec wants.
fn build_icon(rgb: [u8; 3]) -> ksni::Icon {
    let rgba = duja_ui::icon::monitor_rgba(ICON_SIZE, rgb);
    ksni::Icon {
        // RATIONALE (cast_possible_wrap): `ICON_SIZE` is a literal 32, so the
        // `i32` the spec's struct asks for is exact and the cast cannot wrap.
        #[allow(clippy::cast_possible_wrap)]
        width: ICON_SIZE as i32,
        #[allow(clippy::cast_possible_wrap)]
        height: ICON_SIZE as i32,
        data: rgba_to_argb32(&rgba),
    }
}

/// The tray item's state, as ksni asks for it.
///
/// Every field is something a [`ksni::Tray`] method reads. There is deliberately
/// no handle back to the app: this value lives on ksni's thread.
struct DujaTray {
    /// The accent glyph, already in the spec's ARGB32 layout.
    icon: ksni::Icon,
    /// The version an update check found, if any. `Some` is what puts the
    /// "Update available" row at the top of the menu and fills the tooltip.
    update: Option<String>,
}

impl ksni::Tray for DujaTray {
    fn id(&self) -> String {
        // The spec asks for something unique to the application and stable across
        // sessions; the crate name is exactly that.
        env!("CARGO_PKG_NAME").to_owned()
    }

    fn title(&self) -> String {
        "Duja".to_owned()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Duja".to_owned(),
            description: match &self.update {
                Some(version) => super::surface::update_label(version),
                None => String::new(),
            },
            ..ksni::ToolTip::default()
        }
    }

    /// A left click toggles the flyout, which is what the tray does on the other
    /// two platforms.
    ///
    /// `x` and `y` are the spec's hint about where a window should go, in screen
    /// coordinates. They are the **only** cursor position a Wayland session will
    /// ever hand Duja — there is no global pointer query there — so this is the
    /// anchor source the Wayland half of `cursor_anchor` was always going to need.
    /// Threading them into placement is its own change and `docs/debt.md` carries
    /// the row; today the flyout opens the way a hotkey opens it.
    fn activate(&mut self, _x: i32, _y: i32) {
        dispatch(Action::Toggle);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};

        /// One row that dispatches `action` and reads nothing from `self`.
        fn row(label: &str, action: Action) -> MenuItem<DujaTray> {
            StandardItem {
                label: label.to_owned(),
                activate: Box::new(move |_: &mut DujaTray| dispatch(action)),
                ..StandardItem::default()
            }
            .into()
        }

        let mut items = Vec::new();
        // Rendered from `self.update` rather than appended once, because ksni
        // calls this again on every host refresh. The "prepend exactly once" the
        // other backends need has no counterpart here and must not be emulated:
        // doing so would add a row per refresh.
        if let Some(version) = &self.update {
            items.push(row(
                &super::surface::update_label(version),
                Action::OpenReleases,
            ));
            items.push(MenuItem::Separator);
        }
        items.push(row("Open", Action::Open));
        items.push(row("Settings", Action::OpenSettings));
        items.push(row("Restore screen", Action::Restore));
        items.push(MenuItem::Separator);
        items.push(row("Restart", Action::Restart));
        items.push(row("Quit", Action::Quit));
        items
    }
}

/// A running Linux tray.
pub(super) struct LinuxTray {
    /// The handle ksni gives back; every change goes through its `update`.
    handle: ksni::blocking::Handle<DujaTray>,
}

impl LinuxTray {
    /// Start the tray service and return a handle to it.
    ///
    /// # Errors
    /// Returns a message if the `StatusNotifierItem` cannot be registered — no
    /// session bus, or no `StatusNotifierWatcher` to register with. That is fatal
    /// to the tray in the same way a failed `TrayIconBuilder::build` is on
    /// Windows, and `build_tray`'s caller treats it identically.
    pub(super) fn start(rgb: [u8; 3]) -> anyhow::Result<Self> {
        let tray = DujaTray {
            icon: build_icon(rgb),
            update: None,
        };
        let handle = tray
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to register the status notifier item: {e}"))?;
        Ok(Self { handle })
    }

    /// Repaint the glyph in `rgb`.
    pub(super) fn set_icon(&self, rgb: [u8; 3]) {
        // `update` answers `None` once the service has stopped, which is not worth
        // surfacing for a colour change: there is no tray left for the message to
        // be about. The other arm's equivalent can fail for reasons that are still
        // actionable, which is why the seam keeps a `Result` and this does not.
        let _ = self.handle.update(|tray| tray.icon = build_icon(rgb));
    }

    /// Record that `version` is available, so the menu and tooltip render it.
    pub(super) fn announce_update(&self, version: &str) {
        let version = version.to_owned();
        let _ = self.handle.update(move |tray| tray.update = Some(version));
    }
}

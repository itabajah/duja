//! Internal laptop-panel brightness control.
//!
//! DDC/CI cannot reach internal panels; each OS has a distinct native API:
//! Windows `WmiMonitorBrightnessMethods` (`root\wmi`), macOS private
//! `DisplayServicesSetBrightness` (dlopen'd, graceful fallback), Linux logind
//! D-Bus `SetBrightness` with a `/sys/class/backlight` write fallback.
//!
//! # Architecture
//!
//! A [`PanelTransport`] is the minimal, OS-specific brightness primitive (query
//! current + levels, set brightness). [`PanelController`] adapts any transport
//! to `duja_core`'s [`BrightnessController`](duja_core::controller::BrightnessController)
//! trait, applying the panel semantics (brightness-only, percent-domain,
//! clamp-on-overrange). This split keeps the `unsafe` COM code confined to the
//! Windows `wmi` module while the whole adapter is exercised cross-platform by
//! `duja_core`'s controller contract suite against a fake transport.
//!
//! # Enumeration and graceful absence
//!
//! [`enumerate`] lists the internal panels that expose brightness control. On a
//! machine with **no** internal panel — every desktop — it returns
//! `Ok(vec![])`, never an error: the absence of the WMI class or of any panel
//! instance is the expected state, not a failure. Only a genuine backend fault
//! on a machine that *does* have a panel surfaces as [`PanelError`].
//!
//! This crate has a Windows backend (`wmi`) and a macOS backend
//! (`display_services`); on any other target [`enumerate`] is a no-op returning
//! an empty list, so the workspace still builds and tests everywhere. The pure
//! adapter logic — the transport seam, the float/level and identity mapping — is
//! platform-independent and exercised by the controller contract on every OS.
//!
//! # Geometry, for dimming below the backlight's floor
//!
//! A panel's backlight has a floor, and below it the only way down is *software*
//! dimming — an overlay over the panel's rectangle, or a gamma ramp on it. Both
//! need to know where the panel is, so [`enumerate`] reports a
//! [`PanelGeometry`] alongside each panel wherever the backend can produce one
//! (macOS today; never on Windows, where WMI exposes no rectangle). This is the
//! crate's job rather than the caller's on purpose: it keeps display-geometry FFI
//! out of binaries that exist to have none, and stops anyone re-deriving it from
//! [`PanelDisplay::instance_name`], which is documented opaque.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod controller;
mod display_services;
mod error;
mod transport;

#[cfg(windows)]
pub mod wmi;

pub use controller::PanelController;
pub use error::PanelError;
pub use transport::{PanelBrightness, PanelTransport};

#[cfg(target_os = "macos")]
pub use display_services::{DisplayServicesApi, DisplayServicesTransport, RealDisplayServices};

use duja_core::dimmer::DisplayBounds;
use duja_core::id::StableDisplayId;

/// Where a panel sits on the desktop, and the tokens the **software**-dimming
/// channels address it by — everything a caller needs to dim a panel below the
/// floor its backlight can reach.
///
/// Reported by the backends that can report it, which today means macOS alone:
///
/// - **macOS** — present whenever CoreGraphics gives a usable rectangle, which is
///   every ordinary case: `DisplayServices` panels are CoreGraphics displays, so
///   `CGDisplayBounds` and `CGDisplayMirrorsDisplay` answer for the built-in
///   screen exactly as they do for an external monitor. It is withheld for a rect
///   that is not finite or encloses no area — `CGRectNull`, which CoreGraphics
///   returns for a display it considers invalid, is both — because such a
///   rectangle is not a position and an overlay drawn from it would dim nothing.
///   See `display_services`' `panel_geometry` for the two arms and why neither is
///   a threshold.
/// - **Windows** — always absent. WMI's `WmiMonitorBrightnessMethods` exposes no
///   monitor rectangle and no GDI device for the panel it controls, so there is
///   nothing honest to put here; a Windows laptop panel that needs software
///   dimming reaches it through the DDC fallback carrier instead (see
///   `duja-app`'s `backend::merge_displays`), and `docs/debt.md` tracks the
///   pure-WMI residue.
///
/// Absence therefore means "this backend cannot say", never "this panel has no
/// position" — a caller must treat `None` as *unknown* and simply not plan
/// software dimming for that panel, which is what `duja-app`'s planner does.
///
/// # The two tokens are not interchangeable
///
/// [`Self::gamma_token`] **addresses** this one display; [`Self::surface_token`]
/// names the **framebuffer** it draws from, which every member of a mirror set
/// shares and which is therefore the key a mirror merge buckets on. They are the
/// same string for a standalone display and differ for a mirror clone. Driving a
/// per-display call through the surface token would act on a *different* display
/// — possibly one the caller never enumerated. [`duja_core::macos`] holds the
/// rule and the reasoning; `duja-ddc` reports the same pair for external
/// monitors, from that same function, which is what lets a mirror set spanning
/// both backends collapse into one control.
///
/// Both are opaque: compare them, pass them to the matching channel, and never
/// show one to a user or parse it as a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelGeometry {
    bounds: DisplayBounds,
    gamma_token: String,
    surface_token: String,
}

impl PanelGeometry {
    /// Build a geometry from its parts. Backend-agnostic: each backend produces
    /// the tokens in whatever way its platform defines them.
    #[must_use]
    pub fn new(bounds: DisplayBounds, gamma_token: String, surface_token: String) -> Self {
        PanelGeometry {
            bounds,
            gamma_token,
            surface_token,
        }
    }

    /// The panel's bounds in the OS's global display space — **points** on macOS,
    /// per [`DisplayBounds`]' own per-platform note.
    #[must_use]
    pub fn bounds(&self) -> DisplayBounds {
        self.bounds
    }

    /// The token that **addresses** this panel for gamma: its own
    /// `CGDirectDisplayID` in decimal on macOS. See the type docs.
    #[must_use]
    pub fn gamma_token(&self) -> &str {
        &self.gamma_token
    }

    /// The token that names this panel's **framebuffer**, which mirrored displays
    /// are grouped by: the mirror-set master's `CGDirectDisplayID` in decimal on
    /// macOS, or the panel's own when it mirrors nothing. See the type docs.
    #[must_use]
    pub fn surface_token(&self) -> &str {
        &self.surface_token
    }
}

/// An internal panel discovered by [`enumerate`], carrying its durable identity,
/// a human-readable name, enough OS handle to open a controller for it, and — on
/// a backend that can report it — its [`PanelGeometry`].
///
/// `instance_name` is the OS handle `open` binds a transport to: on Windows the
/// WMI `InstanceName` that keys every `WmiMonitor*` class for this panel, on
/// macOS the panel's `CGDirectDisplayID` rendered in decimal. It is kept as a
/// `String` so the public type is uniform across backends.
///
/// On macOS `instance_name` and [`PanelGeometry::gamma_token`] happen to carry
/// the same digits. They are still separate values, because they are separate
/// contracts: `instance_name` is the transport handle and is documented opaque,
/// while the geometry's tokens belong to the dimming channels. A consumer that
/// re-parsed `instance_name` to reach CoreGraphics would be reading a value it
/// was told not to interpret — and would put display-geometry FFI in a binary
/// that exists to have none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelDisplay {
    id: StableDisplayId,
    name: String,
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    // RATIONALE: `instance_name` keys the transport in `open()`, which only
    // exists on Windows and macOS; on other targets the field is retained for a
    // uniform public type but is unused.
    instance_name: String,
    geometry: Option<PanelGeometry>,
}

impl PanelDisplay {
    /// The panel's durable, EDID-derived identity.
    #[must_use]
    pub fn id(&self) -> &StableDisplayId {
        &self.id
    }

    /// A human-readable name for the panel (falls back to a generic label when
    /// the panel exposes no friendly name).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The OS handle that identifies this panel: the WMI `InstanceName` on
    /// Windows, the decimal `CGDirectDisplayID` on macOS.
    #[must_use]
    pub fn instance_name(&self) -> &str {
        &self.instance_name
    }

    /// The panel's [`PanelGeometry`], or `None` on a backend that cannot report
    /// one (every Windows/WMI panel — see the type docs, which explain why `None`
    /// means "unknown", not "nowhere").
    ///
    /// Captured at [`enumerate`] time rather than queried on demand: the values
    /// belong to the same snapshot as `instance_name`, and a display id is a
    /// volatile session handle that a hot-plug can retire, so a later lookup
    /// could answer for a display that is no longer the one enumerated here.
    #[must_use]
    pub fn geometry(&self) -> Option<&PanelGeometry> {
        self.geometry.as_ref()
    }

    /// Open a brightness controller bound to this panel.
    ///
    /// Constructs a fresh WMI transport (and COM apartment) on the calling
    /// thread; see [`wmi::WmiTransport`] for the threading contract.
    ///
    /// # Errors
    /// [`PanelError`] if the COM apartment or WMI connection cannot be
    /// established.
    #[cfg(windows)]
    pub fn open(&self) -> Result<PanelController<wmi::WmiTransport>, PanelError> {
        let transport = wmi::WmiTransport::open(self.instance_name.clone())?;
        Ok(PanelController::new(transport))
    }

    /// Open a brightness controller bound to this panel.
    ///
    /// Parses the `CGDirectDisplayID` back out of `instance_name` and binds a
    /// [`DisplayServicesTransport`] over the resolved private framework.
    ///
    /// # Errors
    /// [`PanelError`] if `instance_name` is not a `CGDirectDisplayID` (it always
    /// is for a value from [`enumerate`]) or the private framework can no longer
    /// be resolved.
    #[cfg(target_os = "macos")]
    pub fn open(
        &self,
    ) -> Result<PanelController<DisplayServicesTransport<RealDisplayServices>>, PanelError> {
        let display: display_services::CgDisplayId = self
            .instance_name
            .parse()
            .map_err(|_| PanelError::Malformed("panel instance name is not a CGDirectDisplayID"))?;
        let api = RealDisplayServices::resolve().ok_or(PanelError::DisplayServices {
            context: "resolve DisplayServices framework",
            code: 0,
        })?;
        Ok(PanelController::new(DisplayServicesTransport::new(
            display, api,
        )))
    }
}

/// Enumerate the internal panels that expose brightness control.
///
/// Returns `Ok(vec![])` when there is no internal panel (the desktop case); see
/// the [crate docs](crate) on graceful absence.
///
/// # Errors
/// [`PanelError`] only on a genuine backend fault (a COM/WMI failure on a
/// machine that has the WMI infrastructure). A missing class or an empty
/// instance set is **not** an error.
#[cfg(windows)]
pub fn enumerate() -> Result<Vec<PanelDisplay>, PanelError> {
    wmi::enumerate()
}

/// Enumerate the internal panels that expose brightness control.
///
/// Returns `Ok(vec![])` when the private `DisplayServices` framework is
/// unavailable or no builtin panel reports brightness control; see the
/// [crate docs](crate) on graceful absence.
///
/// # Errors
/// Never errors: every absence is modelled as an empty list.
#[cfg(target_os = "macos")]
pub fn enumerate() -> Result<Vec<PanelDisplay>, PanelError> {
    Ok(display_services::enumerate())
}

/// Enumerate the internal panels that expose brightness control.
///
/// On targets without a panel backend (non-Windows, non-macOS) this is a no-op,
/// so the list is always empty. See the Windows and macOS variants for the real
/// behaviour.
///
/// # Errors
/// Never errors on these targets.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn enumerate() -> Result<Vec<PanelDisplay>, PanelError> {
    Ok(Vec::new())
}

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

    #[test]
    fn enumerate_on_this_machine_does_not_error() {
        // Enumerate must return Ok and never panic on any machine. On Windows and
        // macOS a host *with* an internal panel legitimately returns a non-empty
        // list, so we assert only success there; a virtualized macOS CI runner
        // may or may not report a builtin display, which is exactly why this
        // asserts Ok, not emptiness (see the P6 brief).
        let panels = enumerate().expect("enumerate must not error on this machine");
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(panels.is_empty());
        let _ = panels;
    }

    #[test]
    fn panel_display_accessors() {
        let display = PanelDisplay {
            id: StableDisplayId::from_parts("GSM", 0x5B09, Some("PANEL1")).unwrap(),
            name: "Internal Display".to_owned(),
            instance_name: r"DISPLAY\GSM5B09\4&abcd&0&UID0".to_owned(),
            geometry: None,
        };
        assert_eq!(display.id().as_str(), "GSM-5B09-PANEL1");
        assert_eq!(display.name(), "Internal Display");
        assert!(display.instance_name().contains("GSM5B09"));
        // A WMI panel reports no geometry: "this backend cannot say".
        assert!(display.geometry().is_none());
    }

    /// The accessors hand back what was put in, each from its own field. Written
    /// with three *different* values because the failure this guards is a
    /// same-typed mix-up — two `String` tokens beside each other — which a fixture
    /// that reused one string could not see.
    #[test]
    fn panel_geometry_accessors_keep_the_two_tokens_apart() {
        let geometry = PanelGeometry::new(
            DisplayBounds::new(-1920, 12, 1512, 982),
            "9".to_owned(),
            "4".to_owned(),
        );
        assert_eq!(geometry.bounds(), DisplayBounds::new(-1920, 12, 1512, 982));
        assert_eq!(geometry.gamma_token(), "9");
        assert_eq!(geometry.surface_token(), "4");
    }
}

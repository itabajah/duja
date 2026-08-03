//! Everything in this crate that talks to a Linux display server.
//!
//! The split is ADR-0011's, and it is the same one in every submodule here: the
//! rules are pure, run on every CI lane, and name no `wayland-client` or `x11rb`
//! type; these modules connect, fetch what a rule cannot fetch for itself, and
//! carry the answer back to the wire.
//!
//! | pure rule | evidence / effect |
//! |---|---|
//! | [`crate::linux_caps`] | [`x11`], [`wayland`] — two booleans and a list of interface names |
//! | [`crate::linux_outputs`] | [`outputs`] — each output's name, EDID and rectangle |
//! | [`crate::linux_overlay`] | [`overlay`] — the override-redirect ARGB windows |
//! | [`crate::linux_gamma`] | [`gamma`] — the `RandR` CRTC transfer tables |
//!
//! Nothing here can run in CI — a GitHub runner has no X server and no
//! compositor — which is precisely why each of these is as small as it is. Every
//! decision the feature makes is on the other side of the boundary.

mod gamma;
mod outputs;
mod overlay;
mod wayland;
mod x11;

pub use gamma::{
    GammaDisplay, display_supports_gamma, enumerate_gamma_displays, is_hdr_active, restore_all,
    restore_identity, set_gamma,
};
pub use outputs::enumerate_outputs;
pub use overlay::X11Dimmer;

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};

use crate::linux_caps::{Probe, SessionEnv, SurfaceCaps, Transport, resolve, transport};

/// The [`Dimmer`] for a Linux session, chosen at **runtime**.
///
/// Windows and macOS each have one windowing system, so their `PlatformDimmer` is
/// a type alias. Linux does not: whether an overlay is possible, and by what
/// mechanism, is a property of the session rather than the build. So this is a
/// real type that picks when it starts, which is the same answer ADR-0011 gives
/// for the capability report and for the same reason.
#[derive(Debug)]
pub struct LinuxDimmer {
    inner: Box<dyn Dimmer>,
}

impl LinuxDimmer {
    /// Start the backend this session can actually use.
    ///
    /// # Errors
    /// [`DimmerError::Unsupported`] when the session has no overlay mechanism —
    /// no display server, or a Wayland compositor (whose layer-shell backend
    /// lands in the next wave). [`DimmerError::Os`] for a session that should
    /// have worked and did not, which the caller logs before disabling software
    /// dimming.
    ///
    /// The caller treats both the same way (no dimmer, hardware control intact);
    /// they are distinguished because one is a fault worth a log line naming the
    /// cause and the other is an ordinary session.
    pub fn spawn() -> Result<Self, DimmerError> {
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let display = std::env::var("DISPLAY").ok();
        let env = SessionEnv {
            wayland_display: wayland_display.as_deref(),
            display: display.as_deref(),
        };
        match transport(env) {
            Transport::X11 => X11Dimmer::spawn().map(|dimmer| LinuxDimmer {
                inner: Box::new(dimmer),
            }),
            Transport::Wayland | Transport::None => Err(DimmerError::Unsupported),
        }
    }
}

impl Dimmer for LinuxDimmer {
    fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
        self.inner.apply(commands)
    }

    fn clear(&mut self) -> Result<(), DimmerError> {
        self.inner.clear()
    }
}

/// Resolve what this session can actually do.
///
/// Reads `WAYLAND_DISPLAY` and `DISPLAY`, connects to whichever the rule selects,
/// and returns the report. Never fails: every way this can go wrong is a
/// capability the report already has a reason for.
///
/// Only the **selected** transport is connected to. A Wayland session almost
/// always also has an Xwayland `DISPLAY`, and probing that too would produce a
/// second, contradictory answer for a screen the compositor already owns.
#[must_use]
pub fn probe_session() -> SurfaceCaps {
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    let env = SessionEnv {
        wayland_display: wayland_display.as_deref(),
        display: display.as_deref(),
    };
    match transport(env) {
        Transport::Wayland => {
            let (connected, interfaces) = wayland::probe();
            let borrowed: Vec<&str> = interfaces.iter().map(String::as_str).collect();
            resolve(
                env,
                &Probe {
                    connected,
                    globals: &borrowed,
                    randr: false,
                    // Not asked, and not consulted: a Wayland compositor *is* the
                    // compositing manager, so the rule's Wayland arm ignores this
                    // field. `false` is the honest value for a question that was
                    // never put to a server.
                    compositor: false,
                },
            )
        }
        Transport::X11 => {
            let answered = x11::probe();
            resolve(
                env,
                &Probe {
                    connected: answered.connected,
                    globals: &[],
                    randr: answered.randr,
                    compositor: answered.compositor,
                },
            )
        }
        // No display server: nothing to connect to, and the rule already says so.
        Transport::None => resolve(env, &Probe::default()),
    }
}

//! Gathering the evidence ADR-0011's capability rule decides on.
//!
//! [`crate::linux_caps`] is the rule and this is the only thing that talks to a
//! display server. The split is the ADR's: the rule is pure, runs on every CI
//! lane, and names no `wayland-client` or `x11rb` type; this module connects,
//! reads two booleans and a list of interface names, and hands them over as
//! plain data.
//!
//! Nothing here can run in CI — a GitHub runner has no X server and no
//! compositor — which is precisely why it is this small. Every decision the
//! feature makes is on the other side of the boundary.

mod wayland;
mod x11;

use crate::linux_caps::{Probe, SessionEnv, SurfaceCaps, Transport, resolve, transport};

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
            let (connected, randr, compositor) = x11::probe();
            resolve(
                env,
                &Probe {
                    connected,
                    globals: &[],
                    randr,
                    compositor,
                },
            )
        }
        // No display server: nothing to connect to, and the rule already says so.
        Transport::None => resolve(env, &Probe::default()),
    }
}

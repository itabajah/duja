//! The X11 half of ADR-0011's evidence: does the connection work, does the
//! server answer for `RandR`, and is a compositing manager running.
//!
//! Deliberately tiny. Everything that *decides* anything lives in
//! [`crate::linux_caps`], which is pure and runs on every CI lane; this module
//! only fetches the answers that module cannot fetch for itself. The connection
//! is opened and dropped inside one call, because the probe runs at startup and
//! on session change and holding an X connection open between them would be a
//! file descriptor and a wakeup source for no benefit.

use x11rb::connection::RequestConnection;
use x11rb::protocol::randr;
use x11rb::protocol::xproto::ConnectionExt as _;

/// What the X server answered.
///
/// Named fields rather than a tuple because all three are `bool`: transposing two
/// positionally is a silent wrong answer, and it is one no CI lane can catch.
pub(super) struct X11Probe {
    /// Whether the connection opened at all.
    pub(super) connected: bool,
    /// Whether the server offers the `RandR` extension.
    pub(super) randr: bool,
    /// Whether a compositing manager owns `_NET_WM_CM_S<n>`.
    pub(super) compositor: bool,
}

impl X11Probe {
    /// The answer for a server that could not be reached: nothing was asked, so
    /// nothing is claimed. [`crate::linux_caps::resolve`] already reports
    /// `ConnectFailed` for both mechanisms and never reads the other two.
    const fn unreachable() -> Self {
        X11Probe {
            connected: false,
            randr: false,
            compositor: false,
        }
    }
}

/// Connect to the X server named by `DISPLAY` and report what it offers.
///
/// A failed connect is [`X11Probe::unreachable`] rather than an error:
/// [`crate::linux_caps::resolve`] already has a reason for it (`ConnectFailed`),
/// and a second error type here would only be mapped back onto that one.
///
/// The `RandR` question is asked with `QueryExtension`, which is the protocol's
/// own answer and needs no version negotiation — Duja only needs to know the
/// extension is *there*, because per-CRTC gamma has been in `RandR` since 1.2 and
/// any server new enough to have the extension at all has it.
pub(super) fn probe() -> X11Probe {
    let Ok((connection, screen)) = x11rb::connect(None) else {
        return X11Probe::unreachable();
    };
    // `extension_information` returns `Ok(None)` for "the server does not have
    // it" and `Err` for "the request itself failed", and those are the same
    // answer here: either way there is no RandR to drive gamma with.
    let randr = connection
        .extension_information(randr::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some();
    X11Probe {
        connected: true,
        randr,
        compositor: compositor_running(&connection, screen),
    }
}

/// Whether a compositing manager owns `_NET_WM_CM_S<screen>`.
///
/// This is what decides whether an X11 overlay can dim at all rather than black
/// the screen out: X draws a window's colour bytes at full coverage and ignores
/// its alpha, so only a compositor blending the redirected pixmap makes 20% look
/// different from 100%. [`crate::linux_caps`] carries the full argument, and owns
/// the atom name so that at least that much of this is testable.
///
/// The screen number comes from the connection. It is the *X screen* — a separate
/// root window, as in `DISPLAY=:0.1` — not a monitor, and while it is 0 in almost
/// every session, hard-coding it would silently answer for the wrong root in the
/// sessions where it is not.
///
/// Every failure answers "no compositor". A request that errors outright is a
/// connection Duja could not place a window on either, and `intern_atom` with
/// `only_if_exists` short-circuits a server where the atom has never been created
/// at all. That last case is rarer than it sounds — atoms live as long as the
/// server, and GTK and Qt both intern this one at startup for their own
/// is-composited checks — so the owner query below is the answer that actually
/// fires. It cannot go stale either: the X server clears a selection when its
/// owner disconnects, so a compositor that exited leaves the atom interned and
/// unowned.
fn compositor_running(connection: &impl RequestConnection, screen: usize) -> bool {
    let selection = crate::linux_caps::compositor_selection(screen);
    let Ok(cookie) = connection.intern_atom(true, selection.as_bytes()) else {
        return false;
    };
    let Ok(reply) = cookie.reply() else {
        return false;
    };
    if reply.atom == x11rb::NONE {
        return false;
    }
    connection
        .get_selection_owner(reply.atom)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|owner| owner.owner != x11rb::NONE)
}

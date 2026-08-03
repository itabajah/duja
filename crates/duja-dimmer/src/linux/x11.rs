//! The X11 half of ADR-0011's evidence: does the connection work, does the
//! server answer for `RandR`, and is a compositing manager running.
//!
//! Deliberately tiny. Everything that *decides* anything lives in
//! [`crate::linux_caps`], which is pure and runs on every CI lane; this module
//! only fetches the booleans that module cannot fetch for itself. The
//! connection is opened and dropped inside one call, because the probe runs at
//! startup and on session change and holding an X connection open between them
//! would be a file descriptor and a wakeup source for no benefit.

use x11rb::connection::RequestConnection;
use x11rb::protocol::randr;
use x11rb::protocol::xproto::ConnectionExt as _;

/// Connect to the X server named by `DISPLAY` and report what it offers.
///
/// Returns `(connected, randr, compositor)`. A failed connect is
/// `(false, false, false)` rather than an error: [`crate::linux_caps::resolve`]
/// already has a reason for it (`ConnectFailed`), and a second error type here
/// would only be mapped back onto that one.
///
/// The `RandR` question is asked with `QueryExtension`, which is the protocol's
/// own answer and needs no version negotiation — Duja only needs to know the
/// extension is *there*, because per-CRTC gamma has been in `RandR` since 1.2 and
/// any server new enough to have the extension at all has it.
pub(super) fn probe() -> (bool, bool, bool) {
    let Ok((connection, screen)) = x11rb::connect(None) else {
        return (false, false, false);
    };
    // `extension_information` returns `Ok(None)` for "the server does not have
    // it" and `Err` for "the request itself failed", and those are the same
    // answer here: either way there is no RandR to drive gamma with.
    let randr = connection
        .extension_information(randr::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some();
    (true, randr, compositor_running(&connection, screen))
}

/// Whether a compositing manager owns `_NET_WM_CM_S<screen>`.
///
/// This is what decides whether an X11 overlay can dim at all rather than black
/// the screen out: X ignores a window's alpha channel, and Duja's overlay is
/// premultiplied black, so only a compositor blending the off-screen pixmap makes
/// 20% look different from 100%. [`crate::linux_caps`] carries the full argument.
///
/// The screen number comes from the connection. It is the *X screen* — a separate
/// root window, as in `DISPLAY=:0.1` — not a monitor, and while it is 0 in almost
/// every session, hard-coding it would silently answer for the wrong root in the
/// sessions where it is not.
///
/// Every failure answers "no compositor". `intern_atom` with `only_if_exists`
/// returns [`x11rb::NONE`] when nothing has ever created the atom, which is the
/// common case on a bare session and settles it without a second round trip; a
/// request that errors outright is a connection Duja cannot trust to place a
/// window on either.
fn compositor_running(connection: &impl x11rb::connection::Connection, screen: usize) -> bool {
    let selection = format!("{}{screen}", crate::linux_caps::COMPOSITOR_SELECTION_PREFIX);
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

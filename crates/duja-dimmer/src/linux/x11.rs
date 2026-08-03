//! The X11 half of ADR-0011's evidence: does the connection work, and does the
//! server answer for `RandR`.
//!
//! Deliberately tiny. Everything that *decides* anything lives in
//! [`crate::linux_caps`], which is pure and runs on every CI lane; this module
//! only fetches the two booleans that module cannot fetch for itself. The
//! connection is opened and dropped inside one call, because the probe runs at
//! startup and on session change and holding an X connection open between them
//! would be a file descriptor and a wakeup source for no benefit.

use x11rb::connection::RequestConnection;
use x11rb::protocol::randr;

/// Connect to the X server named by `DISPLAY` and report what it offers.
///
/// Returns `(connected, randr)`. A failed connect is `(false, false)` rather
/// than an error: [`crate::linux_caps::resolve`] already has a reason for it
/// (`ConnectFailed`), and a second error type here would only be mapped back
/// onto that one.
///
/// The `RandR` question is asked with `QueryExtension`, which is the protocol's
/// own answer and needs no version negotiation — Duja only needs to know the
/// extension is *there*, because per-CRTC gamma has been in `RandR` since 1.2 and
/// any server new enough to have the extension at all has it.
pub(super) fn probe() -> (bool, bool) {
    let Ok((connection, _screen)) = x11rb::connect(None) else {
        return (false, false);
    };
    // `extension_information` returns `Ok(None)` for "the server does not have
    // it" and `Err` for "the request itself failed", and those are the same
    // answer here: either way there is no RandR to drive gamma with.
    let randr = connection
        .extension_information(randr::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some();
    (true, randr)
}

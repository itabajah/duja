//! The Wayland half of ADR-0011's evidence: connect, and read the interface
//! names out of the registry.
//!
//! Deliberately tiny, for the same reason as the X11 half: everything that
//! *decides* anything lives in [`crate::linux_caps`], which is pure and runs on
//! every CI lane. This module fetches the one list that module cannot fetch for
//! itself, and hands it over as plain `String`s — which is not incidental. The
//! pure module may not name a `wayland-client` type, because the Windows and
//! macOS lanes compile it under `cfg(test)` where that crate does not exist, so
//! the boundary between the two is exactly here.
//!
//! The connection is opened and dropped inside one call. A capability probe runs
//! at startup and on session change; the *surfaces* wave 4 builds on top of this
//! will hold their own connection, and sharing one between a probe and a
//! compositor client would mean a queue this module has no business dispatching.

use wayland_client::Connection;
use wayland_client::globals::{GlobalListContents, registry_queue_init};

/// Connect to the compositor named by `WAYLAND_DISPLAY` and list what it offers.
///
/// Returns `(connected, interfaces)`. A failed connect is `(false, vec![])`
/// rather than an error, for the same reason as the X11 half:
/// [`crate::linux_caps::resolve`] already distinguishes "no server" from "the
/// server refused", and a second error type here would only be mapped back onto
/// those.
///
/// # Why one round trip and no dispatch loop
///
/// `registry_queue_init` binds `wl_registry` and performs a single blocking
/// round trip, which is enough by construction: a compositor sends a `global`
/// event for **every** interface it offers before the first `wl_display.sync`
/// completes. Anything advertised later is a hot-plugged output or a
/// dynamically-added global, neither of which changes whether the compositor
/// implements layer-shell or gamma-control. So there is no event loop here, no
/// queue to keep alive, and no thread.
pub(super) fn probe() -> (bool, Vec<String>) {
    let Ok(connection) = Connection::connect_to_env() else {
        return (false, Vec::new());
    };
    let Ok((globals, _queue)) = registry_queue_init::<Probe>(&connection) else {
        // The connection opened and the registry round trip did not. That is a
        // compositor answering the socket and then failing to speak the
        // protocol, which is not a state Duja can do anything with — report it
        // as connected-with-nothing rather than inventing a third outcome, and
        // let the capability report say both mechanisms are absent.
        return (true, Vec::new());
    };
    let interfaces = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|global| global.interface)
        .collect();
    (true, interfaces)
}

/// The client state `registry_queue_init` requires.
///
/// Empty on purpose: this probe binds nothing and dispatches nothing, so there
/// is no state to keep. It exists because the API is generic over a state type,
/// not because there is one.
struct Probe;

// `registry_queue_init` wants the `GlobalListContents` form specifically: it
// keeps the registry bound and hands the contents over, which is the whole reason
// the round trip is enough. `delegate_noop!` only generates the `()` form, so the
// impl is written out.
impl wayland_client::Dispatch<wayland_client::protocol::wl_registry::WlRegistry, GlobalListContents>
    for Probe
{
    fn event(
        _state: &mut Self,
        _registry: &wayland_client::protocol::wl_registry::WlRegistry,
        _event: wayland_client::protocol::wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _handle: &wayland_client::QueueHandle<Self>,
    ) {
        // Nothing to do: the globals are read once from `GlobalList::contents`
        // after the initial round trip, and a global appearing later is a
        // hot-plugged output rather than a compositor gaining a protocol.
    }
}

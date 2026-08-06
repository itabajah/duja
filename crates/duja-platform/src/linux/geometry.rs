//! The X11 half of the Linux tray anchor: the reads, and only the decisions that
//! cannot be made without them.
//!
//! Every *value* this module obtains is copied into one of
//! [`crate::linux_geometry`]'s plain structs and handed straight back out again.
//! Nothing here chooses a monitor, subtracts a strut or resolves a scale factor —
//! that all lives next door, where it is unit-tested on every CI lane. What is
//! left is the part no lane can run: the connection, the round trips, the
//! property decoding, and a small number of rules that are *about* the fetching
//! rather than about the geometry.
//!
//! Each such rule is documented where it is made, because between them they are
//! the code in this crate with the least test coverage and the most bug history.
//!
//! Neither a count nor a list is given here, and that is the third attempt at this
//! paragraph rather than laziness. The first said "no decisions" and the second
//! named three as though that were all of them; both were contradicted by this
//! same file a paragraph later, because a summary of rules is a second copy of
//! each — in the one place nothing checks it against the code. The rules live in
//! [`cursor_anchor`], [`monitors`], [`crtcs`], [`struts`], [`intern`] and on
//! [`WHOLE_PROPERTY`]; read them there. (That list named `atom` until rustdoc
//! refused it — the function was split into [`intern`] and [`resolve`] in this
//! same PR, and a link is the only kind of cross-reference here that a tool can
//! check.)
//!
//! # What it reads
//!
//! Five entries, and they are not five requests — the last bundles three DPI
//! sources of which one never touches the server:
//!
//! 1. `QueryPointer` on the root — where the cursor is.
//! 2. `GetGeometry` on the root — how big the screen is, which is the space
//!    struts are measured in.
//! 3. `RandR`'s CRTC list, each CRTC's rectangle, and the physical size of the
//!    first output it drives.
//! 4. `_NET_CLIENT_LIST` and each managed window's `_NET_WM_STRUT_PARTIAL` (or
//!    the legacy `_NET_WM_STRUT`) — what the panels have reserved.
//! 5. The three DPI sources — and only two of those are X requests. The
//!    XSETTINGS manager's `Xft/DPI` is a property read; the `Xft.dpi` X resource
//!    goes through `resource_manager`, which reads files as well as a property;
//!    and `WINIT_X11_SCALE_FACTOR` is an environment variable that never touches
//!    the server at all.
//!
//! # A connection per call
//!
//! The connection is opened and dropped inside [`cursor_anchor`], the same shape
//! and for the same reason as `duja-dimmer`'s X11 probe: this runs when the user
//! clicks the tray icon, and a connection kept open between clicks is a file
//! descriptor and a wakeup source earning nothing.
//!
//! The cost is a socket connect, a handful of round trips, **and blocking file
//! I/O**: `resource_manager::new_from_default` reads `$XENVIRONMENT` or
//! `$HOME/.Xdefaults-<hostname>` on every call (and `$HOME/.Xresources` or
//! `$HOME/.Xdefaults` too when `RESOURCE_MANAGER` is unset), plus a `gethostname`.
//! winit loads the same database once and caches it behind an `RwLock`, reloading
//! on `PropertyNotify`. Naming that rather than pricing this at "a few round
//! trips": it is the part of the per-call cost that is not obvious from the
//! request list, and it is the first thing to cache if the tray path ever needs
//! to be cheaper. The narrower `new_from_resource_manager` would skip the files,
//! and is deliberately not used — winit reads them, so a bare `startx` session
//! whose only `Xft.dpi` lives in `.Xresources` has to be visible here too.
//!
//! Requests are pipelined wherever there is more than one of a kind — every
//! `GetCrtcInfo` is sent before the first reply is read, and so is every strut
//! property — so the round-trip count grows with the *kinds* of question, not
//! with the number of monitors or windows.
//!
//! # Every failure is the fallback, not an error
//!
//! [`cursor_anchor`] returns [`Option`] and its caller substitutes the same
//! fallback anchor Windows uses when its own calls fail. There is nothing else to
//! do with an error here: [`crate::geometry::cursor_anchor`] promises never to
//! fail, and a flyout on a guessed work area is a cosmetic problem where no
//! flyout is a broken app.

use x11rb::connection::Connection;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, AtomEnum, ConnectionExt as _};
use x11rb::resource_manager;
use x11rb::rust_connection::RustConnection;

use crate::geometry::TrayAnchor;
use crate::linux_geometry::{DpiSources, X11Monitor, X11Screen, X11Strut, anchor_from_x11};
use crate::linux_xsettings;

/// How much of a property to ask for, in the four-byte units `GetProperty`
/// counts in: two gigabytes, so every read here is a single round trip and the
/// server returns whatever actually exists.
///
/// The ceiling is `i32::MAX / 4` rather than `u32::MAX` or `u32::MAX / 4`, and
/// the reason is the server's arithmetic rather than ours. `GetProperty` returns
/// `MINIMUM(remaining, 4 * long_length)`, and Xorg computes that `4 *` into a
/// `long` — 64-bit on the servers anyone runs, but 32-bit on an ILP32 build,
/// where `u32::MAX / 4` multiplies to `0xFFFFFFFC` and lands on the wrong side of
/// zero. Quartering `i32::MAX` instead keeps the product at `0x7FFFFFFC`, which is
/// positive in either width.
///
/// winit reads the same properties in 4 KB chunks and loops on `bytes_after`.
/// One shot is equivalent here only because the bound is far past what these
/// properties can hold — a `_NET_CLIENT_LIST` is four bytes per managed window
/// and an `_XSETTINGS_SETTINGS` blob is a few kilobytes — and it is one round trip
/// instead of two on a path that runs when the user clicks the tray.
const WHOLE_PROPERTY: u32 = i32::MAX.unsigned_abs() / 4;

/// The `RandR` version whose `GetScreenResourcesCurrent` this module prefers.
///
/// 1.3 introduced it, and the difference from `GetScreenResources` is not
/// cosmetic. The protocol spells it out both ways: the older request "explicitly
/// asks the server to ensure that the configuration data is up-to-date wrt the
/// hardware — if that requires polling, this is when such polling would take
/// place", where the newer one "merely returns the current configuration, and
/// does not poll for hardware changes". How long that poll takes is the driver's
/// business, which is the point — this path runs on a tray click and has nothing
/// to gain from re-probing every connector. A server older than 1.3 falls back to
/// the polling request, because a slow anchor beats no anchor.
const CURRENT_RESOURCES_SINCE: (u32, u32) = (1, 3);

/// The anchor for this X11 session, or [`None`] if the server would not answer.
///
/// [`None`] covers everything from "there is no `DISPLAY`" to "`RandR` reported no
/// enabled CRTC", because the caller does the same thing with all of them.
pub(crate) fn cursor_anchor() -> Option<TrayAnchor> {
    let (connection, screen_index) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots.get(screen_index)?.root;

    // Both of these are one round trip and neither depends on the other, so they
    // are sent together.
    let pointer = connection.query_pointer(root).ok()?;
    let geometry = connection.get_geometry(root).ok()?;
    let pointer = pointer.reply().ok()?;
    let geometry = geometry.reply().ok()?;

    // `root_x`/`root_y` are relative to the root the pointer is *logically on*,
    // which on a multi-screen display (`DISPLAY=:0.1`, one X server with several
    // independent roots) is not necessarily the one this connection's monitor
    // list describes. Placing a flyout with another screen's coordinates would
    // put it confidently in the wrong corner, so this is the fallback's case
    // rather than a best effort. Always true on the single-root sessions RandR
    // produces, which is every desktop this decade.
    if !pointer.same_screen {
        return None;
    }

    // The root's *live* size, not `setup().roots[..]`'s. The setup is a snapshot
    // from connect time, and a `RandR` reconfiguration since then would leave it
    // describing a screen that no longer exists — which would misplace every
    // strut, since a strut is a depth measured from this rectangle's edge.
    let screen = X11Screen {
        width: u32::from(geometry.width),
        height: u32::from(geometry.height),
    };

    let monitors = monitors(&connection, root);
    let struts = struts(&connection, root, screen);
    let xsettings_dpi = xsettings_dpi(&connection, screen_index);
    // `new_from_default` rather than the narrower `new_from_resource_manager`,
    // which would read only the root property and skip the files. winit reads the
    // files, so a bare `startx` session whose only `Xft.dpi` lives in
    // `.Xresources` has to be visible here too — the module docs' "A connection
    // per call" section prices what that costs. Held in a binding because
    // `get_string` borrows from it.
    let database = resource_manager::new_from_default(&connection).ok();
    let scale_override = std::env::var("WINIT_X11_SCALE_FACTOR").ok();

    anchor_from_x11(
        (i32::from(pointer.root_x), i32::from(pointer.root_y)),
        &monitors,
        screen,
        &struts,
        &DpiSources {
            scale_override: scale_override.as_deref(),
            xsettings_dpi,
            xft_dpi: database
                .as_ref()
                .and_then(|db| db.get_string("Xft.dpi", "")),
        },
    )
}

/// Every enabled CRTC, with the physical size of the first output it drives.
///
/// The filter is winit's — `width == 0 || height == 0 || outputs.is_empty()` —
/// and it means "this CRTC drives nothing", not "its output is disconnected".
/// Neither this nor winit reads `RandR`'s `connection` field, so a CRTC still
/// driving a `Disconnected` output passes both. Including a CRTC that drives
/// nothing would let the cursor land on a monitor that is displaying nothing.
///
/// Sharing the filter is what makes this module's list *comparable* to winit's,
/// which the scale chain's last step depends on; it does not make the two lists
/// identical, and `linux_geometry`'s module docs say where they part company.
///
/// An empty vector on any failure. `RandR` is not optional here the way it is for
/// the dimmer's probe: without it there is no monitor list at all, and the caller
/// falls back rather than placing a flyout against a screen rectangle that may
/// span several displays.
fn monitors(connection: &RustConnection, root: xproto::Window) -> Vec<X11Monitor> {
    let Some(crtcs) = crtcs(connection, root) else {
        return Vec::new();
    };

    // Pipelined: every `GetCrtcInfo` goes out before the first reply is read, so
    // a six-monitor desktop costs one round trip rather than six.
    let pending: Vec<_> = crtcs
        .iter()
        .filter_map(|&crtc| {
            connection
                .randr_get_crtc_info(crtc, x11rb::CURRENT_TIME)
                .ok()
        })
        .collect();
    let infos: Vec<_> = pending
        .into_iter()
        .filter_map(|cookie| cookie.reply().ok())
        .filter(|info| info.width > 0 && info.height > 0 && !info.outputs.is_empty())
        .collect();

    // Then the same trick for the output sizes, which cannot be requested until
    // the CRTC replies have named the outputs. Sending and reading are two passes
    // for the same reason as above: folding them into one closure would turn a
    // single round trip back into one per monitor.
    let pending: Vec<_> = infos
        .iter()
        .map(|info| {
            info.outputs.first().and_then(|&output| {
                connection
                    .randr_get_output_info(output, x11rb::CURRENT_TIME)
                    .ok()
            })
        })
        .collect();
    let sizes: Vec<_> = pending
        .into_iter()
        .map(|cookie| cookie.and_then(|cookie| cookie.reply().ok()))
        .collect();

    infos
        .iter()
        .zip(sizes)
        .map(|(info, size)| X11Monitor {
            bounds: crate::geometry::WorkRect {
                x: i32::from(info.x),
                y: i32::from(info.y),
                w: u32::from(info.width),
                h: u32::from(info.height),
            },
            // A failed output query leaves zero, which the scale chain reads as
            // "this display would not say how big it is" and answers 1.0 for.
            mm_width: size.as_ref().map_or(0, |size| size.mm_width),
            mm_height: size.as_ref().map_or(0, |size| size.mm_height),
        })
        .collect()
}

/// The CRTC list, from whichever request this server supports.
fn crtcs(connection: &RustConnection, root: xproto::Window) -> Option<Vec<randr::Crtc>> {
    let version = connection
        .randr_query_version(CURRENT_RESOURCES_SINCE.0, CURRENT_RESOURCES_SINCE.1)
        .ok()?
        .reply()
        .ok()?;
    // The reply is the lower of what was asked for and what the server has, so
    // getting back what we asked for means the server has at least that.
    if (version.major_version, version.minor_version) >= CURRENT_RESOURCES_SINCE {
        let reply = connection
            .randr_get_screen_resources_current(root)
            .ok()?
            .reply()
            .ok()?;
        Some(reply.crtcs)
    } else {
        let reply = connection
            .randr_get_screen_resources(root)
            .ok()?
            .reply()
            .ok()?;
        Some(reply.crtcs)
    }
}

/// Every managed window's reserved space.
///
/// The window list is `_NET_CLIENT_LIST`, which the window manager maintains and
/// which is therefore empty when there is no window manager — correctly, since
/// without one nothing is honouring struts anyway and the whole monitor really is
/// available.
///
/// A window that publishes both properties contributes only the partial one,
/// **unless the partial one reserves nothing**. EWMH's rule is that the window
/// manager MUST ignore `_NET_WM_STRUT` when `_NET_WM_STRUT_PARTIAL` is present,
/// and a client computing the same work area has to make the same choice or it
/// disagrees with the shell about where a window fits. Mutter is more forgiving
/// than the rule it implements — `meta_window_x11_update_struts` falls back to the
/// legacy property whenever the partial one produced no strut at all, which
/// includes twelve zeroes — and that is the behaviour copied here. It can only
/// ever add a reservation, and adding one costs a flyout that sits further from
/// an edge than it had to, where missing one costs a flyout under a panel.
///
/// The two strut atoms are looked up **independently**, which is not a detail: a
/// session whose panels use only the partial form may never have interned
/// `_NET_WM_STRUT` at all, and treating a missing atom as fatal would throw away
/// every strut on it.
///
/// **Every managed window counts, including ones on another workspace.**
/// `meta_workspace_ensure_work_areas_validated` collects struts from
/// `meta_workspace_list_windows` — that workspace's windows — where this reads
/// `_NET_CLIENT_LIST`, which is all of them. Matching it would mean
/// `_NET_CURRENT_DESKTOP` plus a `_NET_WM_DESKTOP` per window, and the difference
/// only shows for a non-sticky window with a strut, which in practice means a
/// panel someone has confined to one workspace. Again the error is to
/// over-reserve, which is why this is a paragraph rather than two more round
/// trips per tray click.
fn struts(connection: &RustConnection, root: xproto::Window, screen: X11Screen) -> Vec<X11Strut> {
    // All three names interned in one round trip.
    let client_list = intern(connection, b"_NET_CLIENT_LIST");
    let partial_atom = intern(connection, b"_NET_WM_STRUT_PARTIAL");
    let legacy_atom = intern(connection, b"_NET_WM_STRUT");
    let client_list = resolve(client_list);
    let partial_atom = resolve(partial_atom);
    let legacy_atom = resolve(legacy_atom);

    let Some(client_list) = client_list else {
        return Vec::new();
    };
    if partial_atom.is_none() && legacy_atom.is_none() {
        return Vec::new();
    }

    let clients = cardinals(connection, root, client_list, AtomEnum::WINDOW).unwrap_or_default();

    // Both properties for every client, all in flight before the first reply is
    // read. A desktop session has tens of managed windows and almost none of them
    // has a strut, so asking serially would be tens of round trips to find two
    // panels.
    let pending: Vec<_> = clients
        .iter()
        .map(|&client| {
            let partial = partial_atom
                .and_then(|name| property(connection, client, name, AtomEnum::CARDINAL));
            let legacy =
                legacy_atom.and_then(|name| property(connection, client, name, AtomEnum::CARDINAL));
            (partial, legacy)
        })
        .collect();

    pending
        .into_iter()
        .filter_map(|(partial, legacy)| {
            let twelve = partial
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| values(&reply))
                .and_then(|values| <[u32; 12]>::try_from(values.as_slice()).ok())
                .map(X11Strut::from_partial)
                .filter(X11Strut::reserves_anything);
            if twelve.is_some() {
                // The unused `legacy` cookie goes out of scope unread, which is
                // safe rather than merely tidy: x11rb's cookie `Drop` issues a
                // `discard_reply` for that sequence number, so the connection
                // stays in step and a later reply cannot be mistaken for this one.
                return twelve;
            }
            let four = values(&legacy?.reply().ok()?)?;
            Some(X11Strut::from_legacy(
                <[u32; 4]>::try_from(four.as_slice()).ok()?,
                screen,
            ))
        })
        .collect()
}

/// `Xft/DPI` from whichever window owns this screen's XSETTINGS selection.
///
/// The selection name carries the **X screen** number — a separate root window,
/// as in `DISPLAY=:0.1` — not a monitor. It is 0 in almost every session, but
/// hard-coding it would read another screen's settings in the ones where it is
/// not.
///
/// No owner means no settings manager, and [`None`] here sends the scale chain on
/// to the `Xft.dpi` X resource. winit arrives at the same place by a different
/// route: its `new_xsettings_screen` subscribes to `PropertyNotify` on the owning
/// window, which fails with `BadWindow` when the selection is unowned, and it then
/// skips XSETTINGS for the life of the connection.
fn xsettings_dpi(connection: &RustConnection, screen_index: usize) -> Option<f64> {
    // Both names in one round trip; the second is needed only if the first has an
    // owner, but asking for it costs nothing next to a second round trip.
    let selection = intern(connection, format!("_XSETTINGS_S{screen_index}").as_bytes());
    let settings = intern(connection, b"_XSETTINGS_SETTINGS");
    let selection = resolve(selection);
    let settings = resolve(settings);

    let owner = connection
        .get_selection_owner(selection?)
        .ok()?
        .reply()
        .ok()?;
    if owner.owner == x11rb::NONE {
        return None;
    }
    // The property's type is the `_XSETTINGS_SETTINGS` atom itself, which is what
    // the specification says and what winit asks for.
    let settings = settings?;
    let reply = connection
        .get_property(false, owner.owner, settings, settings, 0, WHOLE_PROPERTY)
        .ok()?
        .reply()
        .ok()?;
    linux_xsettings::xft_dpi(&reply.value)
}

/// Ask for an **existing** atom, returning the cookie so the caller can pipeline.
///
/// `only_if_exists` is true, and every atom this module wants is one someone else
/// creates: `_NET_CLIENT_LIST` by the window manager, the strut atoms by the
/// panels, the XSETTINGS pair by the settings manager. So "no such atom" and "no
/// window has ever published this" are the same answer, and asking this way gets
/// it in the same round trip without interning a name into a server where atoms
/// live until it exits.
fn intern<'c>(
    connection: &'c RustConnection,
    name: &[u8],
) -> Option<x11rb::cookie::Cookie<'c, RustConnection, xproto::InternAtomReply>> {
    connection.intern_atom(true, name).ok()
}

/// Read an [`intern`] cookie, mapping both "the request failed" and "the server
/// has no such atom" to [`None`].
fn resolve(
    cookie: Option<x11rb::cookie::Cookie<'_, RustConnection, xproto::InternAtomReply>>,
) -> Option<xproto::Atom> {
    Some(cookie?.reply().ok()?.atom).filter(|&atom| atom != x11rb::NONE)
}

/// Send a `GetProperty` for a whole 32-bit property, returning the cookie so the
/// caller can pipeline.
fn property(
    connection: &RustConnection,
    window: xproto::Window,
    property: xproto::Atom,
    kind: AtomEnum,
) -> Option<x11rb::cookie::Cookie<'_, RustConnection, xproto::GetPropertyReply>> {
    connection
        .get_property(false, window, property, kind, 0, WHOLE_PROPERTY)
        .ok()
}

/// Read a whole 32-bit property in one round trip.
fn cardinals(
    connection: &RustConnection,
    window: xproto::Window,
    name: xproto::Atom,
    kind: AtomEnum,
) -> Option<Vec<u32>> {
    let reply = property(connection, window, name, kind)?.reply().ok()?;
    values(&reply)
}

/// A property reply's 32-bit values, or [`None`] if it is not 32-bit at all.
///
/// A property that exists with the wrong format is not an error worth
/// distinguishing from one that does not exist: both mean this window said
/// nothing this module can use.
fn values(reply: &xproto::GetPropertyReply) -> Option<Vec<u32>> {
    reply.value32().map(Iterator::collect)
}

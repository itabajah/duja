//! Asking the display server where its outputs are.
//!
//! The evidence side of [`crate::linux_outputs`], and the same split as
//! [`super::x11`] / [`super::wayland`] versus [`crate::linux_caps`]: everything
//! that *decides* anything is pure and runs on every CI lane, and this module
//! only fetches what that one cannot fetch for itself. Nothing here can run in
//! CI — a GitHub runner has no X server and no compositor.
//!
//! Both halves connect, read, and drop the connection inside one call. Output
//! geometry is re-read on a display event, not watched: `duja-platform`'s uevent
//! pump already delivers those, and a second long-lived connection here would be
//! a second thing to keep alive across a session change for no extra information.

use duja_core::dimmer::DisplayBounds;

use crate::linux_caps::{SessionEnv, Transport, transport};
use crate::linux_outputs::ServerOutput;

/// Enumerate the outputs of whichever display server this session is on.
///
/// Returns an empty list when there is no display server, when the connection
/// fails, or when the server has nothing enabled. All three mean the same thing
/// to the caller — no display can be placed — and [`crate::linux_caps`] already
/// carries the reason for the user-facing report, so this does not duplicate it
/// as an error type.
///
/// Only the **selected** transport is asked, for the reason
/// [`super::probe_session`] gives: a Wayland session almost always also has an
/// Xwayland `DISPLAY`, and asking that too would return a second, contradictory
/// set of rectangles for a screen the compositor already owns.
#[must_use]
pub fn enumerate_outputs() -> Vec<ServerOutput> {
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    let env = SessionEnv {
        wayland_display: wayland_display.as_deref(),
        display: display.as_deref(),
    };
    match transport(env) {
        Transport::Wayland => wayland(),
        Transport::X11 => x11(),
        Transport::None => Vec::new(),
    }
}

/// The `RandR` output list: name, EDID, CRTC rectangle, CRTC id.
fn x11() -> Vec<ServerOutput> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::ConnectionExt as _;

    let Ok((connection, screen)) = x11rb::connect(None) else {
        return Vec::new();
    };
    let Some(root) = connection.setup().roots.get(screen).map(|s| s.root) else {
        return Vec::new();
    };

    // `GetScreenResourcesCurrent` reads the server's cached view. The plain
    // `GetScreenResources` re-probes every output over DDC, which takes on the
    // order of a second per connector on some drivers, and Duja calls this on
    // every display event.
    let Some(resources) = connection
        .randr_get_screen_resources_current(root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return Vec::new();
    };

    // `only_if_exists` = true: `EDID` is interned by the driver that publishes
    // the property, so its absence means no output has one and there is nothing
    // to ask for. `x11rb::NONE` reads as "no such atom" and every output then
    // reports `edid: None`, which the join treats as name-or-nothing.
    let edid_atom = connection
        .intern_atom(true, b"EDID")
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map_or(x11rb::NONE, |reply| reply.atom);

    let timestamp = resources.config_timestamp;
    let mut outputs = Vec::new();
    for output in resources.outputs {
        let Some(info) = connection
            .randr_get_output_info(output, timestamp)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            continue;
        };
        // A CRTC of NONE is an output with no rectangle: disconnected, or
        // connected and left disabled in the desktop's display settings. Either
        // way there is nothing to cover, and `crate::linux_outputs::join` would
        // drop it again on the empty bounds.
        if info.crtc == x11rb::NONE {
            continue;
        }
        let Some(crtc) = connection
            .randr_get_crtc_info(info.crtc, timestamp)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        else {
            continue;
        };
        outputs.push(ServerOutput {
            // `RandR` output names are ASCII in practice; `from_utf8_lossy`
            // rather than a failure so a driver with an odd byte still produces
            // a display that can join by EDID.
            name: String::from_utf8_lossy(&info.name).into_owned(),
            edid: read_edid(&connection, output, edid_atom),
            bounds: DisplayBounds::new(
                crtc.x.into(),
                crtc.y.into(),
                crtc.width.into(),
                crtc.height.into(),
            ),
            // The CRTC, not the output: two outputs on one CRTC are an X11
            // mirror, and they share both a framebuffer and a gamma table. See
            // `ServerOutput::token`.
            token: info.crtc.to_string(),
        });
    }
    outputs
}

/// The `EDID` output property, if the driver publishes one for this output.
///
/// The length is requested in 32-bit units, which is what the protocol counts in;
/// 128 of them is 512 bytes, enough for a base block and three extensions. Any
/// failure is `None` rather than an error: an EDID is the join's *fallback*, so a
/// driver that will not hand one over costs nothing on the common path where the
/// names already agree.
fn read_edid(
    connection: &impl x11rb::connection::Connection,
    output: x11rb::protocol::randr::Output,
    edid_atom: x11rb::protocol::xproto::Atom,
) -> Option<Vec<u8>> {
    use x11rb::protocol::randr::ConnectionExt as _;

    if edid_atom == x11rb::NONE {
        return None;
    }
    let reply = connection
        .randr_get_output_property(
            output,
            edid_atom,
            // `AtomEnum::ANY` as a bare 0: accept whatever type the driver
            // stamped the property with. Some publish it as `INTEGER` and some
            // as an interned `EDID` type, and either is the same bytes.
            x11rb::NONE,
            0,
            128,
            false,
            false,
        )
        .ok()?
        .reply()
        .ok()?;
    if reply.data.is_empty() {
        None
    } else {
        Some(reply.data)
    }
}

/// The `wl_output` list, with logical geometry from `xdg_output`.
///
/// Wayland publishes **no EDID** — there is no protocol for it — so a Wayland
/// output joins on its name or not at all. The name comes from `wl_output`
/// version 4, which is where the connector name (`DP-1`) became available; on an
/// older compositor there is no name to join on and Duja stays hardware-only,
/// which is the honest answer rather than one guessed from `make`/`model`.
fn wayland() -> Vec<ServerOutput> {
    use wayland_client::Connection;
    use wayland_client::globals::registry_queue_init;
    use wayland_client::protocol::wl_output::WlOutput;
    use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;

    let Ok(connection) = Connection::connect_to_env() else {
        return Vec::new();
    };
    let Ok((globals, mut queue)) = registry_queue_init::<Collector>(&connection) else {
        return Vec::new();
    };
    let handle = queue.handle();

    let mut collector = Collector::default();
    let registry = globals.registry();
    for global in globals.contents().clone_list() {
        if global.interface != WlOutput::interface().name {
            continue;
        }
        // Version 4 is what carries `name`; asking for more than the compositor
        // offers is a protocol error, so the bind is capped at what it advertised.
        let version = global.version.min(WL_OUTPUT_NAME_VERSION);
        let key = collector.outputs.len();
        let output = registry.bind::<WlOutput, _, _>(global.name, version, &handle, key);
        collector.outputs.push(Collected::new(output));
    }
    if collector.outputs.is_empty() {
        return Vec::new();
    }

    // `xdg_output` is optional. Without it the logical rectangle is unknown and
    // every output reports empty bounds, which the join drops — the same outcome
    // as an output the compositor disabled, and for the same reason: Duja does not
    // know where to put an overlay.
    let manager = globals.bind::<ZxdgOutputManagerV1, _, _>(&handle, 1..=3, ());
    if let Ok(manager) = &manager {
        for (key, entry) in collector.outputs.iter().enumerate() {
            manager.get_xdg_output(&entry.output, &handle, key);
        }
    }

    // Two round trips: the first drains the `wl_output` and `xdg_output` bursts
    // the requests above provoked, the second covers a compositor that defers
    // either `done` past the first. A `wl_display.sync` is answered only after
    // everything queued before it, so this cannot spin.
    for _ in 0..2 {
        if queue.roundtrip(&mut collector).is_err() {
            break;
        }
    }

    collector.finish()
}

/// The `wl_output` version that added the `name` event carrying the connector
/// name. Binding above what the compositor advertises is a protocol error, so
/// this is a ceiling rather than a requirement.
#[cfg(target_os = "linux")]
const WL_OUTPUT_NAME_VERSION: u32 = 4;

use wayland_client::Proxy as _;

/// One `wl_output` and everything the compositor has said about it so far.
struct Collected {
    output: wayland_client::protocol::wl_output::WlOutput,
    name: Option<String>,
    position: Option<(i32, i32)>,
    size: Option<(u32, u32)>,
}

impl Collected {
    fn new(output: wayland_client::protocol::wl_output::WlOutput) -> Self {
        Collected {
            output,
            name: None,
            position: None,
            size: None,
        }
    }
}

/// The dispatch state for one enumeration pass.
#[derive(Default)]
struct Collector {
    outputs: Vec<Collected>,
}

impl Collector {
    /// Turn what arrived into [`ServerOutput`]s, dropping anything incomplete.
    ///
    /// An output with no name cannot join (Wayland has no EDID to fall back to)
    /// and one with no logical rectangle cannot be placed, so neither is carried
    /// forward as a half-answer.
    fn finish(self) -> Vec<ServerOutput> {
        self.outputs
            .into_iter()
            .filter_map(|entry| {
                let name = entry.name?;
                let (x, y) = entry.position?;
                let (width, height) = entry.size?;
                Some(ServerOutput {
                    // The connector name is both the join key and the address:
                    // `zwlr_gamma_control_manager_v1` takes a `wl_output`, and
                    // Wayland has no mirroring for a framebuffer token to differ
                    // from an output token. See `ServerOutput::token`.
                    token: name.clone(),
                    name,
                    edid: None,
                    bounds: DisplayBounds::new(x, y, width, height),
                })
            })
            .collect()
    }

    /// The entry a bound object belongs to, by the index handed over as its user
    /// data.
    fn entry(&mut self, key: usize) -> Option<&mut Collected> {
        self.outputs.get_mut(key)
    }
}

impl
    wayland_client::Dispatch<
        wayland_client::protocol::wl_registry::WlRegistry,
        wayland_client::globals::GlobalListContents,
    > for Collector
{
    fn event(
        _state: &mut Self,
        _registry: &wayland_client::protocol::wl_registry::WlRegistry,
        _event: wayland_client::protocol::wl_registry::Event,
        _data: &wayland_client::globals::GlobalListContents,
        _connection: &wayland_client::Connection,
        _handle: &wayland_client::QueueHandle<Self>,
    ) {
        // The globals are read once from `GlobalList::contents`. An output that
        // appears during this pass is a hot-plug, and the uevent pump will bring
        // the whole enumeration round again.
    }
}

impl wayland_client::Dispatch<wayland_client::protocol::wl_output::WlOutput, usize> for Collector {
    fn event(
        state: &mut Self,
        _output: &wayland_client::protocol::wl_output::WlOutput,
        event: wayland_client::protocol::wl_output::Event,
        key: &usize,
        _connection: &wayland_client::Connection,
        _handle: &wayland_client::QueueHandle<Self>,
    ) {
        // Only `name` is taken from `wl_output`. Its `geometry` origin and `mode`
        // size are in physical pixels with an integer `scale` beside them, which
        // cannot express fractional scaling and so cannot be divided back into
        // the logical rectangle a surface occupies. `xdg_output` answers that
        // directly, and mixing the two would produce a rectangle that is right on
        // some monitors and quietly wrong on others.
        if let wayland_client::protocol::wl_output::Event::Name { name } = event
            && let Some(entry) = state.entry(*key)
        {
            entry.name = Some(name);
        }
    }
}

impl
    wayland_client::Dispatch<
        wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1,
        (),
    > for Collector
{
    fn event(
        _state: &mut Self,
        _manager: &wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1,
        _event: wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::Event,
        _data: &(),
        _connection: &wayland_client::Connection,
        _handle: &wayland_client::QueueHandle<Self>,
    ) {
        // The manager is a factory and sends no events.
    }
}

impl
    wayland_client::Dispatch<
        wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::ZxdgOutputV1,
        usize,
    > for Collector
{
    fn event(
        state: &mut Self,
        _xdg_output: &wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::ZxdgOutputV1,
        event: wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::Event,
        key: &usize,
        _connection: &wayland_client::Connection,
        _handle: &wayland_client::QueueHandle<Self>,
    ) {
        use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::Event;

        let Some(entry) = state.entry(*key) else {
            return;
        };
        match event {
            Event::LogicalPosition { x, y } => entry.position = Some((x, y)),
            Event::LogicalSize { width, height } => {
                // The protocol types these `i32` and a negative one is not a
                // size. `try_from` failing leaves the entry without a size, so
                // `finish` drops it rather than wrapping into a vast `u32`.
                if let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) {
                    entry.size = Some((width, height));
                }
            }
            _ => {}
        }
    }
}

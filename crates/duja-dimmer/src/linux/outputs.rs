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
    use x11rb::protocol::randr::{self, ConnectionExt as _};

    let Ok((connection, screen)) = x11rb::connect(None) else {
        return Vec::new();
    };
    let Some(root) = connection.setup().roots.get(screen).map(|s| s.root) else {
        return Vec::new();
    };

    // Negotiate the extension version before issuing any of its requests. The
    // protocol leaves a client's behaviour undefined otherwise, and every other
    // client (libXrandr, GTK, `xrandr`, winit) does it. 1.3 is what
    // `GetScreenResourcesCurrent` needs; the rest of this function is 1.2. The
    // capability probe in `super::x11` deliberately does *not* negotiate, and
    // that is not an inconsistency: it only asks whether the extension exists
    // and issues no RandR request at all.
    if connection
        .randr_query_version(1, 3)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_none()
    {
        return Vec::new();
    }

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

    // `only_if_exists` = true, so a server where nothing has ever created the
    // atom answers `NONE` and every output reports `edid: None` — which the join
    // treats as name-or-nothing. `EDID_DATA` is what pre-RandR-1.2 drivers called
    // it, and rescuing the legacy stacks is what this property is *for*, so it is
    // worth one extra round trip when the modern name is absent.
    //
    // Two things this is not: it is not proof a driver publishes the property
    // (any client can intern an atom, so `EDID` existing says only that something
    // asked about it), and it is not per output — a server offering `EDID_DATA`
    // alone is only reached when no `EDID` atom exists at all. Both failure
    // directions cost the *fallback* and nothing else, which is why one
    // connection-wide lookup is the right shape.
    let edid_atom = intern(&connection, b"EDID")
        .unwrap_or_else(|| intern(&connection, b"EDID_DATA").unwrap_or(x11rb::NONE));

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
        // A non-`Success` status leaves every other field in the reply
        // undefined per the protocol — `InvalidConfigTime` for a timestamp the
        // server has moved past, which is what a hot-plug racing this walk looks
        // like. Current X servers always answer `Success`, so this is a latent
        // guard rather than a live one, and it costs a comparison.
        if info.status != randr::SetConfig::SUCCESS {
            continue;
        }
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
            .filter(|crtc| crtc.status == randr::SetConfig::SUCCESS)
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
            //
            // Through `crtc_token` rather than `to_string`, because the gamma
            // channel parses this back with `crtc_from_token` and the two ends
            // are in different crates: the pair is round-tripped by one test, so
            // changing the format here cannot silently make every Linux display
            // refuse a ramp.
            token: crate::linux_gamma::crtc_token(info.crtc),
        });
    }
    outputs
}

/// Look up an existing atom, or `None` if nothing has ever created it.
fn intern(connection: &impl x11rb::connection::Connection, name: &[u8]) -> Option<u32> {
    use x11rb::protocol::xproto::ConnectionExt as _;

    connection
        .intern_atom(true, name)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.atom)
        .filter(|atom| *atom != x11rb::NONE)
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
    // `data` is sized `num_items * format/8`, so at format 32 it would be four
    // bytes per item in the host's order rather than the EDID's bytes. Every X
    // driver publishes this at format 8; anything else is not an EDID this can
    // compare, and a wrong-format blob could only ever fail to match — but
    // saying so is better than relying on that.
    if reply.format != 8 || reply.data.is_empty() {
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
    use wayland_client::Proxy as _;
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

    // One round trip is enough by construction: `wl_display.sync` is answered
    // only after every request queued before it and every event those requests
    // generated, so the whole `wl_output`/`xdg_output` burst is delivered before
    // it returns. There is no `done` handshake to wait on and nothing here can
    // spin.
    if queue.roundtrip(&mut collector).is_err() {
        return Vec::new();
    }

    collector.finish()
}

/// The `wl_output` version that added the `name` event. Binding above what the
/// compositor advertises is a protocol error, so this is a ceiling rather than a
/// requirement, and `xdg_output`'s own `name` covers the compositors below it.
const WL_OUTPUT_NAME_VERSION: u32 = 4;

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
                    // The output name is both the join key and the address:
                    // `zwlr_gamma_control_manager_v1` and `zwlr_layer_shell_v1`
                    // are both per-`wl_output`, so per-output is the granularity
                    // Wayland grants.
                    //
                    // An earlier draft added that this is "NOT a mirror-set key,
                    // because a compositor that mirrors gives the two outputs the
                    // same logical rectangle and different names". That premise is
                    // wrong and `#130` retracted it: KWin and Hyprland both
                    // *withdraw* a mirrored monitor's `wl_output` global, and
                    // wlroots has no mirror mode, so a mirrored pair is one output
                    // and there is nothing for a mirror-set key to collapse. What
                    // survives is narrower — two outputs a user placed at one
                    // origin would not group — and `docs/debt.md` carries that,
                    // rather than what this comment used to point at.
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
            // `xdg_output` has carried a name since **version 2**, and the
            // protocol requires compositors to keep sending it even though it is
            // marked deprecated in favour of `wl_output.name`. Without this arm a
            // compositor that advertises `wl_output` v3 and `xdg_output` v2 —
            // wlroots and Mutter both did until fairly recently, so anything on
            // an older LTS — hands over a full logical rectangle with no name to
            // join it by, and every output is dropped for want of data that
            // arrived. `wl_output`'s name wins when both come, hence the guard.
            Event::Name { name } => {
                if entry.name.is_none() {
                    entry.name = Some(name);
                }
            }
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

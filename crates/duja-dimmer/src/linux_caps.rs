//! The pure half of ADR-0011: environment and registry contents in, a software
//! dimming capability report out.
//!
//! ADR-0011 decided that Duja detects **by capability, never by compositor
//! identity** — no table of desktop names, because such a table is a claim about
//! third-party software Duja cannot verify, it goes stale silently, and it
//! cannot represent a compositor that gains support in a later release. This
//! module is the rule that decision names, and it is deliberately the largest
//! testable surface the feature has: real X11 windows and real `wl_surface`s
//! cannot run on a headless runner, so everything that *can* be decided without
//! one is decided here.
//!
//! # It names no Wayland type, and that is load-bearing
//!
//! `wayland-client` and `wayland-protocols-wlr` are Linux-target dependencies,
//! and the Windows and macOS lanes compile this module under `cfg(test)` where
//! they do not exist. A single `zwlr_layer_shell_v1` in a signature would turn
//! "tested on every lane" into a build error on two of them. So the interface is
//! plain data: interface names as `&str`, environment variables as
//! `Option<&str>`, a capability report out. `duja-platform`'s `mac_events` obeys
//! the same constraint for the same reason (it takes a raw `u32`, not a
//! `CGDisplayChangeSummaryFlags`).
//!
//! # Presence is the answer for the overlay, and not for gamma
//!
//! `zwlr_layer_shell_v1` in the registry means a layer surface can be created.
//! `zwlr_gamma_control_manager_v1` in the registry does **not** mean gamma can be
//! taken: that protocol describes itself as being for a privileged client, grants
//! one client *exclusive* access per output, and defines a `failed` event on the
//! per-output object that `get_gamma_control` returns. A session running
//! `wlsunset` or `gammastep` advertises the global and still refuses Duja — which
//! is not a corner case, it is the commonest reason a user would have that
//! protocol at all. So the gamma arm is settled in two steps and the report is a
//! value that can change after startup, not one settled once.
//!
//! # On X11 a connection is not the whole answer for the overlay either
//!
//! ADR-0011 as first written said an X11 overlay needs no extension and "a
//! successful connection is the whole requirement". That was wrong, and wrong in
//! the direction that breaks a screen rather than the direction that refuses one.
//!
//! X11 has no per-window translucency of its own. An ARGB32 window's alpha
//! channel means nothing to the X server: it draws the window's **colour bytes,
//! at full coverage, whatever they are**. The channel is honoured only by a
//! **compositing manager**, which redirects the window to an off-screen pixmap
//! and blends that. Duja's overlay is filled black, so with no compositor running
//! every alpha from 1% to 100% paints the same thing — a black rectangle over the
//! whole monitor, with Duja's own UI behind it and the only exit a keyboard the
//! user can no longer see. (Premultiplied or straight alpha makes no difference;
//! black is `(0, 0, 0)` either way. A lighter fill would give an opaque grey
//! screen instead, which is not an improvement.)
//!
//! So the X11 overlay arm asks a second question: does a compositing manager own
//! the `_NET_WM_CM_S<n>` selection. Every compositing manager takes it — the
//! window-manager spec requires it, which is why `gdk_screen_is_composited` and
//! Qt's `isCompositingManagerRunning` ask the same question — and the X server
//! clears a selection when its owner disconnects, so the answer cannot go stale.
//!
//! **This is necessary, not sufficient, and neither half is settled at startup.**
//! Two things the check does not cover, both owed to the wave that builds the
//! window:
//!
//! - A compositing manager that **stops** mid-session (`picom` crashes, or the
//!   user restarts it) turns an already-mapped overlay into that black rectangle.
//!   Nothing re-resolves the report today, because there is no overlay to protect
//!   yet; when there is, it has to watch the selection —
//!   `XFixesSelectSelectionInput` on `_NET_WM_CM_S<n>` — and tear down on an owner
//!   change, which is the exact analogue of [`SurfaceCaps::refuse_gamma`].
//! - Every compositing manager **unredirects** a fullscreen window as a
//!   performance optimisation, and an always-on-top fullscreen window is precisely
//!   what an overlay is. A window with an alpha channel below 1 normally
//!   disqualifies itself, but the EWMH way to be sure is
//!   `_NET_WM_BYPASS_COMPOSITOR = 2`, and the overlay must set it.
//!
//! `docs/debt.md` carries both.

use std::fmt;

/// Which display server this session is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A Wayland compositor.
    Wayland,
    /// An X server (including Xwayland, which Duja neither detects nor needs to:
    /// an X client on Xwayland gets X11's mechanisms and mostly they work — with
    /// the caveat below).
    ///
    /// The caveat is the compositor check. Under Xwayland the Wayland compositor
    /// blends X windows whether or not anything owns `_NET_WM_CM_S<n>`, so a
    /// session that reaches this arm with `WAYLAND_DISPLAY` unset — a systemd user
    /// unit, a sanitised environment — could be told its overlay is unavailable
    /// when it would in fact work. wlroots, Mutter and Weston all have their X
    /// window manager claim the selection, so this is a small risk rather than a
    /// live one, and it errs toward refusing a working overlay rather than
    /// blacking out a screen.
    X11,
    /// Neither. A TTY, a container, a service.
    None,
}

/// Why a mechanism is not available.
///
/// Carried rather than collapsed to a bool because `dujactl doctor` prints it,
/// and "software dimming unavailable" with no reason is what sends a user to an
/// issue tracker. Every variant is a state a user can be in and none of them is a
/// bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No `WAYLAND_DISPLAY` and no `DISPLAY`: there is no display server to ask.
    ///
    /// `WAYLAND_SOCKET` is **not** in that list, because [`transport`] does not
    /// consult it - see its docs for why a single-use fd cannot be a durable
    /// session marker. Naming it here was worse than untidy: a Flatpak or portal
    /// client, which is exactly the case D-093 describes, would be told by
    /// `dujactl doctor` that `WAYLAND_SOCKET` is unset when it is set, and sent
    /// to fix the one variable that was already correct.
    NoDisplayServer,
    /// A display server was named but the connection to it failed.
    ConnectFailed,
    /// The compositor does not advertise the protocol this mechanism needs.
    ProtocolAbsent {
        /// The interface that was looked for, e.g. `zwlr_layer_shell_v1`.
        interface: &'static str,
    },
    /// The X server does not answer for the extension this mechanism needs.
    ExtensionAbsent {
        /// The extension name, e.g. `RANDR`.
        extension: &'static str,
    },
    /// X11 only: no compositing manager owns `_NET_WM_CM_S<n>`, so the X server
    /// would draw the overlay's colour bytes at full coverage and ignore its
    /// alpha.
    ///
    /// Not a Wayland state. A Wayland compositor *is* the compositing manager, so
    /// there is no session in which layer-shell exists and blending does not.
    NoCompositor,
    /// The protocol is advertised and the bind was **refused** — another client
    /// holds it exclusively, or the output does not support gamma tables.
    ///
    /// Only reachable for gamma: layer-shell has no equivalent refusal.
    Refused,
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unavailable::NoDisplayServer => {
                write!(f, "no display server (neither WAYLAND_DISPLAY nor DISPLAY)")
            }
            Unavailable::ConnectFailed => write!(f, "the display server refused the connection"),
            Unavailable::ProtocolAbsent { interface } => {
                write!(f, "the compositor does not offer {interface}")
            }
            Unavailable::ExtensionAbsent { extension } => {
                write!(f, "the X server does not offer the {extension} extension")
            }
            Unavailable::NoCompositor => {
                write!(
                    f,
                    "no compositing manager is running, so X11 cannot blend a translucent window"
                )
            }
            Unavailable::Refused => {
                write!(
                    f,
                    "another client holds it, or the output has no gamma table"
                )
            }
        }
    }
}

/// Whether one mechanism can be used, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// Usable.
    Available,
    /// Not usable, with the reason.
    Unavailable(Unavailable),
}

impl Capability {
    /// Whether this mechanism can be used.
    #[must_use]
    pub fn is_available(&self) -> bool {
        matches!(self, Capability::Available)
    }
}

/// What this session can actually do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceCaps {
    /// The display server this report describes.
    pub transport: Transport,
    /// Whether a click-through overlay can be placed. A **session**-wide answer:
    /// layer-shell and X11 override-redirect windows are either available or not,
    /// with no per-output component.
    pub overlay: Capability,
    /// Whether a gamma ramp can be set. Starts as the *protocol-level* answer and
    /// may be downgraded per output by [`SurfaceCaps::refuse_gamma`] once a bind
    /// is actually attempted.
    pub gamma: Capability,
}

/// The interface that gets a Wayland overlay onto the screen at all.
pub const LAYER_SHELL: &str = "zwlr_layer_shell_v1";

/// The interface that gets a dim *into* that overlay.
///
/// A dim is one translucent black rectangle covering an output, and `wl_shm` sizes
/// a buffer in pixels — so without `wp_viewporter` to scale a single pixel up,
/// covering a 4K output means allocating and re-rendering a 33 MB framebuffer per
/// output per slider sample. Duja treats it as a requirement rather than an
/// optimisation, which is why it is here in the *report* and not only in the
/// backend: a doctor line saying "overlay: available" for a session the backend
/// then refuses is worse than either answer alone.
///
/// It is a stable protocol from 2016 and every implementation checked ships it —
/// wlroots as `types/wlr_viewporter.c`, `KWin` as `src/wayland/viewporter.cpp`,
/// Mutter as `src/wayland/meta-wayland-viewporter.c` — so this is expected never to
/// fire.
///
/// **Expected, not proven, and that is the reason it is a check at all.**
/// `zwlr_layer_shell_v1` has at least five independent server implementations —
/// wlroots, `KWin`, Hyprland's own, Smithay's (which niri and COSMIC build on), and
/// Mir's — so no enumeration here can establish that none of them lacks
/// `wp_viewporter`, and one that grew a sixth would not update this comment. An
/// unchecked assumption of that shape fails as a `bind` error at startup on a
/// session the report has already told the user is fine; a checked one is a line in
/// `dujactl doctor` naming the interface.
pub const VIEWPORTER: &str = "wp_viewporter";

/// The interface that says *which output* an overlay belongs on.
///
/// `zwlr_layer_shell_v1.get_layer_surface` takes a `wl_output`, and everything
/// above this layer speaks in rectangles, so a Wayland overlay is only placeable
/// if each output's **logical** geometry is knowable. `wl_output`'s own events
/// report a mode in physical pixels and an integer scale, which cannot express
/// fractional scaling and so cannot be divided back into the desktop rectangle a
/// surface actually occupies. `zxdg_output_manager_v1` is the only protocol that
/// answers it.
///
/// Its absence would already be visible further up — [`crate::linux_outputs::join`]
/// drops an output with no rectangle, so the display never acquires bounds — but it
/// would be visible as *hardware control and no displays to dim*, which reads as a
/// different fault than the one it is.
pub const XDG_OUTPUT: &str = "zxdg_output_manager_v1";

/// The interface a Wayland gamma ramp needs.
pub const GAMMA_CONTROL: &str = "zwlr_gamma_control_manager_v1";

/// The X extension a gamma ramp needs.
pub const RANDR: &str = "RANDR";

/// The selection a compositing manager owns to announce itself on X screen
/// `screen`.
///
/// The spec names it `_NET_WM_CM_Sn`; on the overwhelmingly common single-screen
/// session that is `_NET_WM_CM_S0`, but the number is the *X screen* — a separate
/// root window, as in `DISPLAY=:0.1` — and not a monitor, so it comes from the
/// connection rather than being hard-coded.
///
/// This lives here, on the pure side, rather than beside the one call that uses
/// it. It is string construction and needs no display server, and it is the only
/// part of the compositor check any test can reach: a typo in the atom name
/// compiles on all three lanes, passes every test, and reports "no compositing
/// manager" on **every** X11 session forever, which is a permanently disabled
/// overlay on a platform this project has no machine to notice it on.
#[must_use]
pub fn compositor_selection(screen: usize) -> String {
    format!("_NET_WM_CM_S{screen}")
}

/// The environment variables that name a display server.
///
/// Borrowed rather than owned so the caller can pass what `std::env::var` gave it
/// without allocating, and so a test can hand over literals.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionEnv<'a> {
    /// `WAYLAND_DISPLAY`.
    pub wayland_display: Option<&'a str>,
    /// `WAYLAND_SOCKET`.
    ///
    /// A pre-connected file descriptor handed over by a parent instead of a
    /// socket *name*. It is how `wl_display_connect` is meant to be reached in a
    /// sandbox — Flatpak's `wayland` socket permission, xdg-desktop-portal, and
    /// anything that launches a client with the compositor connection already
    /// open — and in that shape `WAYLAND_DISPLAY` may well be unset.
    pub wayland_socket: Option<&'a str>,
    /// `DISPLAY`.
    pub display: Option<&'a str>,
}

/// The three environment variables [`SessionEnv`] borrows, owned.
///
/// Exists because [`SessionEnv`] borrows and every call site therefore had to
/// read the variables into locals first — which meant five copies of "read these
/// and build the struct", and **D-093 was one of them getting the list wrong**:
/// `WAYLAND_SOCKET` was added to the type and the five sites would each have had
/// to be found and edited to supply it. One door, so the next variable is added
/// once.
#[derive(Debug, Clone, Default)]
pub struct SessionEnvVars {
    wayland_display: Option<String>,
    wayland_socket: Option<String>,
    display: Option<String>,
}

impl SessionEnvVars {
    /// Read them from this process's environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// The same read, against an arbitrary lookup, so the **variable names** are
    /// testable without touching the process environment.
    ///
    /// Splitting this out is not ceremony. A typo here — `WAYLAND_SOCKETS`,
    /// `WAYLAND-DISPLAY` — compiles on all three lanes, passes every test on all
    /// three lanes, and silently disables X11 dimming for every Linux user
    /// forever. That is verbatim the argument [`compositor_selection`]'s own doc
    /// makes for why it lives in this module, and the first version of
    /// [`from_env`](Self::from_env) had exactly the shape it warns about.
    ///
    /// `duja-platform`'s `geometry` keeps its equivalent read *outside* its pure
    /// module, arguing that a test could only assert `std::env::var` reads the
    /// environment and would have to mutate a shared process environment to do
    /// it — unsound in a threaded harness. This answers that objection rather
    /// than ignoring it: the lookup is injected, so nothing is mutated and what
    /// is asserted is the part that can be wrong.
    #[must_use]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        SessionEnvVars {
            wayland_display: lookup("WAYLAND_DISPLAY"),
            wayland_socket: lookup("WAYLAND_SOCKET"),
            display: lookup("DISPLAY"),
        }
    }

    /// Borrow them as the pure input [`transport`] and [`resolve`] take.
    #[must_use]
    pub fn as_session_env(&self) -> SessionEnv<'_> {
        SessionEnv {
            wayland_display: self.wayland_display.as_deref(),
            wayland_socket: self.wayland_socket.as_deref(),
            display: self.display.as_deref(),
        }
    }
}

/// What connecting actually found. Supplied by the caller, which is the only part
/// that touches a display server.
#[derive(Debug, Clone, Copy, Default)]
pub struct Probe<'a> {
    /// Whether the connection to the chosen transport succeeded.
    pub connected: bool,
    /// The interface names in the Wayland registry. Empty on X11.
    pub globals: &'a [&'a str],
    /// Whether the X server answered for the `RandR` extension. Ignored on
    /// Wayland.
    pub randr: bool,
    /// Whether a compositing manager owns `_NET_WM_CM_S<n>`. Ignored on Wayland,
    /// where the compositor is the display server.
    pub compositor: bool,
}

/// Choose the transport this session is on.
///
/// **Wayland wins when both are set**, which is the common case: almost every
/// Wayland session also runs Xwayland and sets `DISPLAY`, so preferring X11 there
/// would put Duja on the compatibility layer and give it Xwayland's view of the
/// screen rather than the compositor's.
///
/// **`WAYLAND_SOCKET` is deliberately NOT consulted, and the reason is the
/// interesting part of [D-093](https://github.com/itabajah/duja/blob/main/docs/debt.md#d-093).**
///
/// The row is real: that variable carries a pre-connected compositor file
/// descriptor rather than a socket *name*, a client launched that way (Flatpak,
/// a portal) may have no `WAYLAND_DISPLAY`, and this function then answers
/// `X11` because Xwayland set `DISPLAY`. P8 wave 4 tried the obvious fix —
/// treat it as Wayland — and a review found that it makes the session **worse**.
///
/// `WAYLAND_SOCKET` is **single-use**. `wayland-client`'s `connect_to_env`
/// parses the fd, takes it as an `OwnedFd`, and calls
/// `env::remove_var("WAYLAND_SOCKET")` so children do not inherit it
/// (`wayland-client-0.31/src/conn.rs`). Duja opens **four independent**
/// connections (`probe`, `enumerate_outputs`, the layer surface, the gamma
/// manager) and re-reads the environment on **every** transport decision, on
/// purpose — a cached answer is wrong for the session that changed under a
/// running process. Those two designs cannot both hold for a one-shot variable.
/// The result was: `Wayland` for exactly one call, `X11` for the rest of the
/// process, with Wayland-format gamma tokens already stamped that the X11 arm
/// rejects — so gamma dimming died silently, where before the change the session
/// at least worked through Xwayland.
///
/// So this stays `X11` until Duja either opens one connection it keeps, or
/// captures the fd itself before anything else can consume it. The row is
/// re-opened with that finding, which is worth more than the row was.
///
/// An **empty** value is treated as unset. That is not pedantry: `DISPLAY=` is
/// what a login shell leaves behind when a session script clears it, and
/// treating it as a display server name produces a connect failure reported as
/// "the display server refused the connection" instead of "there is no display
/// server", which sends the user looking for the wrong thing.
#[must_use]
pub fn transport(env: SessionEnv<'_>) -> Transport {
    let set = |value: Option<&str>| value.is_some_and(|v| !v.is_empty());
    if set(env.wayland_display) {
        Transport::Wayland
    } else if set(env.display) {
        Transport::X11
    } else {
        Transport::None
    }
}

/// Resolve what this session can do.
///
/// Session-type and compositor strings are **not** inputs, deliberately:
/// `XDG_CURRENT_DESKTOP` is set by session scripts, absent on a TTY launch,
/// inherited unchanged into nested sessions, and colon-separated multi-valued.
/// Identity is not capability (ADR-0011).
#[must_use]
pub fn resolve(env: SessionEnv<'_>, probe: &Probe<'_>) -> SurfaceCaps {
    let transport = transport(env);
    match transport {
        Transport::None => SurfaceCaps {
            transport,
            overlay: Capability::Unavailable(Unavailable::NoDisplayServer),
            gamma: Capability::Unavailable(Unavailable::NoDisplayServer),
        },
        _ if !probe.connected => SurfaceCaps {
            transport,
            overlay: Capability::Unavailable(Unavailable::ConnectFailed),
            gamma: Capability::Unavailable(Unavailable::ConnectFailed),
        },
        Transport::Wayland => SurfaceCaps {
            transport,
            // Independently of each other: layer-shell without gamma-control is
            // the commonest wlroots configuration, and a table that treated them
            // as one capability would refuse the overlay on it.
            overlay: wayland_overlay(probe.globals),
            gamma: from_registry(probe.globals, GAMMA_CONTROL),
        },
        Transport::X11 => SurfaceCaps {
            transport,
            // An override-redirect, input-transparent window needs no extension,
            // but it does need someone to blend it: X11 itself ignores an alpha
            // channel, and Duja's overlay is premultiplied black, so without a
            // compositing manager every alpha renders as opaque black and the
            // screen goes dark. See the module docs.
            overlay: if probe.compositor {
                Capability::Available
            } else {
                Capability::Unavailable(Unavailable::NoCompositor)
            },
            gamma: if probe.randr {
                Capability::Available
            } else {
                Capability::Unavailable(Unavailable::ExtensionAbsent { extension: RANDR })
            },
        },
    }
}

/// Every interface a click-through dimming surface is built from: one to place it
/// ([`LAYER_SHELL`]), one to fill it ([`VIEWPORTER`]), one to know which output it
/// goes on ([`XDG_OUTPUT`]).
///
/// **The order is the reported order**, and it is the order the backend binds them
/// in, so the two can never name different missing interfaces for one session.
const WAYLAND_OVERLAY_INTERFACES: [&str; 3] = [LAYER_SHELL, VIEWPORTER, XDG_OUTPUT];

/// Whether this registry can host a click-through dimming surface.
///
/// The first missing one wins, and that is why [`LAYER_SHELL`] is first: the answer
/// is a sentence a user reads. A GNOME session has **two** of the three — Mutter
/// implements `wp_viewporter` and `zxdg_output_manager_v1` and has no layer-shell
/// at all — so `zwlr_layer_shell_v1` is both the true answer there and the only one
/// that does not send someone looking for a Mutter bug that is not there.
fn wayland_overlay(globals: &[&str]) -> Capability {
    for interface in WAYLAND_OVERLAY_INTERFACES {
        match from_registry(globals, interface) {
            Capability::Available => {}
            absent @ Capability::Unavailable(_) => return absent,
        }
    }
    Capability::Available
}

/// Whether `interface` is in the registry.
fn from_registry(globals: &[&str], interface: &'static str) -> Capability {
    if globals.contains(&interface) {
        Capability::Available
    } else {
        Capability::Unavailable(Unavailable::ProtocolAbsent { interface })
    }
}

impl SurfaceCaps {
    /// Downgrade gamma after a refused bind.
    ///
    /// ADR-0011 step 5. `zwlr_gamma_control_v1` sends `failed` **after** a
    /// successful `get_gamma_control`, so registry presence cannot settle this
    /// arm and the report has to be able to move from available to unavailable
    /// once the attempt is made. Same rule one layer below `#96`, which
    /// substitutes an overlay when a gamma ramp is refused, and `#109`, which
    /// drops a refused record rather than latching it.
    ///
    /// Idempotent, and it never *upgrades*: a second refusal changes nothing, and
    /// a mechanism already unavailable for a different reason keeps its original
    /// reason, because that one is the more informative of the two. A compositor
    /// that has no gamma protocol at all did not "refuse" anything.
    pub fn refuse_gamma(&mut self) {
        if self.gamma.is_available() {
            self.gamma = Capability::Unavailable(Unavailable::Refused);
        }
    }

    /// Whether any software dimming is possible at all.
    ///
    /// The overlay alone is enough: ADR-0003 makes it the primary mechanism and
    /// gamma the opt-in enhancement, so a session with a layer shell and no gamma
    /// protocol is fully dimmable.
    #[must_use]
    pub fn any_dimming(&self) -> bool {
        self.overlay.is_available() || self.gamma.is_available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(wayland: Option<&'static str>, x11: Option<&'static str>) -> SessionEnv<'static> {
        SessionEnv {
            wayland_display: wayland,
            wayland_socket: None,
            display: x11,
        }
    }

    /// The handed-over-socket shape: `WAYLAND_SOCKET` set, `WAYLAND_DISPLAY` not.
    fn handed_over(socket: Option<&'static str>, x11: Option<&'static str>) -> SessionEnv<'static> {
        SessionEnv {
            wayland_display: None,
            wayland_socket: socket,
            display: x11,
        }
    }

    /// `WAYLAND_SOCKET` does **not** make a session Wayland, and this pins the
    /// decision so the next person does not re-apply the obvious fix.
    ///
    /// D-093 is real - a client handed a pre-connected compositor fd has no
    /// `WAYLAND_DISPLAY`, so this answers X11 because Xwayland set `DISPLAY`.
    /// P8 wave 4 tried treating it as Wayland and a review showed it makes the
    /// session worse: the variable is **single-use** (`connect_to_env` takes the
    /// fd and `remove_var`s it), Duja opens four independent connections and
    /// re-reads the environment per decision, so the session became Wayland for
    /// exactly one call and X11 thereafter - with Wayland-format gamma tokens
    /// already stamped that the X11 arm rejects. Consistently-X11 at least works
    /// through Xwayland. See [`transport`]'s docs for the whole finding.
    #[test]
    fn a_handed_over_wayland_socket_is_not_treated_as_wayland() {
        assert_eq!(
            transport(handed_over(Some("4"), Some(":0"))),
            Transport::X11,
            "the one-shot fd was treated as a durable session marker"
        );
        // With no X11 at all there is nothing to fall back to, and answering
        // Wayland here would promise a connection only one caller can make.
        assert_eq!(transport(handed_over(Some("4"), None)), Transport::None);
    }

    /// The three variable **names**, pinned without touching the process
    /// environment. A typo in any of them compiles and passes everywhere, and
    /// for `DISPLAY` it would disable X11 dimming for every Linux user.
    #[test]
    fn session_env_vars_reads_the_three_names_it_means_to() {
        let asked = std::cell::RefCell::new(Vec::<String>::new());
        let vars = SessionEnvVars::from_lookup(|name| {
            asked.borrow_mut().push(name.to_owned());
            Some(format!("value-of-{name}"))
        });
        assert_eq!(
            asked.into_inner(),
            ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"]
        );
        let env = vars.as_session_env();
        assert_eq!(env.wayland_display, Some("value-of-WAYLAND_DISPLAY"));
        assert_eq!(env.wayland_socket, Some("value-of-WAYLAND_SOCKET"));
        assert_eq!(env.display, Some("value-of-DISPLAY"));
    }

    /// An unset variable stays `None` rather than becoming an empty string,
    /// which `transport`'s empty-is-unset rule would then have to un-do.
    #[test]
    fn session_env_vars_keeps_unset_apart_from_empty() {
        let vars = SessionEnvVars::from_lookup(|name| match name {
            "DISPLAY" => Some(String::new()),
            _ => None,
        });
        let env = vars.as_session_env();
        assert_eq!(env.wayland_display, None);
        assert_eq!(env.display, Some(""));
        assert_eq!(transport(env), Transport::None, "empty DISPLAY is not X11");
    }

    fn connected<'a>(globals: &'a [&'a str]) -> Probe<'a> {
        Probe {
            connected: true,
            globals,
            randr: true,
            compositor: true,
        }
    }

    /// An X11 probe with both extras present; individual tests knock one out.
    fn x11_probe(randr: bool, compositor: bool) -> Probe<'static> {
        Probe {
            connected: true,
            globals: &[],
            randr,
            compositor,
        }
    }

    /// Almost every Wayland session also runs Xwayland and sets `DISPLAY`.
    /// Preferring X11 there would put Duja on the compatibility layer, where it
    /// would see Xwayland's screen rather than the compositor's.
    #[test]
    fn wayland_wins_when_both_variables_are_set() {
        assert_eq!(
            transport(env(Some("wayland-0"), Some(":0"))),
            Transport::Wayland
        );
        assert_eq!(transport(env(None, Some(":0"))), Transport::X11);
        assert_eq!(transport(env(Some("wayland-1"), None)), Transport::Wayland);
        assert_eq!(transport(env(None, None)), Transport::None);
    }

    /// `DISPLAY=` is what a login shell is left with when a session script clears
    /// it. Treating an empty value as a server name turns "there is no display
    /// server" into "the display server refused the connection", which sends the
    /// user looking for a problem that does not exist.
    #[test]
    fn an_empty_variable_is_unset_rather_than_a_server_name() {
        assert_eq!(transport(env(Some(""), Some(""))), Transport::None);
        assert_eq!(transport(env(Some(""), Some(":0"))), Transport::X11);
        assert_eq!(
            transport(env(Some("wayland-0"), Some(""))),
            Transport::Wayland
        );
    }

    #[test]
    fn a_tty_session_reports_no_display_server_for_both_mechanisms() {
        let caps = resolve(env(None, None), &Probe::default());

        assert_eq!(caps.transport, Transport::None);
        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::NoDisplayServer)
        );
        assert_eq!(
            caps.gamma,
            Capability::Unavailable(Unavailable::NoDisplayServer)
        );
        assert!(!caps.any_dimming());
    }

    /// A named server that will not talk to us is a different state from no
    /// server, and the two have different remedies.
    #[test]
    fn a_failed_connection_is_not_the_same_as_no_server() {
        let probe = Probe {
            connected: false,
            globals: &[],
            randr: false,
            compositor: false,
        };
        let caps = resolve(env(Some("wayland-0"), None), &probe);

        assert_eq!(caps.transport, Transport::Wayland);
        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::ConnectFailed)
        );
        assert_ne!(
            caps.overlay,
            Capability::Unavailable(Unavailable::NoDisplayServer)
        );
    }

    /// The wlroots configuration this design exists for: both protocols present,
    /// both mechanisms available, no compositor named anywhere.
    #[test]
    fn a_compositor_offering_both_protocols_gets_both_mechanisms() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&[
                "wl_compositor",
                LAYER_SHELL,
                VIEWPORTER,
                XDG_OUTPUT,
                GAMMA_CONTROL,
                "wl_seat",
            ]),
        );

        assert_eq!(caps.overlay, Capability::Available);
        assert_eq!(caps.gamma, Capability::Available);
        assert!(caps.any_dimming());
    }

    /// Layer-shell gets the surface on screen; `wp_viewporter` is what puts a dim
    /// in it without allocating a framebuffer per output. The overlay arm needs
    /// both, and this is the half a registry check would otherwise miss — the
    /// backend would bind, fail, and contradict a report that had already told the
    /// user software dimming was available.
    #[test]
    fn a_layer_shell_with_no_way_to_scale_a_pixel_is_not_an_overlay() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&["wl_compositor", "wl_shm", LAYER_SHELL, GAMMA_CONTROL]),
        );

        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::ProtocolAbsent {
                interface: VIEWPORTER
            })
        );
        // And only the overlay arm: gamma does not go through a surface at all.
        assert_eq!(caps.gamma, Capability::Available);
    }

    /// A layer surface is created *on a `wl_output`*, and nothing above this layer
    /// speaks in outputs — it speaks in rectangles. Without `zxdg_output_manager_v1`
    /// no output has a logical rectangle, so there is no way to know which one a
    /// display is, and an overlay would be placed by registry order.
    #[test]
    fn a_compositor_that_will_not_say_where_its_outputs_are_cannot_place_one() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&["wl_compositor", "wl_shm", LAYER_SHELL, VIEWPORTER]),
        );

        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::ProtocolAbsent {
                interface: XDG_OUTPUT
            })
        );
    }

    /// The report and the backend bind the same three interfaces in the same
    /// order, which is what stops them naming different missing ones for one
    /// session. Asserted as a list rather than left to the two call sites, because
    /// the failure is a `dujactl doctor` line that contradicts what actually
    /// happened at startup.
    #[test]
    fn the_reported_order_is_place_then_fill_then_locate() {
        assert_eq!(
            super::WAYLAND_OVERLAY_INTERFACES,
            [LAYER_SHELL, VIEWPORTER, XDG_OUTPUT]
        );
    }

    /// Which one is named matters, because the sentence is printed. The registry
    /// below is a GNOME session: Mutter implements `wp_viewporter` and
    /// `zxdg_output_manager_v1` and no layer-shell, so naming either of the other
    /// two would send someone looking for a Mutter bug that is not there.
    #[test]
    fn a_gnome_session_is_told_about_the_shell_and_not_the_other_two() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&[
                "wl_compositor",
                "wl_shm",
                "xdg_wm_base",
                VIEWPORTER,
                XDG_OUTPUT,
            ]),
        );

        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::ProtocolAbsent {
                interface: LAYER_SHELL
            })
        );
    }

    /// The two are decided independently. Treating them as one capability would
    /// refuse the overlay on a compositor that has layer-shell and no
    /// gamma-control, which is a real and common configuration.
    #[test]
    fn layer_shell_without_gamma_control_still_dims() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&["wl_compositor", LAYER_SHELL, VIEWPORTER, XDG_OUTPUT]),
        );

        assert_eq!(caps.overlay, Capability::Available);
        assert_eq!(
            caps.gamma,
            Capability::Unavailable(Unavailable::ProtocolAbsent {
                interface: GAMMA_CONTROL
            })
        );
        // ADR-0003 makes the overlay primary and gamma the opt-in enhancement, so
        // this session is fully dimmable.
        assert!(caps.any_dimming());
    }

    /// And the other way round, which a name table would not have represented at
    /// all.
    #[test]
    fn gamma_control_without_layer_shell_is_representable() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&["wl_compositor", GAMMA_CONTROL]),
        );

        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::ProtocolAbsent {
                interface: LAYER_SHELL
            })
        );
        assert_eq!(caps.gamma, Capability::Available);
        assert!(caps.any_dimming());
    }

    /// The case the README's GNOME note is about — reported **without naming any
    /// compositor**, and it starts working the day that compositor ships either
    /// protocol, with no code change and no release.
    #[test]
    fn a_compositor_offering_neither_protocol_reports_why_without_being_named() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&["wl_compositor", "wl_seat", "xdg_wm_base"]),
        );

        assert!(!caps.any_dimming());
        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::ProtocolAbsent {
                interface: LAYER_SHELL
            })
        );
        // The reason is printable and names the interface, not the desktop.
        let reason = caps.overlay.clone();
        let Capability::Unavailable(reason) = reason else {
            panic!("expected unavailable");
        };
        let text = reason.to_string();
        assert!(text.contains(LAYER_SHELL), "{text}");
        assert!(!text.to_ascii_lowercase().contains("gnome"), "{text}");
    }

    /// The two X11 arms answer to different questions: the overlay to a
    /// compositing manager, gamma to `RandR`. Neither implies the other.
    #[test]
    fn x11_separates_the_composited_overlay_from_the_randr_gamma() {
        let both = resolve(env(None, Some(":0")), &x11_probe(true, true));
        assert_eq!(both.overlay, Capability::Available);
        assert_eq!(both.gamma, Capability::Available);

        let no_randr = resolve(env(None, Some(":0")), &x11_probe(false, true));
        assert_eq!(no_randr.overlay, Capability::Available);
        assert_eq!(
            no_randr.gamma,
            Capability::Unavailable(Unavailable::ExtensionAbsent { extension: RANDR })
        );
        // An X session with no RandR can still dim; only the ramp is gone.
        assert!(no_randr.any_dimming());
    }

    /// The defect this rule exists for. X11 does not blend an alpha channel;
    /// a compositing manager does. Duja's overlay is premultiplied black, so
    /// with no compositor every alpha renders identically — as opaque black
    /// over the whole monitor, with no visible way back.
    ///
    /// Reporting the overlay as available there would not degrade the feature,
    /// it would black out the screen the first time the user dragged a slider
    /// past the hardware floor.
    #[test]
    fn x11_without_a_compositing_manager_refuses_the_overlay() {
        let caps = resolve(env(None, Some(":0")), &x11_probe(true, false));

        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::NoCompositor)
        );
        // RandR is unaffected: a bare X session with no compositor can still
        // drive a gamma ramp, and that is then the only software dimming it has.
        assert_eq!(caps.gamma, Capability::Available);
        assert!(caps.any_dimming());
    }

    /// A bare X session with neither: no compositor and no `RandR`. Both arms
    /// unavailable, each with its own reason, and no software dimming at all.
    #[test]
    fn x11_with_neither_has_no_software_dimming_and_says_why() {
        let caps = resolve(env(None, Some(":0")), &x11_probe(false, false));

        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::NoCompositor)
        );
        assert_eq!(
            caps.gamma,
            Capability::Unavailable(Unavailable::ExtensionAbsent { extension: RANDR })
        );
        assert!(!caps.any_dimming());
    }

    /// `NoCompositor` is an X11 answer. A Wayland compositor *is* the compositing
    /// manager, so the flag must not be able to refuse a layer-shell session —
    /// otherwise a stray `false` from a probe that never asked the question would
    /// disable the overlay on the platform where it always works.
    #[test]
    fn the_compositor_flag_never_reaches_the_wayland_arm() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &Probe {
                connected: true,
                globals: &[LAYER_SHELL, VIEWPORTER, XDG_OUTPUT],
                randr: false,
                compositor: false,
            },
        );

        assert_eq!(caps.overlay, Capability::Available);
    }

    /// The reason is printed by `dujactl doctor`, so it has to name the missing
    /// thing — not a specific one of them (ADR-0011), and not a symptom the user
    /// cannot map back to a cause.
    #[test]
    fn the_no_compositor_reason_names_the_missing_thing() {
        let text = Unavailable::NoCompositor.to_string();

        assert!(text.contains("compositing manager"), "{text}");
        assert!(text.contains("blend"), "{text}");
        for name in ["picom", "xcompmgr", "compton"] {
            assert!(!text.to_ascii_lowercase().contains(name), "{text}");
        }
    }

    /// The atom name is the whole compositor check, and it is the only part of it
    /// a test on any lane can reach. A typo here reports "no compositing manager"
    /// on every X11 session forever, and Duja has no Linux machine to notice on.
    #[test]
    fn the_compositor_selection_is_the_spec_atom_for_the_screen() {
        assert_eq!(compositor_selection(0), "_NET_WM_CM_S0");
        // Not always zero: the number is the X screen, as in `DISPLAY=:0.1`.
        assert_eq!(compositor_selection(1), "_NET_WM_CM_S1");
        assert_eq!(compositor_selection(12), "_NET_WM_CM_S12");
    }

    /// An X server that will not talk to us reports `ConnectFailed` for both
    /// arms, not the two more specific reasons. Those would be answers to
    /// questions that were never asked, and they would send the user to install a
    /// compositor for a display server they cannot reach.
    #[test]
    fn a_failed_x11_connection_reports_neither_specific_reason() {
        let caps = resolve(
            env(None, Some(":0")),
            &Probe {
                connected: false,
                globals: &[],
                randr: false,
                compositor: false,
            },
        );

        assert_eq!(caps.transport, Transport::X11);
        assert_eq!(
            caps.overlay,
            Capability::Unavailable(Unavailable::ConnectFailed)
        );
        assert_eq!(
            caps.gamma,
            Capability::Unavailable(Unavailable::ConnectFailed)
        );
    }

    /// ADR-0011 step 5. A session running `wlsunset` advertises
    /// `zwlr_gamma_control_manager_v1` and still refuses the bind, so a report
    /// settled at startup would claim a gamma path Duja does not have.
    #[test]
    fn a_refused_bind_downgrades_gamma_after_startup() {
        let mut caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&[LAYER_SHELL, VIEWPORTER, XDG_OUTPUT, GAMMA_CONTROL]),
        );
        assert_eq!(caps.gamma, Capability::Available);

        caps.refuse_gamma();

        assert_eq!(caps.gamma, Capability::Unavailable(Unavailable::Refused));
        // The overlay is untouched: the refusal is per protocol, and ADR-0003's
        // primary mechanism is still there.
        assert_eq!(caps.overlay, Capability::Available);
        assert!(caps.any_dimming());
    }

    /// A second refusal must not change anything, and a refusal must never
    /// overwrite a *more informative* reason. A compositor with no gamma protocol
    /// at all did not refuse anything, and reporting it as a refusal would send
    /// the user hunting for the other client that is supposedly holding it.
    #[test]
    fn refusing_never_upgrades_and_never_overwrites_a_better_reason() {
        let mut refused = resolve(
            env(Some("wayland-0"), None),
            &connected(&[LAYER_SHELL, VIEWPORTER, XDG_OUTPUT, GAMMA_CONTROL]),
        );
        refused.refuse_gamma();
        let once = refused.clone();
        refused.refuse_gamma();
        assert_eq!(refused, once, "refusing twice changes nothing");

        let mut absent = resolve(
            env(Some("wayland-0"), None),
            &connected(&[LAYER_SHELL, VIEWPORTER]),
        );
        let before = absent.clone();
        absent.refuse_gamma();
        assert_eq!(
            absent, before,
            "a protocol that was never there cannot be refused"
        );
    }

    /// One sample of every [`Unavailable`] variant.
    ///
    /// Built from a `match` on a value rather than written out as a list, so a
    /// variant added later is a **compile error** here instead of a silent
    /// omission from the invariant below. The previous form was a bare array, and
    /// `NoCompositor` was added without anyone noticing it was missing.
    fn every_reason() -> Vec<Unavailable> {
        let all = [
            Unavailable::NoDisplayServer,
            Unavailable::ConnectFailed,
            Unavailable::ProtocolAbsent {
                interface: LAYER_SHELL,
            },
            Unavailable::ExtensionAbsent { extension: RANDR },
            Unavailable::NoCompositor,
            Unavailable::Refused,
        ];
        for reason in &all {
            // Exhaustive by construction: no wildcard arm, so a new variant
            // fails to compile until it is added to `all` above.
            match reason {
                Unavailable::NoDisplayServer
                | Unavailable::ConnectFailed
                | Unavailable::ProtocolAbsent { .. }
                | Unavailable::ExtensionAbsent { .. }
                | Unavailable::NoCompositor
                | Unavailable::Refused => {}
            }
        }
        all.to_vec()
    }

    /// Every reason a user can see must say something they can act on, and none
    /// of them may name a desktop — that is the whole decision.
    #[test]
    fn every_reason_is_printable_and_names_no_compositor() {
        let reasons = every_reason();
        for reason in reasons {
            let text = reason.to_string();
            assert!(!text.is_empty(), "{reason:?}");
            for desktop in ["gnome", "kde", "mutter", "kwin", "sway", "plasma"] {
                assert!(
                    !text.to_ascii_lowercase().contains(desktop),
                    "{reason:?} names {desktop}: {text}"
                );
            }
        }
    }
}

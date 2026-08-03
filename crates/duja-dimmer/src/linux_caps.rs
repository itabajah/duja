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
//! channel means nothing to the X server, which copies the window's contents to
//! the screen as they are; the channel is honoured only by a **compositing
//! manager** reading the window's off-screen pixmap and blending it. Duja's
//! overlay is premultiplied black, so its colour bytes are zero at *every* alpha
//! — 10% and 90% are the same pixels, and the difference between them lives
//! entirely in a byte only a compositor reads. Map that window with no compositor
//! running and the screen goes **solid black** at the first hint of dimming, with
//! Duja's own UI behind it and the only exit a keyboard the user cannot see.
//!
//! So the X11 overlay arm asks a second question: does a compositing manager own
//! the `_NET_WM_CM_S<n>` selection. That is the EWMH convention every compositor
//! follows to announce itself, and it is a live answer — a user who kills
//! `picom` mid-session stops being able to dim in software, which is exactly the
//! truth to report. It is also the same shape as the gamma arm: a capability that
//! presence alone cannot settle.

use std::fmt;

/// Which display server this session is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A Wayland compositor.
    Wayland,
    /// An X server (including Xwayland, which Duja neither detects nor needs to:
    /// an X client on Xwayland gets X11's mechanisms and they work).
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
    /// would draw the overlay's alpha channel as opaque black.
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

/// The interface a Wayland overlay needs.
pub const LAYER_SHELL: &str = "zwlr_layer_shell_v1";

/// The interface a Wayland gamma ramp needs.
pub const GAMMA_CONTROL: &str = "zwlr_gamma_control_manager_v1";

/// The X extension a gamma ramp needs.
pub const RANDR: &str = "RANDR";

/// The selection a compositing manager owns to announce itself, less the screen
/// number the caller appends.
///
/// EWMH names it `_NET_WM_CM_Sn` for screen `n`; on the overwhelmingly common
/// single-screen session that is `_NET_WM_CM_S0`, but the number is the *X screen*
/// (a separate root window, as in `DISPLAY=:0.1`), not a monitor, so it must come
/// from the connection rather than be hard-coded.
pub const COMPOSITOR_SELECTION_PREFIX: &str = "_NET_WM_CM_S";

/// The environment variables that name a display server.
///
/// Borrowed rather than owned so the caller can pass what `std::env::var` gave it
/// without allocating, and so a test can hand over literals.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionEnv<'a> {
    /// `WAYLAND_DISPLAY`.
    pub wayland_display: Option<&'a str>,
    /// `DISPLAY`.
    pub display: Option<&'a str>,
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
/// An **empty** value is treated as unset. That is not pedantry: `DISPLAY=` is
/// what a login shell leaves behind when a session script clears it, and treating
/// it as a display server name produces a connect failure reported as
/// "the display server refused the connection" instead of "there is no display
/// server", which sends the user looking for the wrong thing.
#[must_use]
pub fn transport(env: SessionEnv<'_>) -> Transport {
    if env.wayland_display.is_some_and(|v| !v.is_empty()) {
        Transport::Wayland
    } else if env.display.is_some_and(|v| !v.is_empty()) {
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
            overlay: from_registry(probe.globals, LAYER_SHELL),
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
            display: x11,
        }
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
            &connected(&["wl_compositor", LAYER_SHELL, GAMMA_CONTROL, "wl_seat"]),
        );

        assert_eq!(caps.overlay, Capability::Available);
        assert_eq!(caps.gamma, Capability::Available);
        assert!(caps.any_dimming());
    }

    /// The two are decided independently. Treating them as one capability would
    /// refuse the overlay on a compositor that has layer-shell and no
    /// gamma-control, which is a real and common configuration.
    #[test]
    fn layer_shell_without_gamma_control_still_dims() {
        let caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&["wl_compositor", LAYER_SHELL]),
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
                globals: &[LAYER_SHELL],
                randr: false,
                compositor: false,
            },
        );

        assert_eq!(caps.overlay, Capability::Available);
    }

    /// The reason is printed by `dujactl doctor`, so it has to say what to do
    /// about it without naming a compositor (ADR-0011).
    #[test]
    fn the_no_compositor_reason_explains_itself() {
        let text = Unavailable::NoCompositor.to_string();

        assert!(text.contains("compositing manager"), "{text}");
        assert!(text.contains("blend"), "{text}");
        for name in ["picom", "xcompmgr", "compton", "mutter", "kwin"] {
            assert!(!text.to_ascii_lowercase().contains(name), "{text}");
        }
    }

    /// ADR-0011 step 5. A session running `wlsunset` advertises
    /// `zwlr_gamma_control_manager_v1` and still refuses the bind, so a report
    /// settled at startup would claim a gamma path Duja does not have.
    #[test]
    fn a_refused_bind_downgrades_gamma_after_startup() {
        let mut caps = resolve(
            env(Some("wayland-0"), None),
            &connected(&[LAYER_SHELL, GAMMA_CONTROL]),
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
            &connected(&[LAYER_SHELL, GAMMA_CONTROL]),
        );
        refused.refuse_gamma();
        let once = refused.clone();
        refused.refuse_gamma();
        assert_eq!(refused, once, "refusing twice changes nothing");

        let mut absent = resolve(env(Some("wayland-0"), None), &connected(&[LAYER_SHELL]));
        let before = absent.clone();
        absent.refuse_gamma();
        assert_eq!(
            absent, before,
            "a protocol that was never there cannot be refused"
        );
    }

    /// Every reason a user can see must say something they can act on, and none
    /// of them may name a desktop — that is the whole decision.
    #[test]
    fn every_reason_is_printable_and_names_no_compositor() {
        let reasons = [
            Unavailable::NoDisplayServer,
            Unavailable::ConnectFailed,
            Unavailable::ProtocolAbsent {
                interface: LAYER_SHELL,
            },
            Unavailable::ExtensionAbsent { extension: RANDR },
            Unavailable::Refused,
        ];
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

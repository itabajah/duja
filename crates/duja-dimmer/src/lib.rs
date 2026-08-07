//! Software dimming: the fallback layer of Duja's brightness continuum.
//!
//! Primary mechanism (ADR-0003): a per-monitor, borderless, always-on-top,
//! **click-through** overlay window with variable-alpha black fill — the only
//! technique that reaches true black on every OS, survives HDR, and does not
//! disturb the gamma ramp. An opt-in gamma-ramp backend exists only where
//! verified safe (never under HDR) and is engaged through a separate, explicit
//! API so a routine [`Dimmer::apply`] never touches gamma.
//!
//! # Architecture
//!
//! - [`plan`] is the pure diffing kernel: it turns a desired [`DimCommand`] set
//!   into the minimal [`plan::OverlayOp`] list, given the current overlays. It
//!   is OS-free and exhaustively unit-tested on every target.
//! - On Windows, `WindowsDimmer` owns a dedicated thread that holds every
//!   overlay window and its message loop (spawn → HWND-ready handshake,
//!   shutdown → destroy windows → join; `Drop` shuts down). `apply` diffs with
//!   [`plan`] and executes the ops on that thread.
//! - On macOS, `MacDimmer` cannot own a thread: `AppKit` windows may only be
//!   touched on the main thread, which in `duja-app` runs Slint's (winit's)
//!   `NSApplication` loop. `apply` diffs with [`plan`] on the calling thread,
//!   then marshals the resulting overlay ops onto the **main dispatch queue**
//!   (`dispatch_async`); the windows live in a main-thread store. See the `mac`
//!   module docs for the observable-contract difference (non-blocking vs the
//!   Windows blocking `apply`) and the running-run-loop requirement.
//! - On Linux, `LinuxDimmer` picks a backend at **runtime** rather than at build
//!   time, because Linux has no single windowing system. An X11 session gets
//!   `X11Dimmer`, which owns a thread like the Windows one and holds an
//!   override-redirect ARGB window per display, plus a second thread watching the
//!   compositing-manager selection. A Wayland session gets `WaylandDimmer`, one
//!   thread again, holding a `zwlr_layer_shell_v1` surface per dimmed **output** —
//!   which is the difference that shapes it, since a layer surface is bound to an
//!   output rather than placed at a rectangle, and the compositor rather than Duja
//!   decides how big it is. A session with no overlay mechanism reports
//!   `Unsupported` and the app disables software dimming with hardware control
//!   intact. The opt-in gamma channel splits the same way and is chosen the same
//!   way: `RandR`'s per-CRTC transfer table on X11, `zwlr_gamma_control_v1`'s
//!   per-output table on Wayland. Neither is ever used on the other's session —
//!   an `XRandR` ramp on a Wayland session would land on Xwayland's virtual CRTCs,
//!   be accepted, and change nothing.
//! - On other Unix targets, `StubDimmer` records-and-succeeds so higher layers
//!   can run their logic unchanged (documented no-op).
//!
//! # Security invariant
//!
//! Overlays must **never** intercept input. On Windows every overlay carries
//! `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE` and answers `WM_NCHITTEST` with
//! `HTTRANSPARENT`; on macOS every overlay sets `ignoresMouseEvents = true`; on
//! X11 every overlay's `XFixes` **input region is empty**, so the server routes
//! every event to what is beneath. SHAPE's `ShapeInput` expresses the same thing,
//! so `XFixes` is the mechanism this uses rather than the only one there is — and
//! the backend refuses to start without it rather than mapping a window that
//! would swallow every click. On Wayland it is an empty `wl_region` set as the
//! surface's input region, applied **before the first commit** so the surface is
//! never up without it; `set_keyboard_interactivity(none)` is the keyboard half
//! and covers nothing else.
//! Fullscreen-exclusive apps and the OS secure/login screens are documented
//! known-limits on all three (an overlay cannot cover them).
//!
//! # Crash safety
//!
//! Overlay windows die with the process. A Windows gamma ramp **persists** after
//! death, so `ScreenStateGuard` restores identity gamma on drop (including panic
//! unwind) and a marker file lets a fresh start detect a dirty exit and call
//! `restore_all`. macOS is different: the window server restores each process's
//! gamma automatically when the process exits, so the macOS backend needs **no**
//! marker machinery and its `restore_all` is a single
//! `CGDisplayRestoreColorSyncSettings` call.
//!
//! Linux takes three answers rather than one, and which of the two gamma answers
//! applies is a property of the session rather than of the build. Overlays need no
//! marker on either transport:
//! an X11 override-redirect window is owned by the connection and a
//! `zwlr_layer_shell_v1` surface by the compositor, and both die with the socket.
//! The **gamma** channels are where the two transports part company, and they land
//! on opposite sides of Windows and macOS:
//!
//! - **X11 sits with Windows.** The X server holds each CRTC's table and does not
//!   reset it when the client that wrote it disconnects, which is exactly why
//!   `xrandr --output DP-1 --gamma 1:1:0.5` works as a one-shot command. So it
//!   needs the same guard Windows carries — which it does not have yet,
//!   deliberately, because nothing on Linux engages a ramp until the tray does.
//!   `restore_all` and `duja --restore` are the manual rescue in the meantime.
//! - **Wayland sits with macOS.** A `zwlr_gamma_control_v1` ramp lives exactly as
//!   long as the client's object, the compositor destroys every object a client
//!   holds when its socket closes, and destroying the object drops the dim — so
//!   the recovery is automatic and survives `SIGKILL`. There is nothing for a
//!   rescue pass to find and no marker to write. What comes back is the output's
//!   *default*, not some other client's curve: the compositor keeps no such table,
//!   and this protocol cannot read one either.
//!
//! See `src/linux/gamma.rs`, `src/linux/wlr_gamma.rs` and `docs/debt.md`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod plan;

// Pure geometry for the macOS overlay backend. OS-free, so it compiles and is
// tested on every target (like `plan`); only the `mac` backend calls it.
mod mac_geom;

// The pure half of ADR-0011: environment and Wayland-registry contents in, a
// software-dimming capability report out. Public, because `dujactl doctor` and
// the app's disclosure both consume it, and unconditional for the same reason
// `plan` and `mac_geom` are: it names no platform type, so there is nothing to
// gate. Its tests are the largest falsifiable surface Linux dimming has — real
// X11 windows and real `wl_surface`s cannot run on a headless runner.
pub mod linux_caps;

// The other pure Linux half: matching the connectors sysfs found to the outputs
// the display server enumerated, which is how a Linux display acquires the
// rectangle neither `duja-ddc` nor `duja-panel` can supply. Unconditional and
// display-server-free for the same reason `linux_caps` is — it names no `x11rb`
// or `wayland-client` type, so its rules (name first, EDID as the documented
// fallback, ambiguity refused) are tested on all three lanes.
pub mod linux_outputs;

// The three decisions an X11 overlay window makes that are arithmetic rather than
// windowing: which visual it needs, what pixel it is filled with, and whether its
// rectangle fits X11's 16-bit geometry at all. Unconditional and `x11rb`-free for
// the same reason as its two neighbours — each of them fails invisibly, so each
// belongs where every lane can test it.
pub mod linux_overlay;

// The four decisions an XRandR gamma ramp makes that are arithmetic rather than
// X11: whether the session has a gamma channel at all, which CRTC a token names,
// the table itself, and whether a ramp means anything here. Unconditional and
// `x11rb`-free for the same reason as its three neighbours.
pub mod linux_gamma;

// The decisions a Wayland layer-shell overlay makes that are data rather than
// windowing: which layer it sits in, what it refuses to be moved for, whether its
// omitted size is legal for its anchor, and whether clicks pass through it.
// Unconditional and `wayland-client`-free for the same reason as its neighbours,
// and with a sharper edge than most — two of these four are protocol errors,
// which terminate the connection rather than degrading.
pub mod linux_layer;

// The four decisions a `zwlr_gamma_control_v1` ramp makes that are arithmetic
// rather than Wayland: whether the session has this channel at all, how long the
// table is, how many bytes that is, and what those bytes are. Unconditional for
// the same reason as its neighbours, and with the sharpest edge of any of them —
// the compositor reads a fixed byte count and answers a short one by **killing
// the client connection**, which would take the layer-shell overlay down with the
// ramp.
pub mod linux_wlr_gamma;

// Everything in this crate that talks to a Linux display server. Nothing it does
// can run in CI, which is exactly why each of its modules is this small.
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{
    GammaDisplay, LinuxDimmer, WaylandDimmer, X11Dimmer, display_supports_gamma,
    enumerate_gamma_displays, enumerate_outputs, is_hdr_active, probe_session, restore_all,
    restore_identity, set_gamma,
};

// The gamma path's cross-platform vocabulary: the HDR verdict and its safety rule,
// and the shape of a restore pass's report. Unconditional — the rule is the same
// everywhere and only the probe behind it is per-platform — so it is tested on all
// three CI lanes rather than once per backend.
mod gamma_support;

pub use gamma_support::{GammaSupport, RestoreReport, gamma_support_from_hdr};

// The gamma crash marker. Unconditional, and it did not start that way: it lived
// in `win::gamma` while Windows was the only platform whose gamma ramp outlives
// the process. X11's does too, so the Linux gamma sink writes the same marker and
// `startup::recover_from_crash_marker` reads it on both. macOS still writes none.
mod marker;

pub use marker::{clear_marker, mark_dirty, marker_present};

// Re-export the cross-platform vocabulary so callers can depend on this crate
// alone for the dimming surface.
pub use duja_core::dimmer::{
    DimCommand, Dimmer, DimmerError, DisplayBounds, GAMMA_FLOOR, clamp_alpha, clamp_gamma,
};

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{
    GammaDisplay, GammaRamp, ScreenStateGuard, WindowsDimmer, display_supports_gamma,
    enumerate_gamma_displays, is_hdr_active, restore_all, restore_identity, set_gamma,
};

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
pub use mac::{
    GammaDisplay, MacDimmer, display_supports_gamma, enumerate_gamma_displays, is_hdr_active,
    restore_all, restore_identity, set_gamma,
};

#[cfg(not(any(windows, target_os = "macos")))]
mod stub;

#[cfg(not(any(windows, target_os = "macos")))]
pub use stub::StubDimmer;

/// The concrete [`Dimmer`] for the current platform: `WindowsDimmer` on Windows,
/// `MacDimmer` on macOS, `StubDimmer` elsewhere. Callers that want the native
/// backend without a `cfg` write `PlatformDimmer`.
#[cfg(windows)]
pub type PlatformDimmer = WindowsDimmer;

/// The concrete [`Dimmer`] for the current platform (macOS overlay backend).
#[cfg(target_os = "macos")]
pub type PlatformDimmer = MacDimmer;

/// The concrete [`Dimmer`] for a Linux session.
///
/// Not a type alias to one backend, unlike the other two platforms: Linux has no
/// single windowing system, so which mechanism is available is a property of the
/// session rather than of the build. [`LinuxDimmer`] picks when it starts.
#[cfg(target_os = "linux")]
pub type PlatformDimmer = LinuxDimmer;

/// The concrete [`Dimmer`] for a Unix target that is neither macOS nor Linux
/// (the documented no-op stub).
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub type PlatformDimmer = StubDimmer;

/// The lowest gamma factor **this platform's OS** will actually accept.
///
/// A *platform* limit, distinct from [`GAMMA_FLOOR`] — Duja's own cross-platform
/// safety floor on how dark a ramp it is willing to ask for. A caller that wants a
/// factor below this will get a refusal rather than a darker screen, so it must
/// ask **before** attempting the write and realise that part of the range some
/// other way (ADR-0003's overlay is the answer, and `duja-app`'s `dimming::plan`
/// does exactly that).
///
/// On Windows this is `MIN_ACCEPTED_GAMMA` (`0.5`). Microsoft's
/// [Using gamma correction](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/using-gamma-correction)
/// states the rule: *"any entry in the ramp must be within 32768 of the identity
/// value"*, so no application can blank the display. See that constant for the
/// derivation and the hardware measurement that confirms it. `GAMMA_FLOOR` is
/// therefore **unreachable** on Windows.
///
/// Staying at or above this bound is also the only *reliable* protection, because a
/// refusal is not always reported: the same API is documented as able to *"fail
/// silently (that is, it returns TRUE, but it doesn't set your ramp)"*. See
/// `docs/debt.md`.
#[cfg(windows)]
#[must_use]
pub fn min_gamma_factor() -> f32 {
    win::MIN_ACCEPTED_GAMMA
}

/// The lowest gamma factor **this platform's OS** will actually accept.
///
/// A *platform* limit, distinct from [`GAMMA_FLOOR`] — Duja's own cross-platform
/// safety floor on how dark a ramp it is willing to ask for. Off Windows there is
/// no additional OS restriction to report, so this is `GAMMA_FLOOR` itself and no
/// caller ever needs to substitute an overlay for a refused ramp:
///
/// - **macOS**: `CGSetDisplayTransferByFormula` takes an arbitrary
///   `(min, max, gamma)` triple — it will happily accept `max = 0` and black the
///   display out — so it imposes no anti-lockout clamp of its own. (The reason
///   that is safe on macOS and not on Windows is crash behaviour, not validation:
///   the window server restores a process's transfer tables when it exits, so a
///   crashed ramp self-heals.)
/// - **Linux**: neither channel validates the values. `RandR`'s `SetCrtcGamma`
///   accepts a table of zeroes, because `ProcRRSetCrtcGamma` checks only that the
///   table length matches the CRTC's, and `zwlr_gamma_control_v1.set_gamma`
///   likewise checks only the byte count. So [`GAMMA_FLOOR`] is the only floor
///   there is on either, and it is genuinely reachable. (An earlier draft cited
///   `xgamma -gamma 0` here. That is the wrong evidence twice over: `xgamma`
///   drives XFree86-VidModeExtension rather than either API, and it bounds its own
///   argument below at 0.1.)
///
///   Whether that is paired with an OS safety net depends on the transport, not on
///   Linux. An **X11** ramp outlives the process that set it (see the crate docs),
///   so there is no net and `clamp_gamma` is load-bearing. A **Wayland** ramp is
///   undone when the client's gamma-control object dies, which the compositor does
///   when the socket closes — that *is* the macOS situation, so a too-dark ramp
///   there self-heals on exit. `clamp_gamma` still applies to both, because
///   self-healing on exit is no comfort to someone looking at a black screen.
/// - **Other targets**: there is no gamma backend at all, so the value is inert.
#[cfg(not(windows))]
#[must_use]
pub fn min_gamma_factor() -> f32 {
    GAMMA_FLOOR
}

/// Whether this platform's gamma write can report success without applying the
/// ramp, in a way the caller **cannot pre-empt**.
///
/// Not "can a gamma write ever silently fail here" — both supported platforms
/// document or evidence some such mode. The question is whether there is a rule the
/// caller can satisfy to stay out of it, because that is what decides whether Duja
/// can engineer the hazard away or has to disclose it.
///
/// On **Windows** this is `false`. The failure is documented with a trigger:
/// [`SetDeviceGammaRamp`](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-setdevicegammaramp)
/// *"implements heuristics to check whether a provided ramp will result in an
/// unreadable screen. **If a ramp violates those heuristics**, then the function
/// fails silently"* — and the heuristic is quantified elsewhere as "any entry in
/// the ramp must be within 32768 of the identity value". `MIN_ACCEPTED_GAMMA` is
/// derived from exactly that bound and pinned *at* it by `win/gamma.rs`'s tests,
/// and `duja-app`'s planner substitutes an overlay below it, so every ramp Duja
/// sends complies by construction. That is what its own doc means by "staying at or
/// above it is the only reliable protection". A residual remains — a driver or GPU
/// with a tighter, undocumented rule (`docs/debt.md`) — but it is a hardware
/// unknown, not a mode the platform has.
///
/// On **macOS** this is `true`, and the difference is mechanism rather than
/// likelihood. `CGSetDisplayTransferByFormula` is reported to return
/// `kCGErrorSuccess` while leaving the display's curve unchanged, on *valid*
/// triples, in two distinct situations that are deliberately not ranked against
/// each other:
///
/// - Apple DTS reproduced a **total** no-visual-change on an M5 Max under macOS
///   26.3.1 (25D2128), through both the Table and Formula entry points and on both
///   built-in and external displays, with **no setting involved** — and confirmed
///   an M5 non-Max on the same build is unaffected, so it is narrow, recent
///   hardware rather than the platform at large. The same failure is *reported*
///   (by the filer, not reproduced by DTS) on M5 Pro and Neo.
/// - Separately, the ramp is reported not to apply while "Automatically adjust
///   brightness" is on.
///
/// There is no rule to comply with, and readback cannot detect it either: DTS
/// confirmed `CGGetDisplayTransferByTable` returns the values just written while
/// the screen is unchanged. See `docs/debt.md`.
#[cfg(windows)]
#[must_use]
pub fn gamma_is_advisory() -> bool {
    false
}

/// Whether this platform's gamma write can report success without applying the
/// ramp, in a way the caller **cannot pre-empt**. See the Windows arm for the full
/// contract; the short version is that this asks whether the hazard can be
/// engineered away, not whether it exists.
///
/// - **macOS**: `true`. `CGSetDisplayTransferByFormula` is reported to return
///   `kCGErrorSuccess` while leaving the curve unchanged, on *valid* triples —
///   DTS-reproduced on an M5 Max with no setting involved (and *reported* on M5
///   Pro and Neo), and separately reported while "Automatically adjust brightness"
///   is on. No rule exists to comply with, and `CGGetDisplayTransferByTable`
///   returns the written values while the screen is unchanged, so verification by
///   readback does not detect it either.
/// - **Linux (Wayland)**: `true`, and the companion bullet below says why X11 is
///   too. The function cannot tell them apart, because it is chosen per target
///   rather than per session. Here the reason is **timing rather than silence** — on
///   the versions where it is a reason at all. A driver that refuses an
///   otherwise-valid LUT *is* reported; an earlier draft of this bullet said it was
///   not, and named an API (`wlr_output_set_gamma`) that wlroots removed in 0.18.
///
///   Where it is reported from moved. On wlroots **0.16 and earlier**,
///   `gamma_control_handle_set_gamma` calls `gamma_control_apply` inline, which
///   runs `wlr_output_test` and sends `failed` **before the request handler
///   returns** — so the refusal precedes the `done` of Duja's confirming round trip
///   and that round trip catches it. On **0.17 and later** the test moved to the
///   scene layer (`scene_output_state_attempt_gamma`, which sends `failed` when
///   `wlr_output_test_state` rejects the LUT) and runs on a later **output
///   commit** — after `set_gamma` has already returned `Ok`. There the caller has
///   recorded a live ramp by the time the refusal arrives, with no rule it could
///   have satisfied to avoid it.
///
///   So `true` is right for current wlroots and conservative for older ones. The
///   verdict does not turn on it either way: the X11 ground below is unconditional,
///   and this function is chosen per target rather than per session.
/// - **Linux (X11)**: `true`, and for a reason read out of the X server's source
///   rather than inferred. `ProcRRSetCrtcGamma` ends
///   `RRCrtcGammaSet(crtc, red, green, blue); return Success;` — it **discards**
///   that call's `Bool`, and `RRCrtcGammaSet` returns exactly the driver hook's
///   result (`ret = (*pScrPriv->rrCrtcSetGamma)(pScreen, crtc)`). So a write the
///   KMS driver refused is reported to the client as `Success`. Readback does not
///   detect it either: `RRCrtcGammaSet` `memcpy`s into `crtc->gammaRed` first, and
///   `GetCrtcGamma` answers from that server-side copy.
///
///   Duja does not need a driver to fail to reach this state, which is what makes
///   it a property rather than a hazard: `restore_all` deliberately writes to
///   CRTCs that are driving nothing, and a CRTC with no mode has no pipeline to
///   program. It is stored, `Success` is returned, and the table is re-applied if
///   that CRTC is ever enabled — which is the behaviour the rescue *wants*, and is
///   also a write that provably did not reach a screen.
///
///   Like macOS, there is no rule to comply with. `ProcRRSetCrtcGamma` checks the
///   request's wire encoding and that `stuff->size == crtc->gammaSize`, and
///   nothing about the ramp's **values** — so unlike Windows there is no bound to
///   stay inside, and this cannot be engineered away, which is the question this
///   function asks.
/// - **Other targets**: `false` — there is no gamma backend at all, so nothing can
///   silently fail.
#[cfg(not(windows))]
#[must_use]
pub fn gamma_is_advisory() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_against_core() {
        assert_eq!(version(), duja_core::version());
    }

    #[test]
    fn min_gamma_factor_is_never_below_the_safety_floor() {
        // A platform whose OS accepted *less* than `GAMMA_FLOOR` would still not
        // get it: `clamp_gamma` bounds every factor at the floor first, so the
        // effective minimum can only ever be at or above it.
        assert!(min_gamma_factor() >= GAMMA_FLOOR);
    }

    #[cfg(windows)]
    #[test]
    fn windows_cannot_reach_the_cross_platform_gamma_floor() {
        // The whole reason the app needs an overlay fallback: on Windows the OS
        // refuses the bottom of the gamma range outright, so `GAMMA_FLOOR` is not
        // a level the ramp can deliver.
        assert!(
            min_gamma_factor() > GAMMA_FLOOR,
            "Windows caps the ramp above GAMMA_FLOOR; the planner must substitute an overlay"
        );
    }

    #[test]
    fn a_platform_with_a_pre_emptable_failure_mode_is_not_advisory() {
        // The two verdicts are not "does gamma ever silently fail" — Windows
        // documents such a mode too. They are "is there a rule the caller can
        // satisfy", because only that decides whether Duja engineers the hazard
        // away or discloses it. Windows has one and `min_gamma_factor()` *is* it;
        // macOS has none.
        //
        // The pairing is asserted rather than the two constants separately, so
        // flipping either verdict without the other reds: a Windows `true` would
        // put a hazard caption in front of users on a path this crate's own
        // `MIN_ACCEPTED_GAMMA` tests prove compliant, and a macOS `false` would
        // ship the reported silent no-op undisclosed.
        #[cfg(windows)]
        {
            assert!(!gamma_is_advisory());
            assert!(
                min_gamma_factor() > GAMMA_FLOOR,
                "Windows is non-advisory *because* it has a satisfiable bound; if that \
                 bound ever stopped existing, the verdict would have to change with it"
            );
        }
        #[cfg(target_os = "macos")]
        {
            assert!(
                gamma_is_advisory(),
                "macOS accepts the whole range and can still not apply it — the settings \
                 window has to say so"
            );
            assert!(
                (min_gamma_factor() - GAMMA_FLOOR).abs() < f32::EPSILON,
                "no OS bound to comply with is exactly why the verdict is advisory"
            );
        }
        #[cfg(target_os = "linux")]
        {
            assert!(
                gamma_is_advisory(),
                "the X server discards the driver's gamma result and answers \
                 Success regardless, so a write can be accepted and never reach \
                 a screen"
            );
            assert!(
                (min_gamma_factor() - GAMMA_FLOOR).abs() < f32::EPSILON,
                "RandR validates only the table length, which is why the verdict \
                 is advisory and why GAMMA_FLOOR is the only floor there is"
            );
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        assert!(!gamma_is_advisory(), "no gamma backend, nothing to fail");
    }
}

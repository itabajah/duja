# 0011 — Linux software dimming: probe the compositor, never assume it

- Status: accepted
- Date: 2026-08-01

> The ADR index listed this as *"pending (P7 spike)"*. **No spike was run**, and
> one would not have helped: a spike needs a Linux machine and several desktops to
> be worth anything, and the project has neither. What replaced it is a decision
> that does not require knowing the answer — probe the session instead of encoding
> a belief about it — which is the shape a spike would have been used to justify.

## Context

ADR-0003 makes a click-through black overlay the primary software-dimming
mechanism everywhere, with an opt-in gamma ramp where it is verified safe. On
Windows that is one API each. On Linux there is no "Linux": the mechanism depends
on the display server and, under Wayland, on protocols each compositor chooses
independently.

The shape of the problem, as the plan and README have carried it since P0, is that
**GNOME under Wayland offers no third-party path to either mechanism** while
hardware control (DDC/CI, backlight) still works. That framing is right but it
invites the wrong implementation: a table of compositor names.

A name table is the wrong design here for reasons that do not depend on which
compositor supports what today:

- It is a claim about third-party software that Duja cannot verify and that goes
  stale silently. This project has already shipped a whole macOS DDC backend where
  *every request was malformed* and the suite stayed green (`#106`); a hardcoded
  compatibility table has the same failure signature — confidently wrong, and
  green.
- `XDG_CURRENT_DESKTOP` is set by session scripts, forged by nesting, absent under
  bare `Xwayland`, and lists multiple values. Identity is not capability.
- It cannot represent the case that actually matters most in practice: a
  compositor that gains support in a later release. A table would keep refusing.

The capabilities Duja actually needs are directly observable at runtime:

- **X11** — an override-redirect, input-transparent window (the overlay) and
  XRandR's per-CRTC gamma. Availability is "did the X connection and the extension
  query succeed".
- **Wayland** — the overlay needs `zwlr_layer_shell_v1`, and the gamma path needs
  `zwlr_gamma_control_manager_v1`. Both are **advertised in the `wl_registry`
  globals**, which every client enumerates at connect time. Availability is "is
  this interface in the registry".

That last point is what makes the decision easy: the Wayland protocol is designed
so a client asks what the compositor offers, and the answer is authoritative,
per-session, and free.

## Decision

**Detect by capability, never by compositor identity.**

`duja-dimmer`'s Linux backend resolves, at startup and on session change, a
`LinuxSurface` describing what this session can actually do:

1. Choose the transport from `WAYLAND_DISPLAY` / `DISPLAY`, preferring Wayland
   when both are present, and treating a failed connect as "not that one".
2. On Wayland, bind the registry and record whether `zwlr_layer_shell_v1` and
   `zwlr_gamma_control_manager_v1` are present, independently of each other.
3. On X11, record whether the connection succeeded and whether the RandR extension
   answered.
4. Report each of overlay and gamma as available or not, with the reason.

Session-type and compositor strings are read **for the diagnostic and the log
line only** — never as an input to the availability decision. `dujactl doctor`
(wave 5) prints what was found and why, which is the honest version of the
compatibility table: derived from this session rather than from a maintainer's
belief about a desktop.

The pure mapping — environment and registry contents in, capability report out —
lives in a `#[cfg(any(test, target_os = "linux"))]` module so it is unit-tested on
**every** CI lane, in the pattern `mac_events`, `mac_geom` and `correlate` already
follow. That is deliberately the largest testable surface this feature has,
because the parts below it (real X11 windows, real `wl_surface`s) cannot run on a
headless runner at all.

## Consequences

- **GNOME Wayland reports "software dimming unavailable" without being named.**
  Mutter advertises neither `wlr` protocol, so the registry probe produces that
  answer on its own. If Mutter ever adds them, or a user runs a compositor that
  already has them, Duja works with no code change and no release.
- **Every combination is representable**, including ones a table would not
  anticipate: layer-shell without gamma-control (KWin's Wayland session is exactly
  this shape), gamma without an overlay, and X11 sessions on a compositor whose
  Wayland session supports neither.
- **Hardware control is unaffected and must be said so in the UI.** A display with
  DDC or a backlight is fully controllable on GNOME Wayland; only the sub-floor
  software layer is missing. The capability report is per-mechanism precisely so
  the flyout can disclose that rather than implying Duja does not work.
- **The disclosure has an existing shape to match.** `#103` settled that a
  platform limit is disclosed by a caption that appears only where the limit
  applies, gated on a plumbed value rather than a hardcoded string. The Linux
  overlay-unavailable case is the same problem and should reuse it rather than
  invent a second convention.
- **This decides detection, not implementation.** The X11 and Wayland backends are
  waves 4a and 4b, and each will need its own protocol plumbing; `wlr-layer-shell`
  and `wlr-gamma-control` are not in `wayland-protocols` proper and the crate
  choice for them is deferred to that wave, when the code that consumes them
  exists to judge it.
- **Untestable end-to-end here.** No Linux hardware, and a headless runner has
  neither an X server nor a compositor. CI can prove the mapping and the refusal
  logic; it cannot prove an overlay appears. Ships 🧪 until community confirmation.

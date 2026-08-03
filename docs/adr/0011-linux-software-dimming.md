# 0011 — Linux software dimming: probe the compositor, never assume it

- Status: accepted
- Date: 2026-08-01

> The ADR index listed this as *"pending (P7 spike)"* and `docs/STATUS.md` §3 said,
> before this change, *"GNOME Wayland dimming spike first"*.
> **No spike was run before this decision**,
> and that is a deliberate reordering rather than a skipped step: a spike answers
> "what does compositor X support today", and this ADR's whole point is that Duja
> should never encode that answer. The spike is still worth running — on the
> VM/WSL environment STATUS.md §3 anticipates — but as *verification of the probe*,
> against real compositors, not as an input to the decision. STATUS.md is updated
> to say so rather than left contradicting this.

## Context

ADR-0003 makes a click-through black overlay the primary software-dimming
mechanism everywhere, with an opt-in gamma ramp where it is verified safe. On
Windows that is one API each. On Linux there is no "Linux": the mechanism depends
on the display server and, under Wayland, on protocols each compositor chooses
independently.

The shape of the problem, as the plan and README have carried it since P0, is that
**GNOME under Wayland offers no third-party path to either mechanism** while
hardware control (DDC/CI, backlight) still works. That framing is widely reported
and is what those documents carry; this ADR neither confirms it nor rests on it.
What matters here is that it invites the wrong implementation: a table of
compositor names.

A name table is the wrong design here for reasons that do not depend on which
compositor supports what today:

- It is a claim about third-party software that Duja cannot verify and that goes
  stale silently. This project has already shipped a macOS DDC backend whose
  **Apple Silicon arm** malformed every request while the suite stayed green
  (`#106`); a hardcoded compatibility table has the same failure signature —
  confidently wrong, and green.
- `XDG_CURRENT_DESKTOP` is set by the session, so it is unset wherever no session
  script set it (TTY launches, bare compositors, containers), inherited unchanged
  into nested sessions, and colon-separated multi-valued. Identity is not
  capability.
- It cannot represent the case that actually matters most in practice: a
  compositor that gains support in a later release. A table would keep refusing.

The capabilities Duja actually needs are directly observable at runtime:

- **X11** — an override-redirect, input-transparent window (the overlay) and
  XRandR's per-CRTC gamma. Availability is "did the X connection and the extension
  query succeed".
- **Wayland** — the overlay needs `zwlr_layer_shell_v1`, and the gamma path needs
  `zwlr_gamma_control_manager_v1`. Both are **advertised in the `wl_registry`
  globals**, which every client enumerates at connect time, so a compositor that
  does not implement one cannot be mistaken for one that does.

That is what makes the decision easy: the Wayland protocol is designed so a client
asks what the compositor offers rather than guessing, and it answers per session,
at connect time, for free.

**Registry presence is not the whole answer for gamma, and the design must not
pretend it is.** `wlr-gamma-control-unstable-v1` describes itself as a protocol
"for a **privileged** client", allows at most one gamma control per output with
**exclusive** access, and defines — on the per-output `zwlr_gamma_control_v1`
that `get_gamma_control` returns, not on the manager global a client binds from
the registry — a `failed` event whose listed reasons include "the output doesn't
support gamma tables", "setting the gamma tables failed", and "another client
already has exclusive gamma control for this output". So a session running `wlsunset` or
`gammastep` advertises the global and still refuses Duja — which is not a corner
case, it is the commonest reason a Wayland user would have that protocol at all.
Presence is therefore **necessary and not sufficient** for gamma: the honest
capability is only known once `get_gamma_control` has been taken without a
`failed` reply. Layer-shell carries no equivalent wording; for the overlay,
presence is the answer.

## Decision

**Detect by capability, never by compositor identity.**

`duja-dimmer`'s Linux backend resolves, at startup, on session change, and on a
refused gamma bind, a `LinuxSurface` describing what this session can actually
do:

1. Choose the transport from `WAYLAND_DISPLAY` / `DISPLAY`, preferring Wayland
   when both are present, and treating a failed connect as "not that one".
2. On Wayland, bind the registry and record whether `zwlr_layer_shell_v1` and
   `zwlr_gamma_control_manager_v1` are present, independently of each other.
3. On X11, record whether the connection succeeded and whether the RandR extension
   answered.
4. Report each of overlay and gamma as available or not, with the reason —
   overlay for the session, **gamma per output**, because that is the grain the
   protocol grants and refuses it at.
5. **Gamma only: downgrade the report if the bind is refused.** A
   `zwlr_gamma_control_v1::failed` for an output moves that output's gamma from
   available to unavailable, with the reason, and that has to reach the same
   capability report step 4 produced — so the report is a value that can change
   after startup, not one settled once. This is the same rule one layer lower
   than two Duja already applies: `#96` substitutes an overlay when a gamma ramp
   is refused, and `#109` drops a refused record rather than latching it.
   Building the report as write-once would make this unimplementable.

Session-type and compositor strings are read **for the diagnostic and the log
line only** — never as an input to the availability decision. `dujactl doctor`
(wave 5) prints what was found and why, which is the honest version of the
compatibility table: derived from this session rather than from a maintainer's
belief about a desktop.

The pure mapping — environment and registry contents in, capability report out —
lives in a `#[cfg(any(test, target_os = "linux"))]` module so it is unit-tested on
**every** CI lane. That is the gating `duja-platform`'s `mac_events` and
`duja-ddc`'s `correlate` use (`#[cfg(any(test, target_os = "macos"))]` and
`#[cfg(any(windows, test))]` respectively). Where a module has no FFI at all the
project goes further and compiles it unconditionally — `duja-dimmer`'s `plan` and
`mac_geom` — which would be better still here if the Wayland types allow it.
Either way this is deliberately the largest testable surface the feature has,
because the parts below it (real X11 windows, real `wl_surface`s) cannot run on a
headless runner as CI is configured.

**That claim carries a constraint, and it is the constraint that makes or breaks
it: the pure module may not name a `wayland-client` or `wayland-protocols-wlr`
type.** Those crates are Linux-target dependencies, and the Windows and macOS
lanes compile this module under `cfg(test)`, where they do not exist — a single
`zwlr_layer_shell_v1` in a signature turns "tested on every lane" into a build
error on two of them. The interface is therefore plain data: interface names as
`&str`, environment variables as `Option<&str>`, and — for step 5 — an output
identifier plus a refusal reason, capability out. Naming that last input here is
the point: it is what keeps the downgrade a rule this module owns and tests,
rather than a special case bolted on beside it where no lane would see it. Both
precedents already obey this without saying so (`mac_events` takes a raw `u32`
reconfigure flag, not a `CGDisplayChangeSummaryFlags`), which is why it is here
rather than rediscovered.

## Consequences

- **A session that advertises neither protocol reports "software dimming
  unavailable" without any compositor being named in Duja's code.** The
  expectation behind the README's existing GNOME-Wayland note is that Mutter
  advertises neither — but that expectation is exactly the kind of claim this ADR
  refuses to encode, so it is recorded here as *believed, unverified, and never
  relied on*. If it is wrong, or becomes wrong, the probe is already right and no
  release is needed.
- **Every combination is representable**, including ones a table would not
  anticipate: layer-shell without gamma-control, gamma without an overlay, and X11
  sessions on a compositor whose Wayland session supports neither. Deliberately
  stated without naming which compositors are in which state today; that is the
  spike's job to report, not this ADR's to assert.
- **Hardware control is unaffected and must be said so in the UI.** A display with
  DDC or a backlight is fully controllable on GNOME Wayland; only the sub-floor
  software layer is missing. The capability report is per-mechanism precisely so
  the flyout can disclose that rather than implying Duja does not work.
- **The disclosure has an existing shape to match.** `#103` settled that a
  platform limit is disclosed by a caption that appears only where the limit
  applies, gated on a plumbed value rather than a hardcoded string. The Linux
  overlay-unavailable case is the same problem and should reuse it rather than
  invent a second convention.
- **This decides detection, not implementation** — but the protocol bindings are
  not a cost. Both `wlr` protocols live in `wayland-protocols-wlr`, and that crate
  (0.3.12) plus `wayland-client` (0.31.14) are **already normal, non-dev
  dependencies of the Linux build**, arriving through
  `winit → smithay-client-toolkit → i-slint-backend-winit`. The first draft of this
  ADR deferred "the crate choice" to a later wave; there is nothing to choose. The
  X11 side is a genuinely open question and stays deferred.
- **Not testable end-to-end in CI as configured.** A GitHub runner has no X server
  and no compositor by default, so CI proves the mapping and the refusal logic but
  not that an overlay appears. That is a scoping decision rather than a hard limit
  — `Xvfb`, and a headless `weston` or `cage`, are installable on `ubuntu-latest`,
  and doing so is the obvious way to raise coverage in wave 4 if the pure layer
  turns out not to catch enough. Ships 🧪 until community confirmation regardless,
  since a virtual display says nothing about a real desktop.

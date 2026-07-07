# 0003 — Software dimming: overlay primary, gamma opt-in

- Status: accepted
- Date: 2026-07-07

## Context

Duja's continuum extends one slider below the hardware floor to true black,
and covers displays with no hardware control at all. Two candidate
mechanisms, with verified (3-0 adversarial fact-check) limitations:

- **Gamma ramps**: Windows `SetDeviceGammaRamp` *silently* refuses
  near-black ramps (anti-lockout heuristic), is reset by display events, is
  documented-undefined in HDR, and persists after process death until logoff;
  macOS `CGSetDisplayTransferByTable` resets on wake; GNOME/Mutter Wayland
  implements **no** gamma protocol (only wlroots exposes
  `wlr-gamma-control-unstable-v1`).
- **Transparent click-through overlay**: reaches true black, HDR-safe, stable,
  works everywhere a top-level always-on-top window can exist. Limitation:
  cannot cover exclusive-fullscreen apps or the secure desktop.

## Decision

Per-monitor click-through overlay is the **primary** software dimmer. Gamma is
an **opt-in** enhancement only where verified safe (Windows SDR, macOS with
re-apply-on-wake, wlroots), never in HDR.

## Consequences

- Overlay input-transparency is a security property, QA-checked every release.
- Windows gamma use requires the crash-marker + `duja --restore` restitution
  path before it ships (P4).
- GNOME Wayland (no third-party overlay or gamma path): software dimming is
  capability-gated off pending ADR-0011.

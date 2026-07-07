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

## P1 spike verification (2026-07-07, branch `spike/overlay`, Windows 11)

Verified recipe: `WS_POPUP` + `WS_EX_LAYERED|TRANSPARENT|NOACTIVATE|
TOOLWINDOW|TOPMOST`, black class brush, `SetLayeredWindowAttributes(LWA_ALPHA)`
(NOT `UpdateLayeredWindow` — per-pixel only, ~14 MB/frame; NOT
`WS_EX_NOREDIRECTIONBITMAP` — breaks the GDI layered path), `SW_SHOWNA`,
`WM_NCHITTEST → HTTRANSPARENT`. Overlays must remain **childless**.

- **Click-through: PASS with negative control** (SendInput click through an
  alpha-153 overlay reached the window beneath; stripping the flags blocked it).
- No focus steal on creation. Cost: ~0.8% of one core during 60 fps alpha
  animation, ~10 MB RSS.
- `WDA_EXCLUDEFROMCAPTURE` (build 19041+, gate on version) removes the dim
  from captures entirely — even BitBlt — while the physical screen stays
  dimmed: screenshots/shares are never dimmed.
- Z-order product contract: dims desktop, apps, and taskbar; Start menu,
  notifications, later-created topmost windows, exclusive-fullscreen, and the
  secure desktop (UAC/lock) stay undimmed. Re-assert `HWND_TOPMOST` on
  foreground changes; document the contract as "dims your content, not
  system UI".
- Production notes: PerMonitorV2 via the app **manifest** (runtime call only
  as belt-and-braces); one overlay pinned per monitor; rebuild all overlays on
  `WM_DISPLAYCHANGE`/DPI/topology changes.

# 0014 — Perceptual brightness continuum (v2)

- Status: accepted
- Date: 2026-07-13
- Supersedes the mapping in ADR-0003's continuum (the overlay-vs-hardware
  split); the overlay-first *dimming mechanism* decision there still holds.

## Context

The v1 continuum ([`continuum::map_user_level`]) mapped the slider onto hardware
1:1 above the floor and applied `MAX_ALPHA·(floor−p)/floor` overlay below it. Two
problems surfaced in live use:

1. **The default floor (0) had no software zone at all.** With `floor = 0` the
   "below the floor" branch was unreachable (`p < 0` never holds), so a user who
   turned on "software dimming" got nothing. The app worked around this by
   silently seeding a 20% floor when the toggle was enabled
   (`DEFAULT_SOFTWARE_DIM_FLOOR_PCT`) — a hack that moved the hardware handoff and
   surprised anyone who had deliberately set floor 0.
2. **The slider was not perceptual.** Slider 20 meant "hardware 20", whose actual
   brightness varies wildly per panel (and, below the floor, meant something else
   entirely). "20% should look 20% bright" was not true, and the hardware/software
   split ratio did not adapt to the panel.

A monitor's true luminance range is **not** knowable over DDC (the 0–100 scale is
abstract; SDR EDID carries no luminance). So the ratio cannot be measured — it
must be modelled with one tunable per display, and the model must keep the slider
perceptual regardless of the floor.

## Decision

The slider position **is** perceived brightness (0–100%). Each hardware display
carries one tunable, `min_perceived_pct` (call it `m`): the perceived brightness
the panel shows at hardware zero (schema default 25; settings range 5–60; the core
clamps ≤95). Hardware value `h` sits at slider position `pos(h) = m + (100−m)·h/100`
— **line A** at `m` (hardware zero), **line B** at `pos(floor)` (the transition).

- **At/above B:** pure hardware; the hardware value is `pos⁻¹(slider)`, so the top
  75% (default) of the slider is the panel's full range and moving the slider is
  perceptually linear.
- **Below B:** hardware pins at the floor and overlay/gamma supplies the missing
  darkness so perceived brightness still equals the slider (`alpha = 1 − p/B`),
  clamped by the unchanged `MAX_ALPHA` (never fully black).
- The floor is a **write limit**, not a scale change: changing it moves line B but
  never rescales the slider, so a mid-run floor change never moves the thumb.
- `reverse_map` becomes `round(pos(h))` and is **not** floor-clamped — an external
  change that drove the panel below the floor reflects truthfully between A and B
  (the floor is a write policy, not a read policy). This is what the external-change
  reflection path (a later PR) consumes; v1's `reverse_map` had no runtime caller.

`geometry()` exposes the two marker fractions and the minimum usable fraction for
the flyout to draw lines A and B (coincident when floor 0) and to bound the drag.

## Consequences

- **The 20%-seed hack is deleted.** With floor 0 the slider now has a real software
  zone below `m` (slider 0–25 by default), so the dimming toggle simply switches
  the dim mode (Overlay/Off) with no floor seeding.
- **No `schema_version` bump.** `min_perceived_pct` is a serde-defaulted field
  (missing ⇒ 25), so old `config.toml` files load unchanged and no migration is
  needed.
- **No `STATE_VERSION` bump.** Persisted `state.toml` levels are perceived-% and
  were already; launch adoption re-derives each level from the live hardware
  reading via `reverse_map`, so a stored value only matters in the brief
  pre-probe window (worst case: one bounded, self-correcting shift on a pre-Get
  hotkey nudge). A remap would need each display's `m` at migration time
  (config↔state coupling) for negligible benefit.
- **f32 internals, exact endpoints.** All values are ≤100 (exactly representable);
  the single division is ≤1 ulp. Endpoints are pinned structurally (slider 100 ⇒
  hardware 100; slider 0 ⇒ `MAX_ALPHA`) and proptests assert monotonicity and
  continuity-at-B on the perceived metric within ±1 slider unit of quantization.
- **Staged rollout.** This PR lands the pure core (mapping, `reverse_map`,
  `geometry`, the config field) with the app passing `m = 0` at the boundary — so
  default (floor-0) displays are unchanged until the follow-up PR wires the anchor
  through together with the slider markers and the settings control. (For a display
  with a *custom* non-zero floor, the sub-floor overlay curve changes shape even at
  `m = 0` — v2's is perceptually linear where v1's was not; this is the intended
  new behaviour and only affects users who had already tuned a floor.)

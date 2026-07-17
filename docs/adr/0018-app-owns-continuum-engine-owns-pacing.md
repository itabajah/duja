# 0018 — App owns the continuum; the engine owns pacing

- Status: accepted
- Date: 2026-07-17

## Context

Brightness flows through three layers: the UI slider (a perceived-brightness
position), the app's continuum policy (which splits a user level into a hardware
target plus overlay/gamma software dimming, per [ADR-0014](0014-perceptual-continuum-v2.md)),
and the engine's per-monitor workers (which drive the DDC/panel hardware). Two
questions recur at every change: *who decides the hardware value*, and *who
rate-limits writes to a monitor that can only accept one every ~40–50 ms*?

A P4 defect made the answer load-bearing. A UI-side leading-edge throttle on the
slider→`SetUserLevel` path dropped a drag's **final** sample: the slider, the
overlay, and the persisted state all showed the correct number while the hardware
settled at an intermediate value. The throttle was trying to pace writes at the
wrong layer — the layer that cannot see whether a write actually reached the
panel. This decision records the split the fix established so it is not
re-litigated (and not re-broken by a well-meaning "let's debounce the slider").

## Decision

1. **The app owns the continuum.** Only the app translates a user level into a
   `(hardware_pct, overlay_alpha | gamma)` batch, using the pure `duja-core`
   continuum and the per-display config (floor, perceptual anchor, dim mode, HDR
   verdict). The engine and the UI never compute brightness policy; the UI reports
   a slider position and the app decides what that means.

2. **The engine owns pacing, and it is the *single* pacing authority.** Every
   `SetUserLevel` is forwarded to the engine **un-throttled**. The per-monitor
   worker's `write_min_gap` last-wins coalescer is the *only* place writes are
   rate-limited, and it **guarantees final-value delivery**: a burst collapses to
   far fewer hardware writes, but the last requested value always lands. There is
   no UI-side throttle, debounce, or drop.

3. **Software dimming is declarative and app-driven.** The app hands the dimmer a
   full desired-state batch; the dimmer reconciles it against its live overlays.
   The engine (hardware) and the dimmer (software) are two independent sinks the
   app coordinates — HDR forces overlay-only, the floor hands off to software
   below the transition — but neither sink second-guesses the app's policy.

## Consequences

- No layer other than the engine may rate-limit brightness writes. A UI-side
  throttle/debounce is a **regression** by definition (it can strand the final
  value); the throttle-final-value test guards the contract.
- The engine's coalescer must always deliver the last value — "drop intermediate
  samples" is allowed, "drop the final sample" is not.
- Because the app is the single policy owner, cross-cutting concerns that change
  the mapping (perceptual anchor, HDR verdict, floor) live in the app and take
  effect on the next user action or enumeration, without the UI or engine needing
  to know the continuum exists.
- The UI stays presentation-only (pure view-models, no policy), and the engine
  stays a mechanism (open/get/set/coalesce), which keeps both independently
  testable and the seams thin.

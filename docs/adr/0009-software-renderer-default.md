# 0009 — Slint renderer: software renderer as default

- Status: accepted
- Date: 2026-07-07
- Evidence: spike branch `spike/eventloop`, release builds (thin-LTO, strip,
  codegen-units=1), warm measurements on Windows 11.

## Context

Budgets: idle RSS ≤ 35 MB (flyout hidden — the dominant state), stripped
binary ≤ 12 MB. Measured (slint 1.17.1, window hidden, warm):

| Renderer | Stripped binary | Idle WS / Private (hidden) | Verdict |
|---|---|---|---|
| **software** | **9.76 MB** | **~17–18 MB / ~2.5 MB** | passes both |
| skia (prebuilt) | 14.87 MB | ~18–19 MB / ~3.1 MB | binary over budget |
| femtovg (Slint default) | 9.22 MB | **54–62 MB / ~39 MB** (+cold spike ~117 MB) | RSS failure |

Visual quality was indistinguishable across all three for the flyout (dark
Fluent theme, antialiased text, slider). FemtoVG's footprint is the OpenGL
driver stack; a mostly-hidden tray flyout cannot justify it.

## Decision

Ship Slint's **software renderer** as Duja's default on all platforms:
`slint = { default-features = false, features = ["compat-1-2",
"backend-winit", "std", "renderer-software"] }`. Keep the renderer behind a
cargo feature so `renderer-skia` remains a one-line fallback if smooth
high-FPS animation on 4K ever becomes a requirement (would need the binary
budget relaxed to ~15 MB).

## Consequences

- CPU rendering removes the GPU-driver dependency: robust over RDP, VMs, and
  headless CI runners; fewer crash vectors; no cold shader-cache spike.
- Rendering cost is bounded by design: the flyout is small, rarely animated,
  and hidden almost always; overlay dimming does not render through Slint at
  all (it's a layered Win32 window, ADR-0003).
- Note: Slint's *own* `system-tray` default feature is deliberately unused —
  the `tray-icon` crate gives full icon/menu control (see ADR-0001 update).
- Re-measure at the P4 gate on real UI; revisit only on measured failure.

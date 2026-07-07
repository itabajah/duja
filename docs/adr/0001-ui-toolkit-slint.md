# 0001 — UI toolkit: Slint + tray-icon (no webview)

- Status: accepted
- Date: 2026-07-07

## Context

Duja must be ultra-lightweight (≤ 35 MB idle RSS, zero idle wakeups) and
explicitly bans Electron. The real cost driver in "lightweight" frameworks is
the webview: Tauri/Wails ship small binaries but host an OS webview
(~100–180 MB resident per published comparisons) — wrong for a tray app that
idles all day. A brightness flyout has no HTML to justify that.

## Decision

Rust workspace; **Slint** renders the flyout (native GPU/software rendering,
royalty-free license since 1.1); the **tray-icon** crate (Tauri project,
standalone) provides the tray on all three OSes.

## Consequences

- Tear down the rendering surface while the flyout is hidden; idle memory is
  dominated by the tray handle, not a live GPU context.
- Linux: tray-icon needs gtk + libappindicator/libayatana (packaging deps) and
  emits **no tray mouse events** — Linux UX is driven from the context menu.
  ksni re-evaluated in ADR-0010.
- Renderer (FemtoVG vs Skia) decided by measurement in ADR-0009; binary budget
  ≤ 12 MB favors FemtoVG.
- **Fallback trigger:** if the P1 flyout prototype exceeds the RSS budget or
  event-loop cohabitation fails, switch to egui/eframe (documented escape
  hatch) before any UI code lands in main.

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
- Renderer decided by measurement in ADR-0009 (software renderer default).
- Slint's own `system-tray` default feature is NOT used; `tray-icon` gives
  full icon/menu control. Build with `default-features = false`.

## P1 spike verification (2026-07-07, branch `spike/eventloop`)

The cohabitation risk is **resolved**; the egui fallback is retired.
Verified recipe (slint 1.17.1, tray-icon 0.24.1, global-hotkey 0.8.0):

- Construct the Slint component, then the tray and hotkey manager, all on the
  **main thread** — each creates a hidden Win32 window owned by that thread,
  and winit's `GetMessage` pump dispatches for all of them. One pump, no
  manual pumping, no polling timer.
- Deliver events via each crate's `set_event_handler`, marshalling to the UI
  with `slint::Weak::upgrade_in_event_loop` (the `Weak` is `Send`; the
  component handle is not).
- Keep-alive while hidden: `slint::run_event_loop_until_quit()`;
  hide-on-close via `CloseRequestResponse::HideWindow`.
- Measured idle (window hidden): **0.000% CPU over 8 s — zero wakeups**,
  meeting the budget by construction. Injected Ctrl+Alt+F9 toggled the
  window through the real pump; clean exit.
- Residual: menu-click delivery uses the identical dispatch path but wasn't
  independently injected (shell-tray clicks aren't scriptable) — verify by
  hand at the P4 QA gate.

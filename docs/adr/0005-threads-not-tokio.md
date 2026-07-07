# 0005 — Concurrency: std threads + channels, no async runtime

- Status: accepted
- Date: 2026-07-07

## Context

Every slow operation Duja performs is **blocking FFI**: DDC/I2C writes
(~50 ms/op, occasionally multi-second GPU-driver stalls), WMI COM calls,
gamma calls. OS event sources are callback/message-pump shaped. Under tokio
these all become `spawn_blocking` — i.e. threads anyway — plus a runtime,
timer wheel, and binary weight, for zero async I/O.

## Decision

`std::thread` + `crossbeam-channel`, actor-ish:

1. **Main/UI** — Slint loop; owns tray + hotkeys (macOS main-thread rule).
2. **Controller** — owns all mutable state; single `select!` over command/
   event channels; debounce deadlines via `recv_deadline` only while pending.
3. **Per-monitor DDC workers** — each exclusively owns its controller
   (`&mut self` on the trait makes serialization compile-time); trailing-edge
   latest-wins coalescing with a per-feature pending map; quirk-tunable
   `min_write_gap` (default 100 ms). DDC values never animate; overlay alpha
   may.
4. **Platform events / IPC accept / WMI sink** threads feeding channels.

All threads park on `recv` when idle → zero idle wakeups by construction.

## Consequences

- Stuck-driver watchdog: unacked write after 5 s → display `Unresponsive`,
  leak the stuck thread (bounded 1/monitor/session), recover on next hot-plug.
- Every thread body in `catch_unwind` with supervised restart (capped
  backoff); requires `panic = "unwind"` — never switch to abort.
- Deterministic tests: debounce/coalesce are pure state machines fed a
  `FakeClock`; thread shells stay ~10 lines.

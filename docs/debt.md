# Refactor debt

Running list, drained by a dedicated `refactor:` PR at each phase checkpoint
(time-boxed to ~15% of the phase). Add entries during feature work instead of
detouring; delete entries when drained.

| Added | Where | What | Why deferred |
|---|---|---|---|
| P1 | P4 QA | Manually verify tray menu-click delivery (not scriptable; identical dispatch path as verified hotkey) | Needs human hand on shell tray |
| P2 | `.github/workflows/` | Add `coverage.yml` (llvm-cov ≥90% gate) and `fuzz.yml` (weekly nightly burn) CI jobs | Ran locally at the P2 gate; wire into CI in a P8 hardening pass |
| P2 | `duja-core` `sync`↔`config` | Wire `MonitorConfig.sync_offset` (now persisted) into the app's `SyncGroups` load/save path | Consumer lands in P4/P5 UI wiring; core plumbing complete |
| P3 | `duja-ddc` `win/sys.rs` | Validate `classify_failure`'s `GetLastError`-after-VCP-call assumption on real hardware; a gone monitor may classify `Timeout` instead of `Disconnected` | dxva2 VCP calls are not documented to `SetLastError`; needs a live unplug experiment (console session) |
| P3 | `duja-app` `engine.rs` | First `SetUserLevel` before the initial-Get ack scales against the default max=100; wrong absolute level on non-100-max monitors until calibration lands (transient, self-correcting) | Needs a queue-behind-initial-Get design decision; harmless for P3 bring-up |
| P3 | `duja-app` `engine.rs` | Watchdog deadline re-stamps on every dispatch, so continuous slider input against a stuck worker defers detection until the user pauses | Semantics decision (per-op vs per-display deadline); revisit with P4 UI feel |
| P3 | `duja-app` `run.rs` | `PlatformEvent::Suspending` is dropped; no pre-suspend write quiescing | Consumer is the P4 dimmer/state re-apply work |
| P3 | `duja-ddc`/`duja-app` | Full DDC capability probing (contrast, input source) at enumeration; P3 sets brightness-only capabilities statically | Consumers (contrast UI, VCP 0x60 switching) land in P4/P5 |
| P3 | `dujactl` | `duja-ipc` dependency is unused until the P5 transport lands | Placeholder for the P5 IPC client |
| P3 | `duja-panel` `wmi.rs` | `WmiMonitorID` array decoding, `WmiSetBrightness` invocation, and ProductCodeID assumptions never executed on real hardware (dev box has no internal panel) | Needs a 30-min borrowed-laptop run before P5 (plan §P3) |

Drained at P2 gate (2026-07-08): MSI MP273QP quirk rows encoded in `quirks/quirks.toml`; contract suite hardened against `max`-lying backends (ADR-0002); unstamped-config migration semantics fixed (ADR-0007).

Drained at P3 gate (2026-07-10): `ResolvedQuirks` fields are now consumed by the
dxva2 backend (pacing, caps retry, verify-writes, `max_brightness`,
`input_source_allowed`, `no_input_switch`, `caps_unreliable`, `ddc_broken`).

Drained at P4 (2026-07-10): the ubuntu Slint system-deps step (fontconfig +
xkbcommon + xcb + wayland) is wired into the `clippy`, `test`, and `docs` CI
jobs now that `duja-ui` brings Slint into the workspace.

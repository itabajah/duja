# Refactor debt

Running list, drained by a dedicated `refactor:` PR at each phase checkpoint
(time-boxed to ~15% of the phase). Add entries during feature work instead of
detouring; delete entries when drained.

| Added | Where | What | Why deferred |
|---|---|---|---|
| P0 | `.github/workflows/ci.yml` | Add ubuntu system-deps step (fontconfig, xkbcommon…) when Slint lands | No GUI deps exist yet |
| P1 | P4 QA | Manually verify tray menu-click delivery (not scriptable; identical dispatch path as verified hotkey) | Needs human hand on shell tray |
| P2 | `.github/workflows/` | Add `coverage.yml` (llvm-cov ≥90% gate) and `fuzz.yml` (weekly nightly burn) CI jobs | Ran locally at the P2 gate; wire into CI in a P8 hardening pass |
| P2 | `duja-core` `sync`↔`config` | Wire `MonitorConfig.sync_offset` (now persisted) into the app's `SyncGroups` load/save path | Consumer lands in P4/P5 UI wiring; core plumbing complete |
| P2 | `duja-core` `quirks` | `ResolvedQuirks` fields (`max_brightness`, `input_source_allowed`, `ddc_broken`, …) have no consumer yet | Consumed by the P3 dxva2 backend |

Drained at P2 gate (2026-07-08): MSI MP273QP quirk rows encoded in `quirks/quirks.toml`; contract suite hardened against `max`-lying backends (ADR-0002); unstamped-config migration semantics fixed (ADR-0007).

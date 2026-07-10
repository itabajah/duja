# 0012 — Binary size budget raised to 16 MB for 1.0 (variance ADR)

- Status: accepted
- Date: 2026-07-10 (P4 gate)
- Evidence: release builds of `duja.exe` at the P4 gate on Windows/MSVC.

## Context

`docs/perf-budgets.md` set a ≤ 12 MB stripped-binary budget, derived from the
ADR-0009 renderer bake-off where a bare Slint software-renderer binary measured
9.76 MB. The assembled P4 tray application measures **14.9 MB** with thin LTO
(13.9 MB with fat LTO). The overage is not code we wrote: it is the Slint winit
backend's image/SVG decode stack (`resvg`, `image`, `tiny-skia`, `zune-*`,
`png`), `tray-icon`/`muda`, and `tracing-subscriber`'s `regex`-based
`EnvFilter`. Every other perf budget passes with headroom (idle RSS 23.3 MB vs
≤ 35; **zero** idle CPU wakeups measured over 20 s; `dujactl` is 0.6 MB).

## Decision

Raise the 1.0 stripped-binary budget for `duja.exe` to **≤ 16 MB** and treat
size reduction as a P8 hardening work item rather than blocking the MVP. The
levers, in expected-payoff order, are recorded for P8:

1. Fat LTO in the release profile (measured −1.0 MB; deferred only because the
   profile change is workspace-wide and P8 re-verifies budgets anyway).
2. Slint image-format features: the flyout uses no SVG/EXR/animated images —
   investigate disabling the decoder stack Slint pulls by default.
3. `tracing-subscriber` without the `env-filter`/`regex` feature (a static
   level filter is enough for a tray app; `--verbose` can stay).
4. `panic = "unwind"` is required by supervision (ADR-0005) — not a lever.

## Consequences

- `docs/perf-budgets.md` now reads ≤ 16 MB (aspiration 12) for `duja.exe`.
- If P8's levers get the binary back under 12 MB, this ADR is superseded and
  the original budget is restored.
- The RAM budget — the budget users actually feel, and the reason Electron was
  rejected — is unchanged and passing at 23.3 MB idle.

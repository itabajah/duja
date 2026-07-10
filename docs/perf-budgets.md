# Performance budgets

Hard budgets, measured at the P4 and P8 gates on real hardware (dev PC for
Windows; community numbers for macOS/Linux). A missed budget blocks the phase
exit unless an ADR records the variance and the recovery plan.

| Budget | Target | How measured |
|---|---|---|
| Idle RSS (flyout closed) | ≤ 35 MB private (aspiration 25) | Task Manager / `sysinfo` self-report in `--soak` |
| Idle CPU | 0 periodic wakeups | Process Explorer context-switch delta over 60 s; design rule: no polling loops anywhere |
| Cold start → tray icon visible | < 300 ms | tracing span; DDC probing must be off the startup path |
| Slider → DDC write dispatched | ≤ 1 coalesce interval (~100 ms) | tracing span |
| Overlay alpha update | < 16 ms (one frame) | tracing span |
| Stripped release binary | ≤ 16 MB (aspiration 12; see ADR-0012) | CI size report |
| Soak (24 h) RSS growth | < 5 MB; flat GDI/USER handle counts | `--soak` self-report |

Design rules that protect the budgets:

- Event-driven everything; threads park on `recv` (ADR-0005).
- Zero Slint timers/animations while the flyout is hidden.
- DDC values never animate; overlay alpha may (GPU-cheap).
- State-file writes debounced ≥ 2 s trailing edge.

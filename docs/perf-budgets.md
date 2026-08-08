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
| Stripped release binary (`duja`) | ≤ 16,777,216 bytes (16 MiB; [ADR-0012](adr/0012-binary-size-budget-variance.md)) | `cargo xtask size`, gated in the release workflow |
| Stripped release binary (`dujactl`) | ≤ 2,097,152 bytes (2 MiB) | same |
| Soak (24 h) RSS growth | < 5 MB; flat GDI/USER handle counts | `--soak` self-report |

**Why the binary budgets are written in bytes.** They were written "16 MB" for
four gates, and nobody noticed that 16 MB and 16 MiB differ by 5 % - wider than
most of the levers that were being weighed against them. The number here and the
constant in `xtask/src/size.rs` are the same integer, and an xtask test reads
this file and fails if the two drift.

**Two of these rows are not measured by anything today.** "Overlay alpha update"
and "Cold start" were measured by hand at the P4 gate and have not been measured
since; P8 wave 1 changed the optimization level, which plausibly moves both, so
they are booked into [qa-checklist.md](qa-checklist.md) for the next hardware
run. There is no automated render benchmark in this repository. Saying so here
is the point: a budget row that names no live instrument is a claim about the
past, not a guarantee about the build in front of you.

Design rules that protect the budgets:

- Event-driven everything; threads park on `recv` (ADR-0005).
- Zero Slint timers/animations while the flyout is hidden.
- DDC values never animate; overlay alpha may (GPU-cheap).
- State-file writes debounced ≥ 2 s trailing edge.

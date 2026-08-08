# Performance budgets

Hard budgets, measured at the P4 and P8 gates on real hardware (dev PC for
Windows; community numbers for macOS/Linux). A missed budget blocks the phase
exit unless an ADR records the variance and the recovery plan.

| Budget | Target | How measured |
|---|---|---|
| Idle RSS (flyout closed) | ≤ 36,700,160 bytes (35 MiB; aspiration 25) | `duja --soak <secs>` |
| Idle CPU | 0 periodic wakeups | Process Explorer context-switch delta over 60 s; design rule: no polling loops anywhere |
| Cold start → tray icon visible | < 300 ms | tracing span; DDC probing must be off the startup path |
| Slider → DDC write dispatched | ≤ 1 coalesce interval (~100 ms) | tracing span |
| Overlay alpha update | < 16 ms (one frame) | tracing span |
| Stripped release binary (`duja`) | ≤ 16,777,216 bytes (16 MiB; [ADR-0012](adr/0012-binary-size-budget-variance.md)) | `cargo xtask size`, gated in the release workflow |
| Stripped release binary (`dujactl`) | ≤ 2,097,152 bytes (2 MiB) | same |
| Soak (24 h) RSS growth | < 5,242,880 bytes (5 MiB); GDI/USER drift ≤ 8 | `duja --soak 86400` |

**Why the binary budgets are written in bytes.** They were written "16 MB" for
four gates, and nobody noticed that 16 MB and 16 MiB differ by 5 % - wider than
most of the levers that were being weighed against them. The number here and the
constant in `xtask/src/size.rs` are the same integer, and an xtask test reads
this file and fails if the two drift.

**The two soak rows now have the instrument they always cited.** `--soak` did
not exist until P8 wave 3, which meant those rows named a method nobody could
run: `sysinfo` is not a dependency of this workspace and never was, and the flag
was `--stress`, which floods DDC writes and measures something else. It exists
now. It assembles the real pipeline exactly as `--headless` does, leaves it
**idle**, samples RSS and (on Windows) GDI/USER object counts, and exits non-zero
on a budget miss.

Read the idle-soak result for what it is. ADR-0005 parks every thread on `recv`
and the design rule above is "no polling loops anywhere", so an idle soak is the
test of *that*, and it catches a leak in the event pump, a timer somebody added,
or a handle taken per wake. It does **not** test a busy Duja: a soak that drives
level changes and hot-plug for hours is a different harness and does not exist.

On a platform that cannot read its own usage the verdict is `UNMEASURABLE` and
the exit code is non-zero - **not** a pass. macOS is that platform today
(`task_info` is Mach FFI nobody here has run), and reporting success for a run
that measured nothing would be the exact failure these rows had before.

**Two rows still name no instrument.** "Overlay alpha update" and "Cold start"
were measured by hand at the P4 gate and not since; P8 wave 1 changed the
optimization level, which plausibly moves both. There is no automated render
benchmark in this repository ([D-109](debt.md#d-109)), and until there is,
[qa-checklist.md](qa-checklist.md) is where those two get measured. A budget row
that names no live instrument is a claim about the past, not a guarantee about
the build in front of you.

Design rules that protect the budgets:

- Event-driven everything; threads park on `recv` (ADR-0005).
- Zero Slint timers/animations while the flyout is hidden.
- DDC values never animate; overlay alpha may (GPU-cheap).
- State-file writes debounced ≥ 2 s trailing edge.

# Performance budgets

Hard budgets, measured at the P4 and P8 gates on real hardware (dev PC for
Windows; community numbers for macOS/Linux). A missed budget blocks the phase
exit unless an ADR records the variance and the recovery plan.

| Budget | Target | How measured |
|---|---|---|
| Idle RSS (flyout closed) | ≤ 35,000,000 bytes private (aspiration 25) | by hand for the tray build; `duja --soak <secs>` for the headless one |
| Idle CPU | 0 periodic wakeups | Process Explorer context-switch delta over 60 s; design rule: no polling loops anywhere |
| Cold start → tray icon visible | < 300 ms | **no live instrument** - by hand, see below |
| Slider → DDC write dispatched | ≤ 1 coalesce interval (~100 ms) | **no live instrument** - by hand, see below |
| Overlay alpha update | < 16 ms (one frame) | **no live instrument** - by hand, see below |
| Flyout frame render (3 monitors, headless) | < 16 ms (one frame) | `cargo test -p duja-ui --release --test frame_probe -- --ignored --nocapture` |
| Stripped release binary (`duja`) | ≤ 16,777,216 bytes (16 MiB; [ADR-0012](adr/0012-binary-size-budget-variance.md)) | `cargo xtask size`, gated in the release workflow |
| Stripped release binary (`dujactl`) | ≤ 2,097,152 bytes (2 MiB) | same |
| Soak (24 h) RSS growth | < 5,000,000 bytes; flat GDI/USER handle counts | `duja --soak 86400` |

**Why the binary budgets are written in bytes.** They were written "16 MB" for
four gates, and nobody noticed that 16 MB and 16 MiB differ by 5 % - wider than
most of the levers that were being weighed against them. The number here and the
constant in `xtask/src/size.rs` are the same integer, and an xtask test reads
this file and fails if the two drift.

**The two soak rows have an instrument at last, and it is narrower than the
rows.** `--soak` did not exist until P8 wave 3, so those rows named a method
nobody could run - and named `sysinfo`, which is not a dependency of this
workspace and never was. What exists now assembles the pump, the engine and the
IPC server (the three pieces `--headless` has), goes idle, samples, and exits
non-zero on a budget miss or on a run it could not measure.

Three limits, because a budget row that overstates its instrument is worse than
one with no instrument at all:

- **It is the headless process, not the tray one.** "Flyout closed" is a
  tray-mode state, and the soak builds no Slint shell, no tray icon and no
  window. Its number is a lower bound: useful for *growth*, since a leak in the
  engine or the pump appears in both, and not a substitute for the absolute
  idle-RSS figure, which is still a hand measurement on a real tray build.
- **It is not "private".** What every OS here hands back is the whole resident
  set, private plus resident shareable pages. It over-counts against this row,
  which is the safe direction, and `duja_platform::process` has the measured
  difference.
- **It is idle, not busy.** ADR-0005 parks every thread on `recv` and the design
  rule below is "no polling loops anywhere", so an idle soak tests exactly that:
  a leak in the event pump, a timer somebody added, a handle taken per wake. A
  soak that drives level changes and hot-plug for hours is a different harness
  and does not exist.

The harness fails on GDI/USER drift above 8 rather than above 0 - looser than
this row, because [D-005](debt.md#d-005) is the standing example of a harness
gating on absolute zero and reporting FAIL on a healthy run, and because nobody
has run 24 hours to measure the real idle drift. Since its threshold is looser
than the budget, its report **names any non-zero drift even when it passes**: it
cannot print "flat" for something that moved.

On a platform that cannot read its own usage the verdict is `UNMEASURABLE` with
a non-zero exit - **not** a pass. macOS is that platform today (`task_info` is
Mach FFI nobody here has run).

**Three rows name no instrument, and until the P8 gate they named one that does
not exist.** "Cold start", "Slider to DDC write" and "Overlay alpha update" all
said *tracing span* in the How-measured column. **There is not a single
`tracing::span!` or `#[instrument]` in this repository** - `tracing` is used here
purely for events. All three were measured by hand at the P4 gate and not since,
and P8 wave 1 changed the optimization level, which plausibly moves at least two
of them.

A span would not have been much of an instrument anyway: it is a thing a human
reads while watching the app, not a thing that fails a build. The honest column
is the one now there, and [qa-checklist.md](qa-checklist.md) is where these get
measured. A budget row that names an instrument which does not exist is worse
than one that admits it has none - which is the rule this project states,
applied to the file that states it.

**The frame row is new, and it is not one of those three.** [D-109](debt.md#d-109)
proposed a software-renderer harness as the remedy for "Overlay alpha update"
and "Cold start", and that was wrong on both: the overlay is `duja-dimmer`'s
layered Win32 window and is not a Slint surface at all, and a cold start to a
*tray icon* needs an interactive session that no headless renderer can see. The
harness got built anyway, because the thing it does measure is the exposure the
row was actually arguing about - the frame path that P8 wave 1 exempted from
`opt-level = "s"` by name. So the three rows above still have no instrument, and
the row added here is a fourth thing that now does. D-109 is narrowed rather
than drained.

`cargo test -p duja-ui --release --test frame_probe -- --ignored --nocapture`
renders the real `FlyoutShell` - three monitors, 360 x 260, the app's own design
size - through Slint's software renderer into a plain buffer, with no display
server, on any lane. It discards a warm-up tenth and times the rest.

**Measured on this box (Windows, release profile):** min **177 us**, mean
**186-193 us** across runs, worst frame **222-306 us**. Two orders of magnitude
inside the budget.

**And the exemption it exists to check is worth about 1.4x to 1.5x.** With the
five per-package `opt-level = 3` overrides removed - `-Os` everywhere - the same
probe reports min **253 us**, mean **285 us**, worst **390 us**. So the
exemption does what P8 wave 1 argued it would, at the 1,429,504 bytes that
section already prices; and *also*, both builds clear this budget with room to
spare, so nothing here depends on it. Both halves are the measurement, and only
the first was ever predicted.

Two limits on that number, because a budget row that overstates its instrument
is worse than one with no instrument at all:

- **It is a full redraw, which the shipped app does not always do.** The probe
  asks for `NewBuffer`, so every frame repaints all 93,600 pixels; the windowed
  path can repaint a dirty region instead. It over-states against this row,
  which is the safe direction, and the probe asserts the full area was drawn on
  every frame - a partial redraw quoted as a frame time is the one way this
  number could be wrong in the direction nobody checks.
- **It is a reflection, not a drag.** A headless harness has no input to
  inject, so the probe dirties the tree through `update_from_vm`, the same path
  the app's external-change reflection takes. The Slint callback a real thumb
  drives is not exercised here; `shell.rs`'s own tests are what cover that.

Design rules that protect the budgets:

- Event-driven everything; threads park on `recv` (ADR-0005).
- Zero Slint timers/animations while the flyout is hidden.
- DDC values never animate; overlay alpha may (GPU-cheap).
- State-file writes debounced ≥ 2 s trailing edge.

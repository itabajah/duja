# Duja - the plan

What happens next, and in what order. [STATUS.md](STATUS.md) says where the
build stands; this file says where it is going. Anything already done is
described in [history.md](history.md) rather than here, so this file stays short
enough that reading it is never a research task.

## What is left

1. **P7 wave 5** - the Linux tray, half landed. See [below](#wave-5---the-tray).
2. **P7 wave 6** - `xtask dist --target linux`, the release job, the docs.
3. **P7 wave 7** - the phase gate, adversarial review, tag `m7-linux`.
4. **`v0.3.0`** - the Linux release, once the gate passes.
5. **P8** - hardening to `v1.0.0`: fuzz burn-in, a 72 h soak, packaging, the
   binary-size trim ([ADR-0012](adr/0012-binary-size-budget-variance.md)), and
   draining what [debt.md](debt.md) still holds.

Two things are **held rather than pending**, and neither blocks the list above:

- **`v0.2.0` (macOS)** is tagged as `m6-macos` and deliberately unreleased until
  someone has launched `Duja.app` on a real Mac. The phase is closed; the
  release is not. This is a decision, not a blocker.
- **Laptop QA of the v0.1.4 mirror/software-only behaviour**, and a regenerated
  `social-preview.png`, are carried from the Windows train. Both need a human.

## The version ladder

Re-mapped in [ADR-0019](adr/0019-version-ladder-and-release-trains.md): `v0.1.x`
is the Windows train, `v0.2.0` macOS, `v0.3.0` Linux, `v1.0.0` hardening. A
phase exits on a milestone tag; a release is a separate decision from a tag, and
`v0.2.0` is the standing example of the two coming apart.

## The phases

| phase | milestone | state |
|---|---|---|
| P0 Foundation | `m0-foundation` | done |
| P1 Spikes (risk burn-down) | `m1-spikes` | done |
| P2 Core domain (`duja-core`) | `m2-core` | done |
| P3 Windows hardware slice | `m3-win-hw` | done |
| P4 Windows dimmer + UI (MVP) | `m4-win-mvp` | done |
| P5 Power features (Windows complete) | `m5-win-full` | done |
| P6 macOS port | `m6-macos` | done, gate passed, `v0.2.0` held |
| **P7 Linux port** | `m7-linux` / `v0.3.0` | **in progress** |
| P8 Hardening | `m8-hardening` / `v1.0.0` | pending |

P6 was hardware-blind by construction (CI runners plus community verification).
P7 is VM/WSL-assisted, and the GNOME Wayland dimming spike became *verification*
of [ADR-0011](adr/0011-linux-software-dimming.md)'s runtime probe rather than an
input to it, so it follows the decision instead of preceding it.

## P7 waves

The ADRs and commit messages refer to these by number, so they are written down
rather than left implicit.

| wave | scope | state |
|---|---|---|
| 0 | unix IPC + lock-directory hardening (shared with macOS) | done - `#114` |
| 1 | the two reserved ADRs (0010 tray, 0011 dimming), plus 0022 | done - `#115`, `#117` |
| 2 | DRM/sysfs enumeration + EDID identity, `/dev/i2c` bus, backlight (logind primary, sysfs fallback) | done - `#116` |
| 3 | event pump (`NETLINK_KOBJECT_UEVENT` direct, no libudev) + autostart, desktop, geometry | done - `#118` |
| 4 | software dimming: X11 overlay + `RandR` gamma, Wayland layer-shell + `wlr-gamma-control`, and the ADR-0011 capability probe | done - `#119`, `#121`, `#122`, `#123`, `#124`, `#130`, `#131` |
| 4b-5 | the X11 cursor anchor, so the flyout has somewhere to open | done - `#132` |
| **5** | **un-gate the tray (ksni as the third arm)** | **in progress - `#134` landed the seam** |
| 6 | `xtask dist --target linux`, the release job, and the docs | pending |
| 7 | phase gate, adversarial review, tag `m7-linux` | pending |

**Two corrections to the original table, made at the 2026-08-07 checkpoint.**
Wave 5 was written as "un-gate the tray **+ `dujactl doctor`'s Linux
diagnostic**"; the diagnostic half shipped early, in `#120`, because a user with
no visible monitors needed to be told why before anything could be tested on
Linux at all. And wave 4 grew a **4b-5** sub-wave that the table never had: the
tray flyout needs a cursor anchor, `duja-platform` had none for X11, and that is
a wave-4-shaped job (a display-server query) blocking a wave-5 one. The table
now says so rather than leaving two PRs unaccounted for.

### Wave 5 - the tray

`#134` landed the **seam**: `AppState` no longer names a tray library. It holds
one `PlatformTray`, a concrete type per target, with three methods phrased as
outcomes (`set_accent`, `set_tooltip`, `announce_update`) rather than as menu
edits. That shape is what [ADR-0010](adr/0010-linux-tray-ksni.md) asked for,
and the reason is specific: `tray-icon`'s menu model is imperative (hold
handles, mutate in place) and ksni's is declarative (hand the host a tree,
rebuilt from a callback), so a seam written in `tray-icon`'s verbs would have
forced the Linux backend to fake handles it does not have.

What remains is the ksni arm itself. **The work exists** - a `DujaTray`
implementing `ksni::Tray`, the RGBA-to-ARGB32 conversion with its tests, and the
dependency wiring - and was written but not landed, because of the constraint
below.

**The constraint that shapes this wave: `duja-app` cannot be built for Linux on
the Windows dev box.** `yeslogic-fontconfig-sys` needs a cross-compile sysroot.
Every other P7 wave could be developed locally because the pure/impure split
([ADR-0011](adr/0011-linux-software-dimming.md)) kept the decidable half testable
on all three lanes - but un-gating a module is exactly the change that pure code
cannot stand in for. So wave 5's remaining increment is a **CI-only loop** at
roughly ten minutes an iteration, and it should be planned as one: land the
dependency and the `cfg` widening in a first PR that only has to compile, and
the behaviour in a second. Do not batch unrelated changes into either.

Wave 5 also owes five [debt.md](debt.md) rows, all recorded there rather than
here: the `refuse_gamma` production caller, `restore_all`'s three write-side
faces, the HDR probe (`wp_color_manager_v1`, which needs the `staging`
feature), the X11 transport-drift row, and the `#124` crash-guard row.

### The one architectural item worth scheduling

Four debt rows ([D-016](debt.md#d-016), [D-040](debt.md#d-040),
[D-059](debt.md#d-059), [D-065](debt.md#d-065)) all defer on "`AppState` cannot
be constructed in a test", and the 2026-08-07 checkpoint found that reason is
out of date. `#134` removed the `tray_icon::TrayIcon` half, and the "two live
Slint shells" half was never the blocker it was written as - `duja-ui` builds
both shells headless in its own tests today, under a test backend that is
already a workspace dependency.

[D-102](debt.md#d-102) carries the re-triage and, importantly, what is *not* yet
verified. The cheap experiment it names should come before any refactor is
planned: one ignored-by-default test that calls `PlatformTray`'s constructor
headless. If it succeeds, three of those four rows close with no refactor at
all. That is an afternoon, and it decides whether a wave-sized job exists.

Do not fold this into wave 5. It touches the same file the ksni un-gate does,
and `#82` is this project's standing example of what happens when a refactor is
smuggled into a PR that was about something else.

### Wave 6 - packaging

`xtask dist` already picks a target from the host and has a macOS branch
(`#104`); Linux is the third. The decision half - artifact names, the accepted
version alphabet, the host-to-target mapping - is pure code in `xtask`'s
`bundle` and `version` modules and is unit-tested on all three lanes. Follow
that split: what is genuinely platform-bound is filesystem plumbing and
`Command` invocations, and those stay out of `cfg` blocks so every lane's clippy
still sees them.

### Wave 7 - the gate

The phase gate is not a formality and has never once returned nothing. Run it
the way P6's was run: several independent adversarial reviewers over the
cumulative `v0.2.0..main` diff, every non-low finding verified by a separate
agent before it is accepted, every accepted finding fixed test-first. P6's gate
found a blocker that had been shipping since the macOS DDC work began - every
Apple Silicon DDC request was malformed - and no per-crate suite had seen it.
[review-rubric.md](review-rubric.md) is the rubric; [history.md](history.md)
records what the P5 and P6 gates actually caught.

## How work lands

[CONTRIBUTING.md](../CONTRIBUTING.md) has the mechanics. The three rules that
are not obvious from it, and that this project has paid for:

1. **Every PR gets an adversarial review before merge**, by a reviewer that did
   not write it. This has caught every real seam defect the per-crate suites
   missed.
2. **A regression test is proven red before its fix**, and the defect is
   re-inserted **where it historically occurred** rather than where the test can
   reach it. The difference is not academic: `#82` shipped an impeccable-looking
   red-first proof that protected nothing, because the bug had been inserted
   into the one function the test called directly.
3. **A false assurance is worse than an open gap.** Deleting a debt row while
   its warning is still true, or writing a comment that asserts protection which
   does not exist, converts a tracked gap into a lie in the exact file a
   maintainer reads before re-introducing the bug.

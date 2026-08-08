# Duja - the plan

What happens next, and in what order. [STATUS.md](STATUS.md) says where the
build stands; this file says where it is going. Anything already done is
described in [history.md](history.md) rather than here, so this file stays short
enough that reading it is never a research task.

## What is left

**No phase.** P0 through P8 are all closed, and P8's write-up - six waves, the
multi-reviewer gate, and the nine reviews that every one of found something -
is in [history.md](history.md). This file carried 240 lines of that detail until
the `v0.1.6` checkpoint, which is exactly the shape its own opening paragraph
forbids; the P7 wave table was moved for the same reason one checkpoint earlier.

What is left is three things, and none of them is a phase.

### 1. The hardware runs

Duja has still never executed on a Mac or on a Linux desktop. What changed at
`v0.1.6` is that this is no longer self-blocking:
[ADR-0024](adr/0024-preview-artifacts-on-the-patch-train.md) ships both
artifacts as labelled previews, so the community confirmations
[ADR-0013](adr/0013-macos-ddc-wrap-vs-vendor.md) requires are now *obtainable*.
Before, the condition for releasing was confirmation and the mechanism for
getting confirmation was releasing.

So the next real milestone is not something this repository can do:

| what | who | unblocks |
|---|---|---|
| Run `Duja.app` on an Apple Silicon Mac | anyone with one | `v0.2.0`, [D-014](debt.md#d-014), [D-015](debt.md#d-015), [D-019](debt.md#d-019), [D-027](debt.md#d-027) |
| Run the tray on a real Linux session | anyone with one | `v0.3.0`, [D-105](debt.md#d-105), [D-106](debt.md#d-106) |
| 30 minutes on a Windows laptop | anyone with one | [D-009](debt.md#d-009), [D-021](debt.md#d-021), the v0.1.4 mirror QA |

[qa-checklist.md](qa-checklist.md) has what each run must cover, ordered so the
never-executed paths come first. `v1.0.0` needs the first two.

### 2. The AppState refactor, now unblocked

[D-102](debt.md#d-102)'s experiment has been **run**, and it settled the
question four rows deferred on. `build_tray` succeeds in a test process and all
three seam verbs work, so "`AppState` cannot be constructed in a test" is false
in both halves - measured rather than assumed.

It also falsified D-102's own prediction that three rows would then close with
no refactor. They do not: because `build_tray` *succeeds*, a naive test would
put a real icon in the real notification area and answer differently per
session. What the rows need is a **fakeable tray**, which is a third
implementation of the seam `#134` built - and so a decision against
`surface.rs`'s stated reason for making `PlatformTray` concrete per target.
That reason stops being true the moment a fake exists, and re-opening it is
step one rather than an aside.

Closing it drains [D-016](debt.md#d-016), [D-040](debt.md#d-040),
[D-059](debt.md#d-059) and [D-065](debt.md#d-065), and moves
`tray/state.rs` off the worst coverage number in the workspace. It is the
largest single piece of work left that needs no hardware.

### 3. [debt.md](debt.md)

Everything else. It is not a queue to burn down - several rows are deliberately
open, and a few record a deferral reason that later turned out to be false,
which is the part worth reading.

## The version ladder

Re-mapped twice. [ADR-0019](adr/0019-version-ladder-and-release-trains.md) set
`v0.1.x` = Windows, `v0.2.0` = macOS, `v0.3.0` = Linux, `v1.0.0` = hardening.
[ADR-0024](adr/0024-preview-artifacts-on-the-patch-train.md) supersedes its
platform rows: preview artifacts ship on the patch train, and `v0.2.0` /
`v0.3.0` now mean **hardware-confirmed** rather than first-shipped. A phase
exits on a milestone tag; a release is a separate decision from a tag.

## The phases

| phase | milestone | state |
|---|---|---|
| P0 Foundation | `m0-foundation` | done |
| P1 Spikes (risk burn-down) | `m1-spikes` | done |
| P2 Core domain (`duja-core`) | `m2-core` | done |
| P3 Windows hardware slice | `m3-win-hw` | done |
| P4 Windows dimmer + UI (MVP) | `m4-win-mvp` | done |
| P5 Power features (Windows complete) | `m5-win-full` | done |
| P6 macOS port | `m6-macos` | done, gate passed |
| P7 Linux port | `m7-linux` | done, gate run |
| P8 Hardening | `m8-hardening` | done, gate run, `v1.0.0` held |

How each went is in [history.md](history.md). P6 and P7 were both
hardware-blind by construction; P7 and P8 are the only ones written up wave by
wave.

## How work lands

[CONTRIBUTING.md](../CONTRIBUTING.md) has the mechanics. The three rules that
are not obvious from it, and that this project has paid for:

1. **Every PR gets an adversarial review before merge**, by a reviewer that did
   not write it. This has caught every real seam defect the per-crate suites
   missed, and in P8 it caught something in all nine reviews.
2. **A regression test is proven red before its fix**, and the defect is
   re-inserted **where it historically occurred** rather than where the test can
   reach it. The difference is not academic: `#82` shipped an impeccable-looking
   red-first proof that protected nothing, because the bug had been inserted
   into the one function the test called directly.
3. **A false assurance is worse than an open gap.** Deleting a debt row while
   its warning is still true, or writing a comment that asserts protection which
   does not exist, converts a tracked gap into a lie in the exact file a
   maintainer reads before re-introducing the bug.

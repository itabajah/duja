# Duja - the plan

What happens next, and in what order. [STATUS.md](STATUS.md) says where the
build stands; this file says where it is going. Anything already done is
described in [history.md](history.md) rather than here, so this file stays short
enough that reading it is never a research task.

## What is left

P0 through P8 are all closed, and their write-ups are in
[history.md](history.md). What remains splits on a line that is easy to blur and
expensive to blur: **"no hardware to write" is not "no hardware to trust".**

P6 is the standing proof. A fully green DDC codec suite described a wire no
display could answer, and the gate found it. Purity buys host-testability, not
correctness against something external. So what is left sorts into three
buckets, and only the first can be *closed* from here.

### 1. P9 - the app-layer seam and the instruments

The phase that needs no hardware this project lacks. Of the 107 rows open at the
`v0.1.6` checkpoint, about thirty are hardware-blocked and the rest are in
reach; they sort into six waves whose order is a dependency order rather than a
preference.

| wave | what | rows |
|---|---|---|
| 1 | the `AppState` seam - a fakeable `PlatformTray` | [D-102](debt.md#d-102), draining [D-016](debt.md#d-016) [D-040](debt.md#d-040) [D-059](debt.md#d-059) [D-065](debt.md#d-065) |
| 2 | the budgets that cannot fail | [D-005](debt.md#d-005) [D-109](debt.md#d-109) [D-110](debt.md#d-110) [D-111](debt.md#d-111) [D-112](debt.md#d-112) |
| 3 | caps and bounds | [D-113](debt.md#d-113) [D-114](debt.md#d-114) [D-045](debt.md#d-045) [D-076](debt.md#d-076) |
| 4 | what wave 1 unlocks | ~17 app-layer rows that today read "`AppState` cannot be constructed" |
| 5 | features with no consumer | [D-003](debt.md#d-003) [D-012](debt.md#d-012) [D-013](debt.md#d-013) [D-025](debt.md#d-025) [D-057](debt.md#d-057) [D-058](debt.md#d-058) |
| 6 | UI polish | [D-032](debt.md#d-032) and D-034 through D-039 |

**Wave 1 is the keystone** and everything in wave 4 is behind it. Wave 3's
[D-114](debt.md#d-114) is an hour and its deferral reason expired when `v0.1.6`
was tagged, so it goes first in wall-clock even though it is not first in
importance.

**Two of wave 2's rows may end in "narrow the row" rather than "build the
thing"**, and that is a legitimate outcome. [D-110](debt.md#d-110)'s honest cost
is a ~20-minute fat-LTO build against a PR matrix that finishes in a fraction;
[D-112](debt.md#d-112) as written needs the soak to drive real overlays on the
operator's screen for 24 hours. A budget line that is wrong is better deleted
than instrumented.

### 2. The hardware runs

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

### 3. Writable here, confirmable only there

A third bucket that is neither of the above, and the one this file most needs to
name. These fixes can be *written* on this box and are sound on the code's own
terms - [D-015](debt.md#d-015) says so explicitly - but nothing here can
*confirm* them: [D-018](debt.md#d-018), the macOS packaging rows D-070 and
D-072 through D-074, the Linux gamma rows D-094 through D-097 and D-099 through
D-100 (D-098 is already drained),
[D-103](debt.md#d-103), [D-104](debt.md#d-104), [D-106](debt.md#d-106),
[D-107](debt.md#d-107), and
[D-093](debt.md#d-093), which needs a design rather than a patch because the
obvious fix made it worse.

They may be worked at any time. **They do not close when they land.** Marking
one drained on the strength of a green suite is precisely the P6 failure, and a
row that overstates its own evidence is what [debt.md](debt.md) exists to
prevent.

### 4. [debt.md](debt.md)

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
| P9 App-layer seam + instruments | `m9-seam` | in progress |

How each went is in [history.md](history.md). P6 and P7 were both
hardware-blind by construction; P7 and P8 are the only ones written up wave by
wave. P6 and P7 were ports that happened to be hardware-blind; P9 is the first
phase whose *scope was chosen* by that constraint, which is why its section
above opens on the two-kinds-of-blind distinction rather than on a wave table.

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

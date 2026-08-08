# Duja - the plan

What happens next, and in what order. [STATUS.md](STATUS.md) says where the
build stands; this file says where it is going. Anything already done is
described in [history.md](history.md) rather than here, so this file stays short
enough that reading it is never a research task.

## What is left

P0 through P8 are all closed, and their write-ups are in
[history.md](history.md). What remains splits on a line that is easy to blur and
expensive to blur: **"no hardware to write" is not "no hardware to trust".**

P6 is where that was paid for. `duja-ddc`'s macOS codec suite was fully green
while the frames it built were malformed, and [the gate review](history.md#s12)
found it by reading the arm against four other implementations - no display has
ever refused those frames, because no display has ever seen them. The write-up
still records the finding as a reading of the wire rather than an observation,
which is why the macOS rows stayed open even after the fix.

So what is left sorts three ways - what P9 builds here, what needs a machine
this project does not have, and the part of the second that can be *written*
here even though it cannot be closed here - with everything not in one of those
three staying in [debt.md](debt.md), which is the fourth section below and by
some way the largest.

### 1. P9 - the seams and the instruments

**This section names what the phase builds, not which rows it drains.** Two
drafts of it did the latter and both were wrong about it, in the same way each
time: a row's deferral reason is an argument, summarising a hundred of them into
a table produces claims that read as checked and are not, and the corrections
were worse than the original. What each wave actually drains is decided by
measurement when it lands, and recorded in [debt.md](debt.md) then.

| wave | what it builds |
|---|---|
| 1 | the `AppState` test seam: a fakeable `PlatformTray`, and a fixture that constructs the state |
| 2 | the same for the gamma channel, which is what the ordering and re-assert properties need |
| 3 | the instruments behind the performance budgets |
| 4 | the config cap's write side, and the bounded waits |

**Wave 1 is the keystone**, and it is the one place a row list is safe, because
[D-102](debt.md#d-102) has already done the counting: exactly four rows -
[D-016](debt.md#d-016), [D-040](debt.md#d-040), [D-059](debt.md#d-059),
[D-065](debt.md#d-065) - defer on the single sentence "`AppState` cannot be
constructed in a test", and D-102 has shown that sentence false in both halves.
`tray/state.rs` sits at **11.27 %** of regions, which is the largest uncovered
*surface* in the workspace at 1,031 regions rather than the lowest percentage;
seven files are at 0.00 %, all of them smaller by both measures. That
distinction is why it is the target and not one of them.

**Wave 3's rows are budgets that cannot fail**, and some of that work may
honestly end in "narrow the row" rather than "build the thing".
[D-110](debt.md#d-110)'s cost is a ~20-minute fat-LTO build against a PR matrix
that finishes in a fraction; [D-112](debt.md#d-112) as written needs the soak to
drive real overlays on an operator's screen for 24 hours, and offers a cheap
half instead. A budget nobody can fail is worse than no budget, and an
instrument nobody will run is worse than both - so deleting a budget line is a
legitimate outcome of this wave and is not a failure of it.

**What P9 does not include is anything whose deferral names something the phase
cannot produce**: beta reports, a locale pass, a display. Those rows stay in
[debt.md](debt.md), which is where a reader should go for them rather than to a
summary here.

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

Not a third pile: this is drawn **out of the section above**, and saying so is
the point. A row can be listed under a hardware run and worked on today, because
writing a remedy and trusting it are different events on different days.
[D-015](debt.md#d-015) is the clearest case - the row says its remedy "is the
one remedy that does not need a Mac to *write* (it still needs one to confirm)"
- and it is under the macOS run above for the confirmation half.
[D-106](debt.md#d-106) is adjacent rather than identical: its `probe_session`
deadline is writable here too, but its row says a startup probe needs a
*reported* degradation rather than a silent one, "which is a different design
than this row's sibling took", so a design call comes first.

A sample rather than a census: parts of the Linux gamma cluster around D-094 to
D-100 are of this kind too. [D-093](debt.md#d-093) is a third shape again - it
needs a design rather than a patch, because P8 wave 4 tried the obvious fix and
a review caught it making things worse.

**They do not close when they land.** Marking one drained on the strength of a
green suite is the P6 shape exactly, and a row that overstates its own evidence
is what [debt.md](debt.md) exists to prevent.

Two rows are deliberately **not** here, because their own text rules the writing
out rather than the confirming. [D-107](debt.md#d-107) says a native package
built from a machine that has never run the binary "would be a guess presented
as a supported package, which is the false-assurance shape this project rates
worse than an admitted gap", and fixes the ordering: archive, then hardware,
then package. [D-104](debt.md#d-104) makes the softer version of the same
argument about a `StatusNotifierItem` tooltip no lane can host.

### 4. [debt.md](debt.md)

Everything else, and it is by some way the largest of these. It is not a
queue to burn down - several rows are deliberately open, and a few record a
deferral reason that later turned out to be false, which is the part worth
reading.

## The version ladder

Re-mapped twice. [ADR-0019](adr/0019-version-ladder-and-release-trains.md) set
`v0.1.x` = Windows, `v0.2.0` = macOS, `v0.3.0` = Linux, `v1.0.0` = hardening.
[ADR-0024](adr/0024-preview-artifacts-on-the-patch-train.md) supersedes its
platform rows: preview artifacts ship on the patch train, and `v0.2.0` /
`v0.3.0` now mean **hardware-confirmed** rather than first-shipped. A phase
exits on a milestone tag; a release is a separate decision from a tag.

**P9 does not move the ladder**, which is why ADR-0019 needs no amendment for
it - and the reason is ADR-0024 rather than anything about where P9's code
lives. Some of that code is macOS and Linux; what used to make that imply a
platform release was the old mapping, and ADR-0024 re-mapped `v0.2.0` and
`v0.3.0` to mean **hardware-confirmed**. Touching a platform's code no longer
moves its version, so P9 lands on `v0.1.x` like any other patch and `m9-seam`
is a phase tag rather than a version.

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
| P9 App-layer seam + instruments | `m9-seam` | planned |

How each closed phase went is in [history.md](history.md); P9 gets its entry
when it closes. P7 and P8 are the only ones written up wave by wave. P6 and P7
were hardware-blind by construction - they were ports to machines nobody here
has - whereas P9 is the first phase whose *scope was chosen* by that constraint
rather than merely limited by it.

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

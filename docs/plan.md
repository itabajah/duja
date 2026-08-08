# Duja - the plan

What happens next, and in what order. [STATUS.md](STATUS.md) says where the
build stands; this file says where it is going. Anything already done is
described in [history.md](history.md) rather than here, so this file stays short
enough that reading it is never a research task.

## What is left

P0 through P8 are all closed, and their write-ups are in
[history.md](history.md). What remains splits on a line that is easy to blur and
expensive to blur: **"no hardware to write" is not "no hardware to trust".**

P6 is where that was paid for, though not in the way it is usually retold.
`duja-ddc`'s macOS codec suite was fully green while the frames it built
disagreed with the reference implementations it was modelled on, and what caught
that was the gate *reading* four of them side by side - no display has ever
refused those frames, because no display has ever seen them. So the lesson is
sharper than "test on hardware": the green suite was not evidence, and what
substituted for the hardware was a review rather than a better test.
[`#106`'s write-up](history.md#s12) still records the finding as a reading of
the wire rather than an observation, which is why the macOS rows stayed open.

So what is left sorts as follows: what P9 can finish here, what needs a machine
this project does not have, the overlap between them, and the remainder.

### 1. P9 - the app-layer seam and the instruments

The phase that can be *finished* here. It is deliberately narrow: every row
below was checked against its own "Why deferred" rather than against a guess,
and the ones whose deferral names something P9 cannot produce - beta reports, a
locale pass, a design call that is the maintainer's to make, a display - were
left in [debt.md](debt.md) rather than given a wave they would not close in.

| wave | what | rows |
|---|---|---|
| 1 | the `AppState` seam - a fakeable `PlatformTray` | [D-102](debt.md#d-102), draining [D-016](debt.md#d-016) [D-040](debt.md#d-040) [D-059](debt.md#d-059) [D-065](debt.md#d-065) |
| 2 | the budgets that cannot fail | [D-109](debt.md#d-109) [D-110](debt.md#d-110) [D-111](debt.md#d-111) [D-112](debt.md#d-112), then [D-005](debt.md#d-005) |
| 3 | caps and bounds | [D-113](debt.md#d-113) [D-045](debt.md#d-045) [D-076](debt.md#d-076); [D-114](debt-archive.md#d-114) is drained |
| 4 | the hardware-free hoists | [D-018](debt.md#d-018) [D-034](debt.md#d-034) [D-070](debt.md#d-070) |

**Wave 1 is the keystone.** It is also the only thing that drains those four
rows, all of which defer on one sentence - "`AppState` cannot be constructed in
a test" - that [D-102](debt.md#d-102) has already shown to be false in both
halves. `tray/state.rs` sits at **11.27 %** of regions, which is the largest
uncovered *surface* in the workspace at 1,031 regions rather than the lowest
percentage; seven smaller files are at 0.00 %. That distinction is the reason
this is the target and not one of them.

**Wave 2's rows are budgets that cannot fail**, and two of them may honestly end
in "narrow the row" rather than "build the thing". [D-110](debt.md#d-110)'s cost
is a ~20-minute fat-LTO build against a PR matrix that finishes in a fraction.
[D-005](debt.md#d-005) is sequenced last because its own deferral says to
revisit "when the P8 soak numbers set a real threshold", and 90 seconds is not
that - so it waits on [D-111](debt.md#d-111) and may then turn out to be a line
worth deleting rather than tuning. A budget nobody can fail is worse than no
budget, and an instrument nobody will run is worse than both.

**Wave 4 is three rows that say, in their own text, that the fix needs no
hardware.** [D-018](debt.md#d-018) calls the hoist "a small, hardware-free
change and the natural next step" - `duja-panel` already solved the identical
problem with a pure function that reds on the same swap.
[D-070](debt.md#d-070)'s preferred remedy of the three it lists is the pure
`xtask` PNG encoder, "unit-testable on every lane"; only the `sips`/`iconutil`
option it rejects needs a Mac. What a Mac would still settle is whether Finder
*renders* the result, so this one lands with tests and its confirmation line
stays open.

**There is no wave for "what the seam unlocks", and there was one in the first
draft of this file.** It claimed ~17 app-layer rows wait on `AppState`. Four do,
and wave 1 drains all four. The rest - D-006, D-007, D-008, D-033, D-043, D-044,
D-050, D-053, D-060, D-062, D-063, D-064 - defer on design decisions, absent
consumers, or a hardware tolerance, and not one of them mentions `AppState`. The
number was inferred from a theme rather than counted, which is the defect class
this file's last rule is about.

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

Not a fourth pile: this is the **writable subset of the section above**, and
saying so is the point. A row can appear in both because writing a remedy and
trusting it are different events on different days.
[D-015](debt.md#d-015) is the clearest case - the row itself says its remedy "is
the one that does not need a Mac to write (it still needs one to confirm)" -
and it is listed under the macOS run in the table above for the confirmation
half. [D-106](debt.md#d-106)'s `probe_session` deadline is the same shape on the
Linux side.

Others of this kind, and this is a sample rather than a census: parts of the
Linux gamma cluster (D-094 through D-097, D-099, D-100 - D-098 is already
drained), and the Linux tray and platform rows around them.
[D-093](debt.md#d-093) is adjacent but different: it needs a *design* rather
than a patch, because P8 wave 4 tried the obvious fix and a review caught it
making things worse.

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
it. Its work is app-layer and tooling on the Windows train, so it lands on
`v0.1.x` like every other patch; `m9-seam` is a phase tag and not a version.

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

How each went is in [history.md](history.md). P7 and P8 are the only ones
written up wave by wave. P6 and P7 were hardware-blind by construction - they
were ports to machines nobody here has - whereas P9 is the first phase whose
*scope was chosen* by that constraint rather than merely limited by it. Which is
why its section above is short: most of what a maintainer might expect to find
in it is in [debt.md](debt.md) instead, on purpose.

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

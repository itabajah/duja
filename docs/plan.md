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

**This section names what the phase builds, not which rows it will drain.** Two
drafts of it did the latter and both were wrong about it, in the same way each
time: a row's deferral reason is an argument, summarising a hundred of them into
a table produces claims that read as checked and are not, and the corrections
were worse than the original. What a wave *turned out* to drain is a different
thing - it is measured when the wave lands, and recorded below once it has.

The **state** column is about what the wave builds, not about rows: wave 3 is
"landed" with all four of its rows still open, and wave 4 is "partly landed"
because one thing it was for is unbuilt. Reading it as a drain count is what let
a first version of this close-out write "all four waves have landed" into
`STATUS.md` next to a row saying "partly".

| wave | what it builds | state |
|---|---|---|
| 1 | the `AppState` test seam: a fakeable `PlatformTray`, and a fixture that constructs the state | landed |
| 2 | the same for the gamma channel, which is what the ordering and re-assert properties need | landed |
| 3 | the instruments behind the performance budgets | landed |
| 4 | the config cap's write side, and the bounded waits | partly landed |

**Wave 1 is the keystone**, and it is the one place a row list is safe, because
[D-102](debt.md#d-102) had already done the counting: exactly four rows -
[D-016](debt-archive.md#d-016), [D-040](debt-archive.md#d-040),
[D-059](debt.md#d-059), [D-065](debt-archive.md#d-065) - deferred on the single
sentence "`AppState` cannot be constructed in a test", and D-102 showed that
sentence false in both halves. `tray/state.rs` sat at **11.27 %** of regions,
the largest uncovered *surface* in the workspace at 1,031 regions rather than
the lowest percentage; seven files are at 0.00 %, all smaller by both measures.
That distinction is why it was the target and not one of them.

**Waves 1 and 2 have landed, and between them drained three of the four.**
D-040 drained on the `AppState` fixture; D-016 and D-065 on the recording gamma
channel wave 2 added, which is the seam wave 1's own write-up predicted they
would need. **D-059 is still open**, and it needs neither: it wants to observe
*when* `build_tray` ran relative to the loop, which a constructible state is
orthogonal to. Which is this section's warning seen from the inside - a wave
drains what it turns out to drain, and here the plan happened to be right about
all three.

`tray/state.rs`'s **uncovered regions went 1,031 to 823**, and what each PR
contributed is given as a delta rather than a total, because three of P9's PRs
add tests to this one file:

| PR | wave | uncovered regions removed |
|---|---|---|
| `#157` | 1 | 108 |
| `#158` | 4 | 37 |
| `#159` | 2 | 63 |

**Read the merge order, not the wave numbers.** Those PRs landed 157, 158, 159,
so the cumulative percentage after each is 32.28, 40.58 and 48.91 - and `#159`'s
own commit message says *42.01 %, uncovered 860*, which is neither wrong nor
this. It measured on a branch cut from `#157`, before `#158` existed; its
**delta of 63 regions is the same either way**, which is why a delta is what
belongs here.

The **uncovered count is the honest column** and the percentage is the flattered
one. That mechanism is [D-102](debt.md#d-102)'s - a fixture's own body counts as
covered - though the figures are measured here rather than there: the file grew
from 1,162 regions to 1,611, so against the original denominator 823 uncovered
is nearer 29 %.

A first version of this paragraph bolded 48.91 %, credited the whole rise to
waves 1 and 2 when `#158` produced roughly a fifth of it, and dropped the
caveat. Three mistakes in one sentence about measurement. (An intermediate
version then put "8 of the 37 points" near a table whose `#158` row reads 37 -
two different 37s, one percentage points and one regions.)

**Wave 3's rows are budgets that cannot fail**, and some of that work may
honestly end in "narrow the row" rather than "build the thing".
[D-110](debt.md#d-110)'s cost was written as a ~20-minute fat-LTO build against
a PR matrix finishing in a fraction, and **wave 3 measured both halves and found
neither**: the build is 6m22s on average and the matrix 4m12s, so it is about
1.5x rather than a multiple, and "a fraction of that" is the clause that does
not survive. That does not settle the row - see D-110 for what it does and does
not change - but the version of the cost this sentence used to give is gone;
[D-112](debt.md#d-112) as written needs the soak to drive real overlays on an
operator's screen for 24 hours, and offers a cheap half instead. A budget nobody
can fail is worse than no budget, and an instrument nobody will run is worse
than both - so deleting a budget line is a
legitimate outcome of this wave and is not a failure of it.

**What P9 does not include is anything whose deferral names something the phase
cannot produce**: beta reports, a locale pass, a display. Those rows stay in
[debt.md](debt.md), which is where a reader should go for them rather than to a
summary here.

**Six rows have drained so far**, taking `debt.md` from 107 to 101: wave 1's
[D-040](debt-archive.md#d-040), wave 2's [D-016](debt-archive.md#d-016) and
[D-065](debt-archive.md#d-065), and wave 4's [D-113](debt-archive.md#d-113) and
[D-045](debt-archive.md#d-045). This is the recording the section's opening
defers to, and it is why the **wave table** carries no rows: what a wave turns
out to drain is written down here, after it lands.

The sixth, [D-114](debt-archive.md#d-114), belongs to **no wave** - it landed
seven minutes before this plan did, and its review had already struck one
attempt to file it under P9. It is counted here because it is P9-era work, not
because the wave table accounts for it.

**Wave 3 has landed, in two instruments, a workflow and an experiment**, and it
left every one of its four rows open. [D-109](debt.md#d-109) narrowed;
[D-112](debt.md#d-112) closed the half its own row calls cheap;
[D-111](debt.md#d-111) got the mechanism its remedy asks for and then the
reading; and [D-110](debt.md#d-110), which built nothing, had both numbers in
its deferral argument measured for the first time. That is the wave working as
intended rather than falling short: an instrument row drains when the budget it
serves can be checked, and three of these budgets still need hardware or a day
of wall clock that CI cannot give.

What is left of the phase is [D-076](debt.md#d-076) from wave 4 and
[D-059](debt.md#d-059), which wave 1 turned out not to touch.

**Wave 3 opened on [D-109](debt.md#d-109), which narrowed rather than drained**,
and the reason is the shape this section keeps meeting: the row proposed a
software-renderer harness as the remedy for the two budgets it names, and the
harness closes neither. The overlay is a Win32 layered window rather than a
Slint surface, and a cold start to a tray icon needs a session. What the harness
does measure is the frame path P8 wave 1 exempted from `opt-level = "s"` by
name, which is the exposure the row was arguing about even though it is not the
budget the row cited. The exemption had never been measured, is worth roughly
1.3x to 1.4x, and the budget clears by a wide margin either way - about 65x on a
typical frame - so the argument was right and nothing depended on it. (Both
figures read 1.4x and 70x here until this close-out, against `debt.md`'s and
`perf-budgets.md`'s 1.3x-to-1.4x and 65x. The first pair were published
together, and only the other two files were corrected when the re-measurement
widened the ratio and *narrowed* the headroom.) `perf-budgets.md`
gains a row that has an instrument; the three that do not, still do not.

**And that instrument's *timing* assertion runs nowhere automatically.** It is
`#[ignore]`d on purpose, for two reasons the test's own doc keeps apart: a
shared runner under unknown load is not where a duration gate belongs, and
[D-110](debt.md#d-110)'s lesson is the other one - gating on a number nobody has
measured is how a check becomes a thing people disable. So the budget is checked
by hand. What does run on every push is the harness's correctness:
that the real flyout renders, at the size the app presents, with content
reaching the buffer. Worth stating in a paragraph arguing that rows drain when a
budget can be checked, because this one can be checked and is not being.

**[D-112](debt.md#d-112)'s cheap half is counted now**, and the row's argument
became a measurement: a headless soak on this box reports GDI 0 and USER 5
because it builds no GUI objects, and around 250 *kernel* handles because it
builds a pipe server, a log file and threads. Before it, a leaked pipe instance
per connection could have run for a day and reported a clean PASS. Linux gained
a handle signal it never had, since `GetGuiResources` has no counterpart there.

**And [D-111](debt.md#d-111)'s experiment ran**, which is the one result of this
wave nobody could have predicted from the code. `--soak 120 --every 10` on all
three lanes: the pump, the engine and the IPC server come up on every one,
including the two with no display server and no session. The first Linux RSS
figure this tree has ever had is 9,981,952 bytes against Windows' 16,228,352,
and the Windows CI number lands inside the range measured on the dev box - the
first time this instrument has been checked against a machine nobody tuned it
on. macOS assembles and measures nothing, which is what it is documented to do.

**Two of the wave's own fixes were caught by its own instruments.** The soak's
first real kernel reading printed `0` for a count that had moved by nine; and
the CI run showed USER moving 5 to 6 on a runner while several places in the
tree said it was flat at 5. Both are the same shape, and it is the shape this
phase exists to remove: a number that looks measured and is not.

*(A first version of that sentence said "three places". That number was written,
found short, corrected to a different wrong number, and then deleted from
`debt.md` for being an undercount inside the sentence written to fix an
undercount - all inside wave 3. Reinstating it here, in the paragraph arguing
against numbers that look measured, is the joke writing itself, and it is
recorded rather than quietly fixed.)*

**A third fix was caught by a review instead, and it is the one worth reading.**
The paragraph below has it; the only thing to add is that a first version of
this summary counted it among the instrument catches, which gets the lesson
exactly backwards.

**The first version of that measurement was wrong, and a review caught it.** The
probe sized its window from the markup's default rather than from the height the
app presents, so a third monitor's card fell off the bottom edge and a two-card
flyout was timed and published as three. Both of the harness's own "did it draw"
checks passed throughout - one re-asserted the size the probe itself passed in,
the other compared every pixel against a rounded corner and so counted 98 per
cent of any buffer as content. A check that cannot fail is the same defect as no
check, wearing better clothes, and this phase has now produced one in code as
well as in prose.

**Two things are worth carrying out of it, and neither is a row.** Every PR was
reviewed adversarially and every review found something real - which by now is
unremarkable. What is not: **on all six, a later round found a defect an earlier
round's correction had introduced.** Not four of six, which is what a first
version of this paragraph said - an unmeasured count, inside the paragraph
arguing that counts must be measured, which is precisely what `#156`'s review
convicted [D-114](debt-archive.md#d-114) of. The evidence is spread across
review comments, squashed commit messages and the archived rows themselves -
`#155` and `#160` carry no PR comments at all - so it is collected here rather
than left as a pointer:

| PR | what a later round found |
|---|---|
| `#155` | nine of round 2's seventeen findings were introduced by round 1 |
| `#156` | round 2 found seven, **two of them round 1's**; round 3 then found defects in round 2's corrections in turn |
| `#157` | round 1's correction replaced a **true** quotation with a false one and stamped it "measured" |
| `#158` | its commit message: three rounds, "each found a defect the previous round's correction introduced" - the second of the three was self-found, between the two posted reviews |
| `#159` | a correction landed in one of the two places it claimed; and a vacuous test's **replacement** was vacuous too |
| `#160` | the first repair of a mangled log line moved the gap rather than closing it; a correction described an edit that had not been made |

Among them: a fix for one data-loss path that opened another, and tests that
launched a browser and made a live network call one commit after a seam for the
same hazard landed - both `#158`. A correction stamped *measured* that was not,
in `#157`. A claim about compiling on all three lanes that broke two of them, in
`#160`.

Three more, all of a kind a green gate cannot report. `#157`'s first fixture was
**green on CI and red locally**. `#159` shipped two vacuous tests, the second
written as the fix for the first. And `#156`'s round 3 found a **live bug on the
tool that gates releases**: `cargo xtask size --target ""` joined to
`target/release` and measured whatever the last host build had left there, which
`release_dir`'s own doc calls the one failure mode a size check must not have.
The suite was green throughout. That one belongs here more than the other two,
because it could have shipped a wrong artifact, and a first version of this
paragraph left it out.

Corrections are about as defect-prone as the work they correct, and only
measurement tells them apart.

**Seven, counting this file.** Its review rounds are not counted here. The
count that was here was accurate when written, stale one round later, and wrong
the moment it was corrected, so what replaces it is nothing. The first round
found defects of the original draft - a coverage figure credited to the wrong
waves, an unmeasured count. **Every round after that found a defect the round
before it had introduced.** Among them: a table whose rows disagreed with a
merged commit message; a stale percentage in the same sentence as the stale
count it was correcting; a false "every one staled" in the footnote written to
be the accurate account of all that; and a de-duplication that deleted the
corrected copy of a paragraph and kept the false one.

The per-file counts are deleted rather than corrected again, which is
[D-114](debt-archive.md#d-114)'s own rule arriving late. A document
recording that corrections are defect-prone had no business being an exception,
and was not.

The second thing follows from it: **`#82`'s rule caught something again.**
[D-045](debt-archive.md#d-045)'s red-first proof pinned a pure function the fix
itself introduced, which never carried the bug - the historical defect, restored
at the site it occupied, leaves the suite green. That row now says so rather
than reading as protected.

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
| P9 App-layer seam + instruments | `m9-seam` | in progress - 6 rows drained |

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

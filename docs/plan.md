# Duja - the plan

What happens next, and in what order. [STATUS.md](STATUS.md) says where the
build stands; this file says where it is going. Anything already done is
described in [history.md](history.md) rather than here, so this file stays short
enough that reading it is never a research task.

## What is left

1. **P8** - hardening to `v1.0.0`. Six waves, in [the table below](#p8-waves).

That is the whole list of *phases*. Every one before it is closed, and the two
that closed most recently each left a tag without a release: `m6-macos`
(`v0.2.0` held) and `m7-linux` (`v0.3.0` held). [STATUS.md](STATUS.md) has why.

Three things are **held rather than pending**, and none of them blocks P8:

- **`v0.2.0` (macOS)** and **`v0.3.0` (Linux)**, each waiting on one person
  running the build on the hardware it targets. A decision, not a blocker.
- **Laptop QA of the v0.1.4 mirror/software-only behaviour**, and a regenerated
  `social-preview.png`. Both carried from the Windows train, both need a human.

And one row is open and **unscheduled**, which is different from held:
[D-106](debt.md#d-106) - the tray's X11 path is bounded and no other one is. It
is not in a wave below because the honest next step for it (`probe_session`)
needs a *reported* degradation rather than the silent fallback its sibling took,
and that is a design job rather than a hardening one.

### What P8 cannot do, said before what it can

[ADR-0019](adr/0019-version-ladder-and-release-trains.md) defines `v1.0.0` as
"fuzz burn-in, soak, size/perf budgets met, packaging, **cross-platform hardware
sign-off**". Duja has never run on a Mac or on a Linux desktop, and no amount of
work in this repository changes that clause.

So plan for the outcome now rather than discovering it at the gate: **P8 ends
with `m8-hardening` tagged and `v1.0.0` held**, on the same terms and for the
same reason as the two tags before it. What P8 *can* do is make the hold the
only thing standing in the way - every other clause of that definition met and
measured, so that whoever borrows a Mac and a Linux box is running a build that
needs nothing else.

The corollary is a scheduling one, and it is why the waves are ordered the way
they are: **nothing in P8 should be sequenced behind hardware.** A wave that
cannot finish without a machine this project does not have is a wave that closes
by being re-triaged, and this plan has enough of those already.

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
| P7 Linux port | `m7-linux` | done, gate run, `v0.3.0` held |
| **P8 Hardening** | `m8-hardening` / `v1.0.0` | **in progress** |

P6 was hardware-blind by construction (CI runners plus community verification)
and P7 turned out the same way. How each phase actually went is in
[history.md](history.md), including P7's wave table, which used to live here.
P7 is the only phase written up wave by wave; the earlier ones are recorded by
feature area.

## P8 waves

| wave | scope | state |
|---|---|---|
| 1 | binary size: measure first, then trim, then gate it ([D-011](debt-archive.md#d-011), [ADR-0012](adr/0012-binary-size-budget-variance.md)) | done - `#144`, D-011 drained |
| 2 | the fuzz and coverage lanes ([D-002](debt-archive.md#d-002), [D-023](debt-archive.md#d-023)) | done - `#145` |
| 3 | `--soak`, the harness two perf budgets already cite | done - `#146` |
| 4 | the debt drain (`refactor:` PR, the rubric's ~15% time-box) | **partial** - `#147`, see below |
| 5 | the security pass and the docs-truth sweep | done - `#148` |
| 6 | the phase gate - **the multi-reviewer one** - and `m8-hardening` | pending |

Waves 1, 2 and 3 are independent of each other and can land in any order. Wave 4
is **not** independent of wave 3: one of its rows ([D-005](debt.md#d-005)) is
deferred until the soak produces a real error-rate threshold, so that row waits
even though the rest of the wave does not. Wave 5 wants 1 through 4 landed,
because half of what it checks is whether the docs still describe what those
waves left behind. Wave 6 is last by definition.

### Wave 1 - the binary, and checking the ADR's reasoning before following it

[ADR-0012](adr/0012-binary-size-budget-variance.md) raised the budget to 16 MB
at P4, P5 blew through it, and the ledger has said "P8 must recover it" for two
releases. `duja.exe` is **19,446,784 bytes** today. That is the whole of the debt.

The ADR lists levers in expected-payoff order **twice**, and the two lists are
not the same list: one in its *Decision* section (fat LTO, Slint image formats,
`env-filter`, and `panic = "unwind"` marked explicitly as not a lever) and a
second in its *Ledger* (feature-gate the update check, fat LTO, `env-filter`,
Slint image formats). Every reference below is to the **Decision** list, by name
rather than by number, because "lever 2" means different things in the two and
striking the wrong one would remove fat LTO.

**Do not start with the levers, and do not start with `cargo tree` either.** The
baseline attribution is already done, and it cost one wrong answer on the way,
which is the part worth writing down.

`cargo tree -p duja-app -e normal --target x86_64-pc-windows-msvc -i resvg`
appears to report that `resvg` reaches the graph only through
`i-slint-compiler` behind `slint-macros` - a proc macro, so host code, so not one
byte of `duja.exe`. That answer is **false**: `cargo bloat` puts `usvg` at
470.9 KiB of `.text` and `resvg` at another 141.6.

The mechanism is not deduplication, which is what this section said first and
what a reviewer disproved by running the command. **Adding `--target` makes
`cargo tree` print one root tree per feature-resolution universe** - the host
one, holding build scripts and proc macros, and then the target one - and it
prints them back to back under the same heading with only a blank line between.
The proc-macro tree comes first. Reading the first tree and stopping is the whole
of the error, and `--no-dedupe` does not fix it because nothing was deduplicated:
both trees were always there. `cargo tree -e normal -i resvg` **without**
`--target` prints the runtime path first and does not have the problem at all.

So the rule is: count the roots before reading the branches, and treat any
dependency question answered from the tree as a hypothesis until a linker
confirms it.

So ADR-0012's list of causes is right where it was doubted. What is genuinely
absent from the binary is the set nobody suspected: `ravif`/`rav1e` (an AV1
*encoder*, by some distance the largest thing in `Cargo.lock`), `exr`, `tiff`
and `qoi` - all of them reaching only the compiler, because `image-default-formats`
is already off.

**And one of the ADR's levers does not exist.** The Slint image-format one -
"the flyout uses no SVG/EXR/animated images - investigate disabling the decoder
stack Slint pulls by default" - has an answer:
`slint/std` implies `i-slint-core/std`, which implies `image-decoders` **and**
`svg`, with no seam between them. The formats that *were* optional are already
disabled. Removing the rest means patching Slint, which is not a hardening
change. The lever gets struck from the ADR rather than left there for the next
person to spend a day on.

That leaves three levers, and only one of them is a dependency change:

| lever | what it removes | measured `.text` |
|---|---|---|
| drop `env-filter` | `regex-syntax`, `regex-automata`, `matchers` | 345 KiB |
| feature-gate the update check | `rustls`, `ring`, `ureq`, `rustls-webpki`, `webpki-roots` | 724 KiB |
| fat LTO | nothing; it is a profile change | n/a, see below |

The middle row has a catch that decides whether it is worth doing at all.
[D-011](debt-archive.md#d-011) frames it as "so a *lite* build drops both" the TLS stack
and the WinRT toast bindings - and a feature that is **on by default in the
shipped build saves the shipped build nothing**. It creates the possibility of a
smaller artifact nobody currently builds. So it is not a lever against this
budget unless a lite artifact is also a decision, and this wave is not the place
to make that one. The 724 KiB stays in the table as the size of a choice, not of
a saving.

Fat LTO has no `.text` figure because it removes no crate; the ADR records -1.0
MB at P4 and the wave re-measures it. `.text` is 11.3 MiB of an 18.5 MiB file,
so a crate that leaves takes its read-only data with it and the *file* delta
should exceed the `.text` column - a prediction to check rather than assume.

**Apply each lever alone, with a number beside it**, because one combined diff
that lands several megabytes teaches nothing about which lever to reach for
next time.

What the wave owes when it is done:

- ADR-0012 **corrected in place**: lever 2 struck with the reason, the
  `cargo bloat` attribution in the ledger, and the `cargo tree` dedupe trap
  recorded where the next person will hit it. The ADR is what they read first.
- **Per-lever deltas**, in bytes, so the ledger stops being a list of guesses.
- The unit ambiguity settled. The budget says "16 MB" and the ledger's rows say
  14.9 and 17.21 with no unit named. 16 MiB and 16 MB differ by 5%, which is
  larger than the smallest lever on the list, so a budget that does not say
  which one it means cannot be missed *or* met on purpose.
- **A size gate that fails a build on a regression.** This is the part that
  matters after the wave ends. Size drifted from 14.9 to 19.4 across two
  releases with nothing to notice it, and a lever pulled once is a lever any
  dependency bump can give back.

  Where it runs is a real trade rather than a detail. The measurement is only
  meaningful on the profile that ships - fat LTO, one codegen unit - and that
  build is roughly twenty minutes on a hosted Windows runner, against a PR
  matrix that finishes in a fraction of that. Gating the *release* costs nothing
  and makes shipping over budget impossible; gating every PR catches the
  dependency bump on the PR that lands it and slows every other PR to do it.
  Whichever the wave picks, the half it does not pick is a debt row rather than
  an unstated gap - and if it does add a PR job, note that branch protection's
  required checks are a repository setting, so a new job is advisory until
  somebody turns it on.

### Wave 2 - the fuzz and coverage lanes

[D-002](debt-archive.md#d-002) has been open since P2 and names two workflows;
[D-023](debt-archive.md#d-023) names the sixth fuzz target and says to land it with
them, which is right - a target nothing runs is a file, not coverage.

- **`fuzz_config_toml`.** `config.toml` is user-editable and parsed through
  chained `toml_edit` migrations. It is the one untrusted-parse surface with no
  fuzz target; caps, EDID, quirks, IPC frames and DDC packets all have one.
- **`fuzz.yml`.** A weekly nightly burn over all six targets, *plus* a cheap
  PR-time `cargo check` of the fuzz workspace. The second half is the part worth
  arguing for: `fuzz/` is a separate workspace, so nothing in the normal CI
  matrix compiles it, and a rename in `duja-core` breaks a target silently until
  the next Sunday. Checking it on the PR that breaks it costs seconds.
- **`coverage.yml`.** The rubric asks for core >= 90% and ipc/view-models >= 85%.
  Wire `cargo llvm-cov` and report per-crate. Whether the thresholds *gate* is a
  decision for the wave to make against the first real number, not now - and
  either way it must not become a twelfth required check that a flaky
  instrumentation run can block a merge on.

### Wave 3 - `--soak`, which the budgets already assume exists

[perf-budgets.md](perf-budgets.md) names `--soak` twice: as the instrument for
"Idle RSS (flyout closed)" and as the whole of "Soak (24 h) RSS growth < 5 MB;
flat GDI/USER handle counts". **There is no `--soak` flag.** `--stress` exists
and does something else (a DDC input flood).

That makes two hard budgets unmeasurable by the method their own row cites,
which is the [false-assurance](#how-work-lands) shape this project has a rule
against: a maintainer reads the row, believes the budget is checked, and it
never has been. Either the harness exists or the rows are wrong; building it is
the better half of that choice, because ADR-0019 puts a soak in the definition
of `v1.0.0` and something has to produce that number.

Scope it to what a fake backend can drive unattended: RSS and handle counts
sampled on a fixed cadence, a growth verdict against the budget, and an exit
code. It runs on the dev box for the long burn, and a short one belongs in CI.

### Wave 4 - the debt drain

The rubric time-boxes this at ~15% of the phase. Rows are picked for being
*fixable without hardware*, per the scheduling rule above.

- **[D-108](debt-archive.md#d-108) first**, because it is the one whose damage lands on
  a bystander: every clean quit writes identity gamma to *every* display, so
  quitting Duja flattens f.lux, redshift or a calibration curve it never
  touched. Test-first, red proven before the fix, and the defect re-inserted
  where it historically occurred rather than where the test can reach it.
- **[D-102](debt.md#d-102)'s cheap experiment.** One `#[ignore]`d test that
  constructs `PlatformTray` headless. If it passes, three of the four rows that
  defer on "`AppState` cannot be constructed in a test"
  ([D-016](debt.md#d-016), [D-040](debt.md#d-040), [D-059](debt.md#d-059),
  [D-065](debt.md#d-065)) close with no refactor at all. It is an afternoon, and
  it decides whether a wave-sized job exists. Run it *before* planning any
  refactor, not after.
- **[D-005](debt.md#d-005)** - the `--stress` gate reports FAIL on a run with a
  single transient DDC error, which real hardware produces 1-2 of per ~300
  inputs. Its own deferral says to revisit "when the P8 soak numbers set a real
  threshold", and wave 3 is where those numbers come from, so this row is
  sequenced after it rather than beside it.
- **[D-093](debt.md#d-093)** - `WAYLAND_SOCKET` is not consulted, so a client
  handed a compositor socket is classified X11. Pure, testable, no session
  needed.

**What wave 4 actually landed, and what it did not.** `#147` drained
[D-108](debt-archive.md#d-108) and re-opened [D-093](debt.md#d-093) with a cause
rather than closing it. It did **not** do the other two items above, and marking
the wave "done" while two of its four rows were untouched is the kind of drift
this file exists to prevent - so the table says **partial**.

The omission that matters is [D-102](debt.md#d-102)'s experiment, because it is
not independent of D-108: that fix is tested at its seam and **not** at
`begin_quit`, and the reason given for that is the very sentence D-102 records as
out of date. The experiment is an afternoon and it decides whether the gap is
real. It should be the next thing anybody does here.

**[D-021](debt.md#d-021) is explicitly not in this wave.** Unifying `windows`
0.58 to 0.62 in `duja-panel` means rewriting VARIANT-era COM against an API that
changed, in the one module whose set-path has never executed on real hardware.
Its deferral note already says to do it in the pass that also runs WMI on a
borrowed laptop. That laptop is the same one three other rows wait on; the row
stays open and the reason stays written down.

### Wave 5 - the security pass and the docs-truth sweep

The [rubric](review-rubric.md) singles P5 and P8 out for the **full SECURITY.md
checklist, item by item** rather than the summary skim every other phase gets.
Do that, and record what was checked rather than only what failed.

Then the truth sweep, which is cheap and catches the class of drift this project
keeps paying for. The instance this wave started from: `SECURITY.md` described
the release as three artifacts and invited provenance verification on "any of the
three", when P7 wave 6 had made it four. Nobody edited the security policy,
because the tarball landed in `xtask` and the release workflow and there was no
reason to look there - and `release.yml`'s own comment carried the same stale
count, which is how far that kind of drift travels. Both fixed in `#148`; the
write-up is in [history.md](history.md).

### Wave 6 - the gate

**The multi-reviewer gate, run properly this time.** P7's was one targeted pass,
and both [history.md](history.md) and its tag message say so at the top rather
than in a footnote. That was a defensible call for a phase whose code cannot
execute anywhere; it is not defensible for the phase whose entire subject is
whether this is ready to be called 1.0.

So: several independent adversarial reviewers over the cumulative
`m7-linux..main` diff, each finding verified by a reviewer that did not raise
it, weighted toward code over prose - the `#132` lesson, where a 28-round review saw
a growing share of its later findings become claims that *earlier corrections*
had introduced, with the code done around round nine.

Then `m8-hardening`, and `v1.0.0` held.

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

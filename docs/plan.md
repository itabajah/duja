# Duja - the plan

What happens next, and in what order. [STATUS.md](STATUS.md) says where the
build stands; this file says where it is going. Anything already done is
described in [history.md](history.md) rather than here, so this file stays short
enough that reading it is never a research task.

## What is left

1. **P8** - hardening to `v1.0.0`. Six waves, in [the table below](#p8-waves).

That is the whole list. Every phase before it is closed, and the two that closed
most recently each left a tag without a release: `m6-macos` (`v0.2.0` held) and
`m7-linux` (`v0.3.0` held). [STATUS.md](STATUS.md) has why.

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
and P7 turned out the same way. How each phase actually went, wave by wave, is in
[history.md](history.md) - including P7's waves table, which used to live here.

## P8 waves

| wave | scope | state |
|---|---|---|
| 1 | binary size: measure first, then trim, then gate it ([D-011](debt.md#d-011), [ADR-0012](adr/0012-binary-size-budget-variance.md)) | next |
| 2 | the fuzz and coverage lanes ([D-002](debt.md#d-002), [D-023](debt.md#d-023)) | pending |
| 3 | `--soak`, the harness two perf budgets already cite | pending |
| 4 | the debt drain (`refactor:` PR, the rubric's ~15% time-box) | pending |
| 5 | the security pass and the docs-truth sweep | pending |
| 6 | the phase gate - **the multi-reviewer one** - and `m8-hardening` | pending |

Waves 1 through 3 are independent of each other and of wave 4. Wave 5 wants
1-4 landed, because half of what it checks is whether the docs still describe
what those waves left behind. Wave 6 is last by definition.

### Wave 1 - the binary, and checking the ADR's reasoning before following it

[ADR-0012](adr/0012-binary-size-budget-variance.md) raised the budget to 16 MB
at P4, P5 blew through it, and the ledger has said "P8 must recover it" for two
releases. `duja.exe` is **19,446,784 bytes** today. That is the whole of the
debt, and the ADR already lists four levers in expected-payoff order.

**Do not start with the levers, and do not start with `cargo tree` either.** The
baseline attribution is already done, and it cost one wrong answer on the way,
which is the part worth writing down. `cargo tree -e normal -i resvg` reports
that `resvg` reaches the graph only through `i-slint-compiler` behind
`slint-macros` - a proc macro, so host code, so not one byte of `duja.exe`.
That answer is **false**, and `cargo bloat` says so: `usvg` is 470.9 KiB of
`.text` and `resvg` another 141.6. The reason is that **`cargo tree -i` dedupes
by default**: it printed the proc-macro path and collapsed the runtime one -
`i-slint-common` is also a normal dependency of `i-slint-core` - into a `(*)`.
Pass `--no-dedupe` and both appear. A dependency question answered from the tree
is a guess until a linker confirms it.

So ADR-0012's list of causes is right where it was doubted. What is genuinely
absent from the binary is the set nobody suspected: `ravif`/`rav1e` (an AV1
*encoder*, by some distance the largest thing in `Cargo.lock`), `exr`, `tiff`
and `qoi` - all of them reaching only the compiler, because `image-default-formats`
is already off.

**And one of the ADR's four levers does not exist.** Lever 2 is "Slint
image-format features: the flyout uses no SVG/EXR/animated images - investigate
disabling the decoder stack Slint pulls by default". Investigated:
`slint/std` implies `i-slint-core/std`, which implies `image-decoders` **and**
`svg`, with no seam between them. The formats that *were* optional are already
disabled. Removing the rest means patching Slint, which is not a hardening
change. The lever gets struck from the ADR rather than left there for the next
person to spend a day on.

That leaves three real levers and an addressable `.text` budget of roughly
1.1 MiB before LTO, which per-crate looks like this:

| lever | crates | `.text` |
|---|---|---|
| feature-gate the update check | `rustls`, `ring`, `ureq`, `webpki` | ~724 KiB |
| drop `env-filter` | `regex_syntax`, `regex_automata` | ~345 KiB |
| fat LTO | - | -1.0 MB measured at P4 |

`.text` is 11.3 MiB of the 18.5 MiB file, so each crate removed takes its
read-only data with it and the file delta should exceed the table. That is a
prediction, and the wave's job is to check it rather than assume it: **apply the
levers one at a time with a number beside each**, because a combined diff that
lands 3 MB teaches nothing about which lever to reach for at 1.1.

What the wave owes when it is done:

- ADR-0012 **corrected in place**: lever 2 struck with the reason, the
  `cargo bloat` attribution in the ledger, and the `cargo tree` dedupe trap
  recorded where the next person will hit it. The ADR is what they read first.
- **Per-lever deltas**, in bytes, so the ledger stops being a list of guesses.
- The unit ambiguity settled. The budget says "16 MB" and the ledger's rows say
  14.9 and 17.21 with no unit named. 16 MiB and 16 MB differ by 5%, which is
  larger than the smallest lever on the list, so a budget that does not say
  which one it means cannot be missed *or* met on purpose.
- **A CI size report that fails the job on a regression.** This is the part that
  matters after the wave ends. Size drifted from 14.9 to 19.4 across two
  releases with nothing to notice it, and a lever pulled once is a lever that
  can be given back by any dependency bump. A gate is the only thing that makes
  the trim durable.

### Wave 2 - the fuzz and coverage lanes

[D-002](debt.md#d-002) has been open since P2 and names two workflows;
[D-023](debt.md#d-023) names the sixth fuzz target and says to land it with
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

- **[D-108](debt.md#d-108) first**, because it is the only one that changes what
  a user sees: every clean quit writes identity gamma to every display, so
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
keeps paying for. One known instance to start from: `SECURITY.md` still
describes the release as "the Windows installer `.exe`, a portable `.zip`, and
(from `v0.2.0`) a macOS universal `.dmg`" and invites the reader to verify
provenance on "any of the three artifacts". P7 wave 6 made it four. Nobody
edited the security policy, because the tarball landed in `xtask` and the
release workflow and there was no reason to look there.

### Wave 6 - the gate

**The multi-reviewer gate, run properly this time.** P7's was one targeted pass,
and both [history.md](history.md) and its tag message say so at the top rather
than in a footnote. That was a defensible call for a phase whose code cannot
execute anywhere; it is not defensible for the phase whose entire subject is
whether this is ready to be called 1.0.

So: several independent adversarial reviewers over the cumulative
`m7-linux..main` diff, each finding verified by a reviewer that did not raise
it, weighted toward code over prose - the `#132` lesson, where a 28-round review
generated defects out of its own corrections after round nine.

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

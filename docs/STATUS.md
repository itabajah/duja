# Duja - Project Status

_Last updated: 2026-08-09. **P0 through P9 are closed**; `v0.1.6` shipped the
two ports that had been held. P9 was the first phase
whose scope was chosen by what the absent hardware still permits, rather than
merely limited by it, and its write-up is [history.md](history.md#s65). It touched
thirteen rows: **seven drained and six did not**, which is the outcome rather than
a shortfall. Of the six, three are wave 3 budgets whose instruments now exist and
whose checks still need hardware or a day of wall clock CI cannot give - an
instrument row drains when the budget it serves can be *checked*.
[D-110](debt.md#d-110) built nothing and had its deferral argument measured
instead; [D-076](debt.md#d-076) is open on a kernel behaviour no wait works
around; and [D-102](debt.md#d-102)'s original question - whether `build_tray`
works on a CI runner's window station - is still unmeasured. The last two rows the
phase worked, [D-059](debt-archive.md#d-059) and D-076, each ended on a mechanism
its own row had not proposed: D-059 drained on a witness type where the row asked
for a test, and D-076 narrowed rather than closing at all. The ports
ship as **unverified previews** rather than as confirmed platforms
([ADR-0024](adr/0024-preview-artifacts-on-the-patch-train.md)): the hold was
self-defeating, because the community confirmations macOS needs to leave
"experimental" cannot arrive for a build nobody can install. `v0.2.0` and
`v0.3.0` are re-mapped to mean **hardware-confirmed**, and `v1.0.0` still waits
on the same clause it always did._

Duja is an ultra-lightweight, cross-platform (Windows/macOS/Linux) system-tray
monitor brightness and display controller in Rust - a no-Electron Twinkle Tray
replacement.

This file is a **snapshot**, and it is meant to stay short enough to read in one
sitting. It was 1,911 lines until the 2026-08-07 checkpoint, because every wave
appended its own write-up to it; those are now in [history.md](history.md),
verbatim and unpruned, which is where they belong.

| you want | read |
|---|---|
| where the build stands | this file |
| what happens next, and in what order | [plan.md](plan.md) |
| why a thing is the way it is | [adr/](adr/) |
| what was tried, and what the review found | [history.md](history.md) |
| what is knowingly unfinished | [debt.md](debt.md) |
| what got finished, and how | [debt-archive.md](debt-archive.md) |
| how to cut a release | [release-checklist.md](release-checklist.md) |
| how to QA by hand | [qa-checklist.md](qa-checklist.md) |
| what Duja could grow into after 1.0 | [future-vision.md](future-vision.md) |

## At a glance

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
| P9 App-layer seam + instruments | `m9-seam` | done - 7 of the 13 rows it touched drained |

`m9-seam` is the one name in that column cut **on** this close-out rather than
before it. Worth a line because the other nine resolve and the table gives a
reader no way to tell which is which. What it cost to get those two sentences
right is in [history.md](history.md#s68), which is the file for that.

| release | train | state |
|---|---|---|
| `v0.1.0` | Windows | shipped - installer + portable zip, signed, auto-update loop |
| `v0.1.1` | Windows | shipped - 10 fix PRs from a 14-module deep review |
| `v0.1.2` | Windows | shipped - multi-monitor and capability fixes, 6 PRs |
| `v0.1.3` | Windows | shipped - the built-in panel no longer vanishes on a GPU-driven backlight |
| `v0.1.4` | Windows | shipped - dark rebrand plus the mirror/software-only pair |
| `v0.1.5` | Windows | shipped - a live monitor no longer sticks as "software-only"; tray Restart |
| `v0.1.6` | Windows + previews | shipped - the first release to carry a macOS `.dmg` and a Linux `.tar.gz`, both labelled unverified previews |
| `v0.2.0` | macOS | **re-mapped** - now means hardware-confirmed, not first shipped |
| `v0.3.0` | Linux | **re-mapped** - same |
| `v1.0.0` | - | **held** - see below |

Each release row is written up in [history.md](history.md), including what its
review found and which of its stated reasons turned out to be wrong.

**What `v0.1.6` changed, and what it did not.** `v0.2.0` and `v0.3.0` were held
for one reason: **nobody has run either build on the hardware it targets.** That
is still true. What changed is the recognition that holding was self-defeating -
[ADR-0013](adr/0013-macos-ddc-wrap-vs-vendor.md) keeps the macOS DDC path
experimental until at least three independent community confirmations per
architecture exist, and those come from other people's machines running an
artifact that was never on the Releases page. The condition for releasing was
confirmation; the mechanism for getting confirmation was releasing.

So [ADR-0024](adr/0024-preview-artifacts-on-the-patch-train.md) ships both on the
patch train as labelled previews, and re-maps `v0.2.0` / `v0.3.0` to mean
**hardware-confirmed** rather than first-shipped. Every release carrying an
unconfirmed platform says so in its own notes, from
[release-notes-preamble.md](release-notes-preamble.md) rather than from whoever
cut it - a label is weaker than a hold, so it gets a mechanism.

**`v1.0.0` is unchanged and still held.**
[ADR-0019](adr/0019-version-ladder-and-release-trains.md) defines it as "fuzz
burn-in, soak, size/perf budgets met, packaging, **cross-platform hardware
sign-off**", and a preview is the opposite of a sign-off. What is different is
that the sign-off is now *obtainable*: [qa-checklist.md](qa-checklist.md) says
what each run must cover, ordered so the never-executed paths come first, and
the artifact those runs need is downloadable at last.

## The build constraint that has not gone away

P8 ran straight into it more than once, and so did this checkpoint: **`duja-app` cannot be built for Linux on the Windows dev box**
(`yeslogic-fontconfig-sys` wants a pkg-config sysroot; `RUST_FONTCONFIG_DLOPEN=1`
gets past it and then `fontique` fails on the dlopen module layout, confirmed
twice), so any size number for a non-Windows target is a CI-only measurement.

**But an isolated crate can be cross-checked, and that is the technique to
reach for first.** Only the pinned toolchain has the target installed, so:

```
cargo +1.96.1 check  --target x86_64-unknown-linux-gnu --all-targets
cargo +1.96.1 clippy --target x86_64-unknown-linux-gnu --all-targets
RUSTDOCFLAGS="-D warnings" cargo +1.96.1 doc --target x86_64-unknown-linux-gnu   --no-deps --document-private-items
```

A throwaway crate that pulls one app module in through `#[path]`, with the
workspace's `[lints]` copied into its manifest, compiles that module for Linux in
seconds. P7 wave 5 validated the entire ksni API surface and later all of
`bin_support/gamma.rs` this way before spending a CI round, and it caught real
errors both times - including a local named `display` that shadows `tracing`'s
own `display` helper inside its macros.

## Health

Measured on this box, 2026-08-08 unless a bullet dates itself otherwise:

- **1,489 tests** pass in a local `cargo test --workspace --all-features`
  (measured 2026-08-09; `9dc0586`, the last commit before P9's final two PRs,
  was 1,488 on the same lane),
  with a further **9 `#[ignore]`d** on top of that rather than among them - an
  ignored test does not pass, and writing it as "8 of them" was wrong for one
  edit's lifetime. **The figure this replaces was 1,459 and 8, and it went stale
  the way this bullet already predicted it would**: it was re-measured at the P9
  checkpoint on 2026-08-08, wave 3's four PRs then merged 29 passing tests and
  one `#[ignore]`d, and none of them re-counted. So the advice below is now
  evidenced twice rather than once. The P8 gate found the figure before it 36 low
  and this bullet then said it would be "re-counted with every release rather
  than carried forward" - which did not survive contact: `v0.1.6`'s 1,413 was
  carried through six merged PRs and was 46 low by the time a review caught it.
  Re-count it at every checkpoint, not every release - and a wave is a
  checkpoint.
  The per-OS count differs, and deliberately is not enumerated here: the
  `#![cfg(windows)]` and `#![cfg(unix)]` integration suites compile out on the
  other lanes, as do per-OS unit tests spread across roughly two dozen modules.
  A closed list of three was wrong within a day of being written.
- Green on **3 OSes**; clippy `-D warnings` clean; `cargo-deny` clean
  (advisories, bans, licenses, sources); **6 fuzz targets** building on stable,
  burned weekly by `fuzz.yml` and compile-checked on every PR.
- Adversarial review of **every PR** at the `v0.1.6` checkpoint as well, and
  every one found major defects: two false capability claims in the file
  published verbatim to users; a test whose *name* asserted the one thing its
  experiment did not measure; and, in the docs sweep itself, two false
  superlatives about coverage plus a link the reorganisation broke. Adversarial gate reviews at
  **P2, P3, P4, P5, P6 and P8**. **P7's was
  narrower** - one targeted pass rather than several independent reviewers - and
  [history.md](history.md) says so at the top of its write-up rather than in a
  footnote. **P8's was the widest yet**: three independent gate reviewers over
  the cumulative diff *plus* a per-PR adversarial review of all six waves, nine
  in total, and every one found something that would otherwise have shipped.
  Plus a full
  post-`v0.1.0` deep review (14 module reviewers, every non-low finding
  adversarially verified) with every confirmed finding fixed test-first.
- Measured at the P4/P5 gates, headless: idle RSS **23.3 MB** (budget 35), idle
  CPU **0 ms over 20 s** - zero wakeups, by construction.
- **`duja --soak <secs>` is the instrument those RSS budgets have cited since
  P4** and did not have until P8 wave 3. Re-run on the **release** build at the
  `v0.1.6` checkpoint, 90 seconds sampling every 10: peak RSS **16,936,960
  bytes** against a 35,000,000 budget, **0 bytes growth**, flat GDI and USER,
  10 samples and none unreadable. Verdict `PASS`, exit 0.
  Read it for what it is - the **headless** process, not the tray one, and the
  *whole* resident set rather than the "private" the row asks for. Two further
  limits this run made visible rather than assumed: the IPC server did **not**
  start, because another Duja held the endpoint, and the report says so instead
  of quietly measuring less; and a 20-second run over the same build returned
  `UNMEASURABLE` with **exit 1**, which is the guarantee that a run measuring
  nothing cannot be read as a pass. The 24-hour run the budget names is still
  undone ([D-111](debt.md#d-111)) - 90 seconds is the longest yet, against the
  30 that row records - and when it happens its measured handle drift should
  replace the harness's tolerance constant, which is a reasoned guess and says
  so.
- **[D-102](debt.md#d-102)'s fixture has landed**, and P9's first six PRs with
  it. Two of those six built it (`#157`, `#159`); `#158` borrows it; `#155` is
  the plan, and `#156` and `#160` are unrelated to it.

  `AppState` is constructible in a test on every lane, behind a recording
  fake tray, a recording gamma channel and the headless Slint backend.
  `tray/state.rs`'s **uncovered regions went 1,031 to 823** across three of
  those PRs, and [history.md](history.md#s70) gives each one's delta - a total
  credited to any single PR is wrong. That table was in `plan.md` until P9's
  close-out moved the section, and this pointer went with it.

  The percentage moved 11.27 % to 48.91 % and is the
  flattered number: the file grew 1,162 regions to 1,611, since a fixture's own
  body counts as covered. `tray/update_flow.rs` moved 26.89 % to 73.08 % on the
  fixture and 73.08 % to 79.72 % on the config-cap PR after it - none of it on
  the gamma seam, which is worth saying inside a bullet a reader would otherwise
  credit for all of it. [D-040](debt-archive.md#d-040) drained on the fixture,
  and [D-016](debt-archive.md#d-016) plus [D-065](debt-archive.md#d-065) on the
  gamma seam that followed it, each proven red at its historical site.
  [D-059](debt-archive.md#d-059) needed neither and drained on neither: it
  wanted to observe *when* `build_tray` ran relative to the loop, and `#167`
  closed it with a witness type that makes the pre-loop call a compile error. Of
  the four rows that shared one deferral sentence, exactly **one** drained on the
  refactor that sentence described. What the experiment
  behind all this settled is still a limit rather than an answer:
  `build_tray` succeeding in a test process was measured on an interactive
  Windows session only, and a CI runner's window station remains unmeasured -
  much less pressing now that no test builds a real tray.
- `duja.exe` is **15,729,664 bytes** (15.00 MiB) release, **within**
  its 16 MiB budget with 1,047,552 bytes to spare; `dujactl.exe` is 644,608
  (2 MiB budget, down from 851,968). Re-measured on the `v0.1.6` tree rather
  than carried over from P8 wave 1, which is why it is 19,968 bytes above the
  figure that ledger records - the checkpoint added code, and the budget
  absorbed it. P8 wave 1 took 3,737,088 bytes off
  the tray binary, 19.2 %, and the budget is now enforced by
  `cargo xtask size` in the release workflow rather than remembered - **on the
  Windows job only**, which is what that workflow builds and measures.
  The measured ledger, and the one lever that is a trade rather than a free win,
  are in [ADR-0012](adr/0012-binary-size-budget-variance.md).
- **Three perf budgets are still not measured by anything.** "Overlay alpha
  update < 16 ms", "Cold start < 300 ms" and "Slider to DDC write dispatched"
  were last measured by hand at P4, and P8 wave 1 changed the optimization
  level, which plausibly moves at least two of them. The interim instrument is
  two rows at the top of [qa-checklist.md](qa-checklist.md).
  [D-109](debt.md#d-109) built a render benchmark in P9 wave 3 and it covers
  **none of these**: it renders the flyout, the overlay is a Win32 layered
  window, and a cold start needs a session. What it did settle is the
  `opt-level = "s"` exemption, which is worth roughly 1.4x on a frame and which
  nothing had measured before.

The CI commands, which a local check must match exactly:

```
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps --all-features --document-private-items
```

## Known gaps carried forward

- **The WMI panel set-path has never executed on real hardware** (this box is a
  desktop): borrow a laptop for a 30-minute run before the beta.
- **Laptop QA of the `v0.1.4` mirror/software-only behaviour** is outstanding,
  as is regenerating and uploading `social-preview.png`.
- **Suspend/resume does not re-push DDC levels** when the display set is
  unchanged; `classify_failure`'s `GetLastError` assumption needs a live unplug.
- **No macOS hardware has ever run any of this.** Every macOS backend is
  verified by types, pure tests and cross-referenced primary sources only.
- **No Linux desktop has ever run the tray either**, and the CI lane cannot: a
  runner has no `StatusNotifierWatcher`, no X server and no compositor, so the
  tray never registers, a menu item never fires and `set_gamma` always refuses.
  Only the refusal paths are exercised ([D-105](debt.md#d-105)).
- **Global hotkeys do not exist on Linux.** `global-hotkey`'s backend there is
  X11-only, so Duja registers nothing and says so rather than half-working; the
  three hotkey settings parse, validate and then do nothing
  ([D-103](debt.md#d-103)).
- Quirk user-override file, sync-group UI, in-UI hotkey editing - all in
  [debt.md](debt.md).

## Notes and gotchas for whoever continues

- **Environment**: Rust pinned 1.96.1 (MSRV 1.94), MSVC, edition 2024.
  Smart App Control must stay **off** on the dev box (os error 4551 otherwise).
  Fuzzing on Windows needs the MSVC ASan DLL on `PATH` (see
  [fuzz/README.md](../fuzz/README.md)).
- **Session trap**: a disconnected session sees no displays - `duja-ddc`
  correctly returns nothing and `dujactl doctor` says so. Check `qwinsta`
  before blaming the code.
- **Cross-platform rustdoc trap**: rustdoc only resolves links in code the
  target actually compiles, so an intra-doc link from cross-platform code to a
  `#[cfg]`-gated item breaks on every lane that cannot see it. Use plain
  backticks there. (Broke PRs #8, #10, #17 on the Linux lane.) Since #85 the
  hazard is symmetric - `cargo doc` runs on all three OSes - so a link into
  `cfg(windows)` code now also breaks the macOS lane, and vice versa.
- **rustdoc silently skips private items**, and it strips them *before*
  resolving intra-doc links, so a plain `cargo doc` compiles a private module
  and then checks almost nothing in it. Every macOS backend is a private
  `mod mac;`, which is why 15 broken links sat undetected until #85 turned on
  `--document-private-items`. Note the asymmetry that hid this: Cargo passes
  that flag automatically for **binary** targets, so `duja-app`'s tray was
  covered by accident while the library crates were not. Both CI doc
  invocations now pass it explicitly and must stay in sync.
- **Lane-gating trap**: before writing where a test runs, open the module gate
  above it and read which targets it admits. A doc comment claiming a test runs
  "on every lane" is false the moment the module is `cfg`-gated, and it is
  exactly the claim a reader trusts when deciding whether a seam is guarded.
- **Outer doc comments on a `mod` declaration resolve in the parent's scope.**
  Rustdoc concatenates a `///` at the declaration with the module file's own
  `//!` header and resolves the whole thing where the *declaration* sits, so a
  `[`super::thing`]` written in the module file starts looking one level too
  high. It fails only on the lane that compiles that module. Use `//` on the
  declaration when the module file carries its own links (`tray.rs`, `#136`).
- **A gate can be widened everywhere except the one place that matters.**
  `#136` un-gated the tray for Linux, and every piece of it compiled and tested
  green on the ubuntu lane for two rounds while `main.rs` still refused to
  launch it. Nothing failed: not `cargo test`, not rustdoc. The only signal was
  clippy's dead-code pass, and only because the module-wide `allow(dead_code)`s
  had been removed in the same PR. When un-gating, grep for the *entry point*
  first and remove blanket allows in the same change, or the lane goes green
  over a feature that is not reachable.
- **`cargo tree --target` prints TWO root trees, and the proc-macro one comes
  first.** Asking which crates the linker actually sees looks like a job for
  `cargo tree -p duja-app -e normal --target <triple> -i <crate>`. For `resvg`
  the output opens with a path through `slint-macros`, a proc macro - i.e.
  "host code, not in the binary", which is false; `cargo bloat` puts `usvg` +
  `resvg` at 612 KiB of `.text`. Resolver 2 keeps the host and target feature
  universes apart, and `--target` makes cargo emit **a separate root tree for
  each**, back to back, separated by one blank line. The runtime answer is the
  second tree.

  The trap is a `head`, not a `(*)`. `--no-dedupe` does **not** help - nothing
  was deduplicated. Either count the roots (`| grep -c '^<crate> v'`) before
  reading the branches, or drop `--target`, which prints the runtime path first.
  Either way, treat the tree as a hypothesis and confirm with `cargo bloat` or a
  measured A/B build: only a linker knows what is in the binary.
- **Elevated-token trap**: an elevated process's default object owner is the
  Administrators group, not the user - the pipe's SDDL therefore sets the owner
  explicitly (`O:<sid>`), or the DACL owner assertion fails under CI.
- **Test-process hygiene**: if an `ipc_pipe-*.exe` lingers after a run, that is
  a hang, not noise - investigate it.
- **Encoding trap**: a bulk text move through a tool that guesses an encoding
  produces mojibake that is *valid UTF-8*, so no gate in this repo detects it.
  140 such characters sat in `docs/future-vision.md` until `#133`. Read and
  write UTF-8 explicitly in any script that moves prose between files.
- **Workflow**: trunk-based, squash-merge PRs only, conventional commits
  (lowercase subjects, 72 chars). The commit-lint job is advisory; PR titles are
  what land on `main`. `style` is **not** an allowed commit type.
- **Lesson worth keeping**: three separate defects (the peek-poll deadlock, the
  dribble slowloris, the P4 throttle) were invisible to per-crate test suites
  and to green agent reports. The phase-gate adversarial review - plus insisting
  every regression test be proven **red** before its fix - is what caught them.
- **Second lesson, from `#132`.** A review round that keeps finding things is
  not necessarily converging. That PR took 28 rounds and roughly 194 findings;
  the code was done around round 9, and a growing share of the later findings
  were claims that *earlier corrections* had introduced. Heavy prose plus a
  correction-commit habit is a defect generator. Two rules came out of it and
  both worked immediately: brief reviewers to weight code over prose, and
  **never write a tally in a comment** - keep the counterexample that stops the
  bug being re-introduced, drop the count that the next edit falsifies.

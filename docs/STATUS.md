# Duja - Project Status

_Last updated: 2026-08-08. **P8 (hardening) is in progress** - six waves to
`m8-hardening`, laid out in [plan.md](plan.md). Every phase before it is closed.
**Two releases are held on the same terms** - `v0.2.0` (macOS) and `v0.3.0`
(Linux) - each waiting for one person to run it on the hardware it targets, and
`v1.0.0` will make three: ADR-0019 puts cross-platform hardware sign-off in its
own definition, so P8 can meet every other clause and not that one._

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
| P6 macOS port | `m6-macos` | done, gate passed, release held |
| P7 Linux port | `m7-linux` | done, gate run, release held |
| **P8 Hardening** | `m8-hardening` / `v1.0.0` | **in progress** |

| release | train | state |
|---|---|---|
| `v0.1.0` | Windows | shipped - installer + portable zip, signed, auto-update loop |
| `v0.1.1` | Windows | shipped - 10 fix PRs from a 14-module deep review |
| `v0.1.2` | Windows | shipped - multi-monitor and capability fixes, 6 PRs |
| `v0.1.3` | Windows | shipped - the built-in panel no longer vanishes on a GPU-driven backlight |
| `v0.1.4` | Windows | shipped - dark rebrand plus the mirror/software-only pair |
| `v0.1.5` | Windows | shipped - a live monitor no longer sticks as "software-only"; tray Restart |
| `v0.2.0` | macOS | **held** - see below |
| `v0.3.0` | Linux | **held** - see below |
| `v1.0.0` | - | pending P8, and **will be held** - see below |

Each release row is written up in [history.md](history.md), including what its
review found and which of its stated reasons turned out to be wrong.

**Why `v0.2.0` and `v0.3.0` are held, and why `v1.0.0` will be.** Both closed
phases withheld their release for the same reason: **nobody has run either build
on the hardware it targets.** A release is a separate decision from a tag, and
this is that decision rather than a blocker - nothing in the codebase stops
either.

`v1.0.0` inherits it. [ADR-0019](adr/0019-version-ladder-and-release-trains.md)
defines that release as "fuzz burn-in, soak, size/perf budgets met, packaging,
**cross-platform hardware sign-off**", and the last clause is not something a
repository can satisfy. P8 is planned around that rather than into it: every
other clause met and measured, so the hold is the only thing left.

For Linux the artifact exists and the pipeline is proven: a `workflow_dispatch`
dry run built, staged, tarred, extracted and verified
`duja-<ver>-linux-x64.tar.gz` alongside the other three, in one `SHA256SUMS`. What
has never happened is a human extracting it and clicking the tray.
[qa-checklist.md](qa-checklist.md)'s Linux section opens with the block that run
has to cover, and it is ordered so the paths that have never executed come first.
Separately, [ADR-0013](adr/0013-macos-ddc-wrap-vs-vendor.md) keeps the macOS DDC
path labelled experimental until there are at least three independent community
confirmations per architecture, which no amount of code closes.

## Where P8 stands

| wave | scope | state |
|---|---|---|
| 1 | binary size: measure, trim, then gate it in CI | next |
| 2 | the fuzz and coverage lanes | done - `#145` |
| 3 | `--soak`, the harness two perf budgets already cite | done - `#146` |
| 4 | the debt drain (`refactor:` PR) | **partial** - `#147` |
| 5 | the security pass and the docs-truth sweep | done - `#148` |
| 6 | the multi-reviewer phase gate, and `m8-hardening` | pending |

[plan.md](plan.md) has what each wave owes and why it is ordered there. P7's
wave table used to live in this section; it is in [history.md](history.md) now.
It is the only wave table there - P0 through P6 were never written up that way,
and this file has said so by omission rather than by claiming otherwise.

**The constraint that shaped P7 has not gone away**, and wave 1 runs straight
into it: **`duja-app` cannot be built for Linux on the Windows dev box**
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

Measured on this box, 2026-08-08:

- **1,370 tests** pass in a local `cargo test --workspace --all-features`
  (1,354 without `--all-features`).
  The per-OS count differs, and deliberately is not enumerated here: the
  `#![cfg(windows)]` and `#![cfg(unix)]` integration suites compile out on the
  other lanes, as do per-OS unit tests spread across roughly two dozen modules.
  A closed list of three was wrong within a day of being written.
- Green on **3 OSes**; clippy `-D warnings` clean; `cargo-deny` clean
  (advisories, bans, licenses, sources); **6 fuzz targets** building on stable,
  burned weekly by `fuzz.yml` and compile-checked on every PR.
- Adversarial gate reviews at **P2, P3, P4, P5 and P6**. **P7's was narrower**
  - one targeted pass rather than several independent reviewers with separate
  verification - and [history.md](history.md) says so at the top of its write-up
  rather than in a footnote. Plus a full
  post-`v0.1.0` deep review (14 module reviewers, every non-low finding
  adversarially verified) with every confirmed finding fixed test-first.
- Measured at the P4/P5 gates, headless: idle RSS **23.3 MB** (budget 35), idle
  CPU **0 ms over 20 s** - zero wakeups, by construction.
- **`duja --soak <secs>` is the instrument those RSS budgets have cited since
  P4** and did not have until P8 wave 3. A 30-second run on this box: peak RSS
  **18,169,856 bytes** against a 35,000,000 budget, zero growth, flat GDI and
  USER. Read it for what it is - the **headless** process, not the tray one, and
  the *whole* resident set rather than the "private" the row asks for. The
  24-hour run the budget names has not been done ([D-111](debt.md#d-111)), and
  when it is, its measured handle drift should replace the harness's tolerance
  constant, which is a reasoned guess and says so.
- `duja.exe` is **15,709,696 bytes** (14.98 MiB) release, **within**
  its 16 MiB budget with 1,067,520 bytes to spare; `dujactl.exe` is 643,584
  (2 MiB budget, down from 851,968). P8 wave 1 took 3,737,088 bytes off
  the tray binary, 19.2 %, and the budget is now enforced by
  `cargo xtask size` in the release workflow rather than remembered - **on the
  Windows job only**, which is what that workflow builds and measures.
  The measured ledger, and the one lever that is a trade rather than a free win,
  are in [ADR-0012](adr/0012-binary-size-budget-variance.md).
- **Two perf budgets are not measured by anything.** "Overlay alpha update
  < 16 ms" and "Cold start < 300 ms" were last measured by hand at P4, and wave
  1 changed the optimization level, which plausibly moves both. There is no
  automated render benchmark ([D-109](debt.md#d-109)); the interim instrument is
  two rows at the top of [qa-checklist.md](qa-checklist.md).

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

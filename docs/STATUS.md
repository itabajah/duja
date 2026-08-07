# Duja - Project Status

_Last updated: 2026-08-07. **P7 (Linux) is in progress**: waves 0 through 4b-5
are merged and wave 5 is one increment in. Every phase before P7 is closed.
`v0.2.0` is tagged as `m6-macos` and **deliberately unreleased** until someone
has launched `Duja.app` on a real Mac._

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
| **P7 Linux port** | `m7-linux` / `v0.3.0` | **in progress** |
| P8 Hardening | `m8-hardening` / `v1.0.0` | pending |

| release | train | state |
|---|---|---|
| `v0.1.0` | Windows | shipped - installer + portable zip, signed, auto-update loop |
| `v0.1.1` | Windows | shipped - 10 fix PRs from a 14-module deep review |
| `v0.1.2` | Windows | shipped - multi-monitor and capability fixes, 6 PRs |
| `v0.1.3` | Windows | shipped - the built-in panel no longer vanishes on a GPU-driven backlight |
| `v0.1.4` | Windows | shipped - dark rebrand plus the mirror/software-only pair |
| `v0.1.5` | Windows | shipped - a live monitor no longer sticks as "software-only"; tray Restart |
| `v0.2.0` | macOS | **held** - see below |
| `v0.3.0` | Linux | pending P7 |
| `v1.0.0` | - | pending P8 |

Each release row is written up in [history.md](history.md), including what its
review found and which of its stated reasons turned out to be wrong.

**Why `v0.2.0` is held.** P6 passed its gate and the phase is closed; the
release is a separate decision, and it is being withheld until `Duja.app` has
been launched on real Apple hardware. Nothing in the codebase blocks it.
Separately, [ADR-0013](adr/0013-macos-ddc-wrap-vs-vendor.md) keeps the macOS DDC
path labelled experimental until there are at least three independent community
confirmations per architecture, which no amount of code closes.

## Where P7 stands

| wave | scope | state |
|---|---|---|
| 0 | unix IPC + lock-directory hardening | done |
| 1 | the reserved Linux ADRs (0010, 0011, 0022) | done |
| 2 | DRM/sysfs enumeration, `/dev/i2c`, backlight | done |
| 3 | event pump, autostart, desktop, geometry | done |
| 4 | software dimming on X11 and Wayland, plus the capability probe | done |
| 4b-5 | the X11 cursor anchor | done |
| **5** | **the Linux tray (ksni)** | **seam landed; the backend is next** |
| 6 | `xtask dist --target linux`, the release job, the docs | pending |
| 7 | phase gate, adversarial review, tag `m7-linux` | pending |

[plan.md](plan.md) has what each remaining wave owes, and the one constraint
that shapes wave 5: **`duja-app` cannot be built for Linux on the Windows dev
box** (`yeslogic-fontconfig-sys` needs a cross-compile sysroot), so un-gating the
tray is a CI-only loop.

## Health

Measured on this box, 2026-08-07:

- **1,339 tests** pass in a local `cargo test --workspace --all-features`.
  The per-OS count differs, and deliberately is not enumerated here: the
  `#![cfg(windows)]` and `#![cfg(unix)]` integration suites compile out on the
  other lanes, as do per-OS unit tests spread across roughly two dozen modules.
  A closed list of three was wrong within a day of being written.
- Green on **3 OSes**; clippy `-D warnings` clean; `cargo-deny` clean
  (advisories, bans, licenses, sources); **5 fuzz targets** building on stable.
- Adversarial gate reviews at **P2, P3, P4, P5 and P6**, plus a full
  post-`v0.1.0` deep review (14 module reviewers, every non-low finding
  adversarially verified) with every confirmed finding fixed test-first.
- Measured at the P4/P5 gates, headless: idle RSS **23.3 MB** (budget 35), idle
  CPU **0 ms over 20 s** - zero wakeups, by construction.
- `duja.exe` is **~19 MB** release (thin LTO), over the 16 MB budget;
  `dujactl.exe` ~0.8 MB. Tracked in
  [ADR-0012](adr/0012-binary-size-budget-variance.md) and owned by P8.

The CI commands, which a local check must match exactly:

```
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps --all-features --document-private-items
```

## Known gaps carried forward

- **Binary ~19 MB against a 16 MB budget** - P8 recovers it.
- **The WMI panel set-path has never executed on real hardware** (this box is a
  desktop): borrow a laptop for a 30-minute run before the beta.
- **Laptop QA of the `v0.1.4` mirror/software-only behaviour** is outstanding,
  as is regenerating and uploading `social-preview.png`.
- **Suspend/resume does not re-push DDC levels** when the display set is
  unchanged; `classify_failure`'s `GetLastError` assumption needs a live unplug.
- **No macOS hardware has ever run any of this.** Every macOS backend is
  verified by types, pure tests and cross-referenced primary sources only.
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

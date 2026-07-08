# Duja — Project Status

_Last updated: 2026-07-08 (end of P2 core domain)._

Duja is an ultra-lightweight, cross-platform (Windows/macOS/Linux) system-tray
monitor brightness & display controller in Rust — a no-Electron Twinkle Tray
replacement. This file is the human-readable snapshot of where the build stands.
The authoritative plan is the phase roadmap; architecture decisions live in
[docs/adr/](adr/).

## At a glance

| Phase | Milestone | State |
|---|---|---|
| P0 Foundation | `m0-foundation` | ✅ done |
| P1 Spikes (risk burn-down) | `m1-spikes` | ✅ done |
| P2 Core domain (`duja-core`) | `m2-core` | ✅ done (pending tag) |
| P3 Windows hardware slice | `m3-win-hw` | ⏭️ next |
| P4 Windows dimmer + UI (MVP) | `m4-win-mvp` / `v0.1.0-alpha` | pending |
| P5 Power features | `m5-win-full` / `v0.2.0-beta` | pending |
| P6 macOS port | `m6-macos` / `v0.3.0-beta` | pending |
| P7 Linux port | `m7-linux` / `v0.4.0-beta` | pending |
| P8 Hardening → 1.0 | `m8-hardening` / `v1.0.0` | pending |

Health: **193 tests + doctests green on 3 OSes**, ~96% line coverage on
`duja-core`, 3×1,000,000 fuzz executions clean, clippy `-D warnings` clean,
`cargo-deny` clean.

## What is done

### P0 — Foundation
- Cargo workspace: 9 crates (`duja-core`, `-ddc`, `-panel`, `-dimmer`,
  `-platform`, `-ipc`, `-ui`, `-app`, `dujactl`) + `xtask`.
- Lint wall: `deny(unwrap/expect/panic/todo/unimplemented/undocumented_unsafe)`,
  clippy pedantic as warnings, `forbid(unsafe_code)` in the pure/UI/ipc crates.
- CI on 3 OSes: fmt, clippy `-D warnings`, tests (nextest), `cargo-deny`, MSRV
  (1.94), rustdoc `-D warnings`, conventional-commit lint. Actions SHA-pinned.
- Governance: dual MIT/Apache-2.0, `SECURITY.md` threat model, contributing
  guide, issue templates incl. **monitor-quirk-report** (seeds the quirk DB),
  branch protection (strict checks + linear history), squash-only auto-merge.

### P1 — Spikes (all verified on real hardware; evidence on `spike/*` branches)
- **UI framework**: Slint + `tray-icon` + `global-hotkey` cohabit on one Windows
  main thread with **zero idle wakeups**, no polling timer (ADR-0001).
- **Renderer**: Slint **software renderer** is the default — 9.76 MB binary,
  ~17 MB idle RSS; FemtoVG fails the RAM budget, Skia the binary budget
  (ADR-0009).
- **DDC**: Duja writes its **own** Windows DDC backend on `windows`-crate dxva2
  (~300–450 LOC). `ddc-hi` duplicated a single monitor, exposes no EDID on
  Windows, is `!Send`, and is passively maintained; the retry/pacing/verify
  layer is ours to build regardless — 60–70% of unpaced reads fail at the
  monitor level (ADR-0002).
- **Overlay**: click-through dimming recipe proven with a negative control;
  `WDA_EXCLUDEFROMCAPTURE` keeps screenshots undimmed; product contract "dims
  content, not system UI" documented (ADR-0003).

### P2 — Core domain (`duja-core`, pure, no OS APIs, no `unsafe`)
Built strictly test-first across three waves, then hardened by an adversarial
review at the phase gate. Modules:
- `id` — total, never-panicking EDID parser → `StableDisplayId` (serial string
  › numeric serial › content hash; slot disambiguation for twins).
- `model` — `Feature` (VCP 0x10/0x12/0x60), `Capabilities`, `DimMode`,
  `DisplayKind`, `DisplaySnapshot`.
- `controller` — the frozen `BrightnessController` trait + `ControlError`.
- `continuum` — one slider → hardware level + overlay alpha (+ optional gamma);
  `hardware_floor: Option<u8>`; continuity at the floor proven by property tests.
- `debounce` — pure trailing-edge `Debouncer` + per-key `Coalescer` (clock
  injected → deterministic).
- `manager` — hot-plug enumeration diffing keyed by stable id: replug restores
  levels, twins get deterministic slots, unresponsive marking for the watchdog.
- `sync` — drift-free sync groups with per-member offsets.
- `config` — schema v1, format-preserving `toml_edit` round-trips (unknown keys
  + comments survive), chained migrations, crash-safe atomic writes, separate
  volatile state file (ADR-0007).
- `caps` — total, iterative MCCS capability-string parser (size/depth capped).
- `quirks` — prefix/glob matcher (exact beats glob, no regex); embedded DB
  seeded with the real MSI MP273QP findings from the P1 spike.
- `testing` (feature `test-support`) — `FakeClock`, scriptable `FakeController`,
  and the reusable `BrightnessController` **contract suite** every backend
  inherits.

**P2 gate outcomes** (all fixed before tagging):
- Contract suite now rejects a `max`-lying backend and tolerates the real
  input-source metadata lie (ADR-0002) — the gap the review found.
- Sync offsets are now persisted (`MonitorConfig.sync_offset`).
- Unstamped config files migrate from v0 (shape-tolerant rename, no clobber)
  per ADR-0007, instead of being silently treated as current.

## What remains

- **P3 (next)** — the real Windows hardware slice: own dxva2 DDC backend +
  EDID-from-registry identity + WMI internal-panel control + `WM_DISPLAYCHANGE`/
  power/session platform events, all behind the `BrightnessController` contract.
  Includes per-monitor worker threads, write coalescing, and the stuck-driver
  watchdog (ADR-0005). Hardware-verifiable on the dev PC + MSI MP273QP.
- **P4** — `duja-dimmer` overlay (spike → production) + Slint flyout & tray →
  daily-driver MVP (`v0.1.0-alpha`).
- **P5** — global hotkeys, DDC input switching (VCP 0x60), `dujactl` + hardened
  IPC, settings UI, opt-in update check (`v0.2.0-beta`, public).
- **P6/P7** — macOS (IOAVService/DisplayServices, CI-verified) and Linux
  (i2c/logind/overlay, VM-assisted) ports.
- **P8** — fuzz burn-in, soak, packaging (winget/brew/AUR), 1.0.

## Notes & gotchas for whoever continues

- **Environment**: Rust pinned to 1.96.1 (MSRV 1.94), MSVC toolchain, edition
  2024. VS Build Tools + `cargo-nextest`/`-deny`/`git-cliff`/`-fuzz`/`-llvm-cov`
  installed. Smart App Control had to be turned **off** to run locally built
  exes (os error 4551) — do not re-enable on the dev box.
- **Fuzzing on Windows** needs the MSVC ASan DLL on `PATH`; see
  [fuzz/README.md](../fuzz/README.md).
- **CI debt**: coverage and fuzz gates run locally today; wiring them as CI jobs
  is a P8 item (see [debt.md](debt.md)).
- **Workflow**: trunk-based, squash-merge PRs only, conventional commits
  (lowercase subjects), milestone tags at phase exits. Spike code lives on
  `spike/*` branches (findings merged as ADRs, code not).

# Duja — Project Status

_Last updated: 2026-07-10 (end of P3 Windows hardware slice)._

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
| P2 Core domain (`duja-core`) | `m2-core` | ✅ done |
| P3 Windows hardware slice | `m3-win-hw` | ✅ done (hardware sign-off pending — see below) |
| P4 Windows dimmer + UI (MVP) | `m4-win-mvp` / `v0.1.0-alpha` | ⏭️ next |
| P5 Power features | `m5-win-full` / `v0.2.0-beta` | pending |
| P6 macOS port | `m6-macos` / `v0.3.0-beta` | pending |
| P7 Linux port | `m7-linux` / `v0.4.0-beta` | pending |
| P8 Hardening → 1.0 | `m8-hardening` / `v1.0.0` | pending |

Health: **326 tests + doctests green on 3 OSes**, clippy `-D warnings` clean,
`cargo-deny` clean, 4 fuzz targets (caps, EDID, quirks, IPC frame) building on
stable, adversarial gate reviews at P2 and P3 with all findings fixed
test-first before tagging.

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
- **DDC**: Duja writes its **own** Windows DDC backend on `windows`-crate dxva2.
  `ddc-hi` duplicated a single monitor, exposes no EDID on Windows, is `!Send`,
  and is passively maintained (ADR-0002).
- **Overlay**: click-through dimming recipe proven with a negative control;
  `WDA_EXCLUDEFROMCAPTURE` keeps screenshots undimmed (ADR-0003).

### P2 — Core domain (`duja-core`, pure, no OS APIs, no `unsafe`)
The whole application brain, built strictly test-first: EDID → `StableDisplayId`
identity, `Feature`/`Capabilities` model, the frozen `BrightnessController`
trait, the brightness continuum (hardware + overlay handoff), pure
debounce/coalesce state machines, the hot-plug `DisplayManager` (twin slotting,
replug level restore, unresponsive marking), drift-free sync groups,
format-preserving config with chained migrations and atomic writes, the total
MCCS caps parser, the quirk DB (seeded with real MSI MP273QP findings), and the
reusable cross-backend contract suite. ~96 % line coverage; 3×1 M fuzz
executions clean at the gate.

### P3 — Windows hardware slice (PRs #7–#12)
- **`duja-ddc`** — in-house dxva2 backend (ADR-0002): `VcpTransport`
  abstraction + `DdcController` owning all policy (quirk-driven pacing,
  ≥3-retry with backoff, verify-by-readback, `max_brightness` override, caps
  fallback probing, `ddc_broken`); Windows enumeration bridges
  HMONITOR → CCD (`QueryDisplayConfig`) → SetupAPI registry EDID, defeating the
  NVIDIA `Default_Monitor` stub hazard live on this machine. Identity verified
  against the real MSI MP273QP EDID (256 bytes → `MSI-30B6-…`). All FFI in
  `win/sys.rs` with per-block `// SAFETY:`; the controller passes the core
  contract suite via a fake transport on all 3 OSes.
- **`duja-platform`** — Windows event pump: hidden **top-level** window
  (message-only HWNDs never get `WM_DISPLAYCHANGE`),
  `RegisterDeviceNotification(GUID_DEVINTERFACE_MONITOR)`, power
  suspend/resume, session unlock → normalized `PlatformEvent`s; live
  posted-message tests.
- **`duja-panel`** — WMI internal-panel backend (raw COM, no `wmi`-crate
  weight); contract-tested via fake transport; graceful `Ok(vec![])` on
  panel-less desktops (verified live).
- **`duja-ipc`** — versioned envelope, strict `Request` / forward-compatible
  `Response`, 64 KiB length-prefixed framing enforced before allocation, new
  `fuzz_ipc_frame` target. Transport lands in P5.
- **`duja-app` engine** — controller actor + per-monitor workers
  (ADR-0005: std threads + crossbeam, zero idle wakeups), latest-wins
  per-feature coalescing, 5 s stuck-driver watchdog with detach-not-join and
  bounded respawns, `catch_unwind` supervision. Controllers are opened **on
  the worker thread** (COM apartment correctness).
- **Binaries** — `duja --headless | --once | --stress <secs> [--hz n]`
  (the stress harness wraps controllers in counting decorators and reports the
  coalescing ratio, restoring baseline levels) and **`dujactl`**
  (`list · get · set <id|all> brightness <n> · doctor · version`, direct
  in-process backends until P5 IPC; exit codes 0/2/3/4).
- **P3 gate**: adversarial review found five real seam defects — restore-level
  clobbering on stuck-then-replugged displays, a debouncer double-poll that
  could drop an enumeration, COM apartment misuse across threads, un-seq-gated
  Get acks, and twin `-slot<n>` misrouting — all fixed test-first (PR #12).

## What remains

- **Hardware sign-off for P3 (needs a human at the console)** — every CI-side
  criterion is green, but the live DDC round-trip could not run: this work was
  driven from a **disconnected** session, and Windows only exposes real
  displays to the interactive console session. When logged in at the machine,
  run:
  ```powershell
  $env:DUJA_HW_TESTS = "1"; cargo nextest run -p duja-ddc --run-ignored all
  cargo run -p dujactl -- list        # expect the MSI MP273QP with MSI-30B6-… id
  cargo run -p dujactl -- set all brightness 40   # watch the panel change; set it back
  cargo run -p duja-app -- --stress 60            # coalescing ratio ≪ 100 writes/100 inputs, zero errors
  ```
  Manual checks from the plan: unplug/replug restores the level ≤ 2 s;
  sleep/resume restores; monitor power-cycle recovers.
- **P4 (next)** — `duja-dimmer` overlay (productionize the spike: per-monitor
  click-through windows, HDR guard, gamma opt-in, crash marker + `--restore`)
  + Slint flyout & tray + view-models → daily-driver MVP (`v0.1.0-alpha`).
  Perf budgets measured at this gate (docs/perf-budgets.md).
- **P5** — global hotkeys, VCP 0x60 input switching, IPC transport hardening +
  `dujactl` over IPC, settings UI, opt-in update check (`v0.2.0-beta`).
- **P6/P7** — macOS and Linux ports behind the same trait + contract suite.
- **P8** — fuzz burn-in, soak, packaging (winget/brew/AUR), 1.0.
- Deferred oddments live in [debt.md](debt.md) (notably: the WMI set-path needs
  a borrowed laptop; `classify_failure`'s `GetLastError` assumption needs a
  live unplug test).

## Notes & gotchas for whoever continues

- **Environment**: Rust pinned 1.96.1 (MSRV 1.94), MSVC, edition 2024.
  Smart App Control must stay **off** on the dev box (os error 4551 otherwise).
  Fuzzing on Windows needs the MSVC ASan DLL on `PATH`; see
  [fuzz/README.md](../fuzz/README.md).
- **Session trap (cost us the P3 live run)**: processes in a disconnected
  session see no real displays — `duja-ddc`/dxva2 correctly return nothing,
  and `dujactl doctor` prints a hint. Check `qwinsta` before blaming the code.
- **Linux rustdoc trap**: the rustdoc CI job runs on ubuntu — intra-doc links
  from cross-platform code to `#[cfg(windows)]`-only items break it. Use plain
  backticks for those (bit PRs #8 and #10).
- **CI debt**: coverage and fuzz gates still run locally only (P8 item).
- **Workflow**: trunk-based, squash-merge PRs only, conventional commits
  (lowercase subjects ≤ 72 chars), milestone tags at phase exits. The
  commit-lint job is advisory on branch commits; PR titles are what land on
  `main`.

# Duja — Project Status

_Last updated: 2026-07-10 (end of P4 Windows MVP)._

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
| P4 Windows dimmer + UI (MVP) | `m4-win-mvp` | ✅ done (`v0.1.0-alpha` waits on the console-session QA below) |
| P5 Power features | `m5-win-full` / `v0.2.0-beta` | ⏭️ next |
| P6 macOS port | `m6-macos` / `v0.3.0-beta` | pending |
| P7 Linux port | `m7-linux` / `v0.4.0-beta` | pending |
| P8 Hardening → 1.0 | `m8-hardening` / `v1.0.0` | pending |

Health: **466 tests + doctests green on 3 OSes**, clippy `-D warnings` clean,
`cargo-deny` clean, 4 fuzz targets (caps, EDID, quirks, IPC frame) building on
stable, adversarial gate reviews at P2/P3/P4 with all findings fixed
test-first before tagging. Perf (headless-measurable, P4 gate): idle RSS
**23.3 MB** (budget ≤ 35), idle CPU **0 ms over 20 s** (zero wakeups),
`duja.exe` 14.9 MB (budget raised to ≤ 16 MB by ADR-0012; P8 trims),
`dujactl` 0.6 MB.

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

### P4 — Windows MVP (PRs #13–#16)
- **`duja-ui`** — Slint flyout on the ADR-0009 software-renderer stack:
  pure-Rust view-models (rows, link-all fan-out, themes; zero Slint types in
  signatures), presentation-only `.slint` (light/dark, `@tr` strings, keyboard
  + accessibility, no timers/animations), `FlyoutShell` confining Slint types.
- **`duja-dimmer`** — the ADR-0003 spike productionized: pure `plan` diffing
  kernel; a dedicated thread owning per-monitor click-through layered overlays
  (`SetLayeredWindowAttributes`, `HTTRANSPARENT`, `WDA_EXCLUDEFROMCAPTURE`);
  opt-in gamma with a safety floor; `ScreenStateGuard` + crash marker +
  `restore_all` (a gamma ramp outlives a dead process — this is the
  "never brick a screen" mechanism); DXGI HDR detection.
- **`duja-app` tray assembly** — the app owns the continuum: persisted user
  level → floored hardware target to the engine + declarative overlay/gamma
  batch to the dimmer (HDR ⇒ overlay-only, decided once per enumeration).
  Single instance (per-user named mutex), config + debounced state
  persistence, crash-marker recovery at startup, tray icon + four-edge flyout
  anchoring, `tracing` logging, PerMonitorV2 manifest, real `--restore`.
- **Supply chain**: Slint royalty-free license exception (scoped per-crate),
  documented advisory ignores for Slint/tray-icon-pinned transitives (GTK3
  family until the P7 `ksni` decision, ADR-0010).
- **P4 gate**: adversarial review found two real seam defects — the UI-side
  throttle could drop a drag's final value (hardware stranded at a mid-drag
  level while the UI looked right; throttle deleted, the engine coalescer is
  the single pacing authority) and `dim_mode = "gamma"` never reached the
  gamma API (now wired via a pure `GammaCoordinator` + `ScreenStateGuard`
  sink, activating the crash-marker machinery). Both fixed test-first.
- **Budget variance**: `duja.exe` 14.9 MB vs the 12 MB budget — raised to
  16 MB by [ADR-0012](adr/0012-binary-size-budget-variance.md) with P8 trim
  levers; RAM and wakeup budgets pass with headroom.

## What remains

- **One console-session checklist (needs you at the machine)** — everything
  CI-verifiable is green; this work ran in a **disconnected** session where
  Windows exposes no real displays. When logged in at the desk:

  **P3 hardware sign-off:**
  ```powershell
  $env:DUJA_HW_TESTS = "1"; cargo nextest run -p duja-ddc --run-ignored all
  cargo run -p dujactl -- list        # expect the MSI MP273QP with MSI-30B6-… id
  cargo run -p dujactl -- set all brightness 40   # watch the panel change; set it back
  cargo run -p duja-app --release -- --stress 60  # writes ≪ inputs, zero errors
  ```
  Then: unplug/replug restores the level ≤ 2 s; sleep/resume restores; monitor
  power-cycle recovers.

  **P4 visual QA** (run `target\release\duja.exe`):
  1. Tray icon visible with "Duja" tooltip, legible on light *and* dark taskbars.
  2. Left-click toggles the flyout near the tray, fully on-screen (try a
     top/left taskbar and mixed DPI if available).
  3. Right-click menu: Open / Restore screen / Quit (clean exit, state saved).
  4. Slider drives real brightness; below the hardware floor the overlay
     engages seamlessly (no visible jump at the handoff); link-all fans out.
  5. Overlays never intercept clicks; screenshots stay undimmed.
  6. Unresponsive display greys out; hot-plug refreshes rows.
  7. Double-clicked release exe opens no console; WARN log rotates under
     `%LOCALAPPDATA%`.
  8. Cold start → tray icon < 300 ms (feel; tracing span has the number).
- **P5 (next)** — global hotkeys, VCP 0x60 input switching, IPC transport
  hardening + `dujactl` over IPC, settings UI, opt-in update check
  (`v0.2.0-beta`, public). Security checklist re-run item-by-item at its gate.
- **P6/P7** — macOS and Linux ports behind the same trait + contract suite.
- **P8** — fuzz burn-in, soak, packaging (winget/brew/AUR), binary-size trims
  (ADR-0012), 1.0.
- Deferred oddments live in [debt.md](debt.md) (notably: the WMI set-path needs
  a borrowed laptop; `classify_failure`'s `GetLastError` assumption and
  suspend/resume DDC re-push need live-hardware evidence).

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

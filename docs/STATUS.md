# Duja — Project Status

_Last updated: 2026-07-10 (end of P5 — Windows feature-complete)._

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
| P3 Windows hardware slice | `m3-win-hw` | ✅ done |
| P4 Windows dimmer + UI (MVP) | `m4-win-mvp` | ✅ done |
| P5 Power features (Windows complete) | `m5-win-full` | ✅ done |
| P6 macOS port | `m6-macos` / `v0.3.0-beta` | ⏭️ next |
| P7 Linux port | `m7-linux` / `v0.4.0-beta` | pending |
| P8 Hardening → 1.0 | `m8-hardening` / `v1.0.0` | pending |

**Release tags (`v0.1.0-alpha`, `v0.2.0-beta`) are deliberately unset** — they
are gated on the one-time console-session QA below, which cannot run from the
disconnected session this work was built in. See "What remains".

Health: **570 tests + doctests green on 3 OSes**, clippy `-D warnings` clean,
`cargo-deny` clean (advisories/bans/licenses/sources), 4 fuzz targets building
on stable, adversarial gate reviews at **P2, P3, P4, P5** with every confirmed
finding fixed test-first before tagging.

Measured (headless, P4/P5 gates): idle RSS **23.3 MB** (budget ≤ 35),
idle CPU **0 ms over 20 s** — zero wakeups, by construction.
`duja.exe` **17.21 MB** (over the 16 MB budget — see
[ADR-0012](adr/0012-binary-size-budget-variance.md)); `dujactl.exe` 0.6 MB.

## What is done

### P0–P2 — foundation, spikes, and the pure core
- 9-crate workspace + xtask; lint wall (`deny` unwrap/expect/panic/todo/
  undocumented-unsafe; pedantic warnings; `forbid(unsafe_code)` in the pure
  crates); 3-OS CI (fmt, clippy `-D warnings`, nextest, deny, MSRV, rustdoc,
  commit-lint), SHA-pinned actions, branch protection, dual MIT/Apache-2.0.
- Spikes settled the load-bearing risks **with measurements, not opinions**:
  Slint + `tray-icon` + `global-hotkey` cohabit on one Windows main thread with
  zero idle wakeups (ADR-0001); the **software renderer** is the only one
  meeting both RAM and binary budgets (ADR-0009); `ddc-hi` is unusable on
  Windows (duplicate monitors, no EDID, `!Send`, dormant) so Duja owns its DDC
  backend (ADR-0002); the click-through overlay recipe works and screenshots
  stay undimmed (ADR-0003).
- `duja-core` (pure, no OS APIs, no `unsafe`): EDID → `StableDisplayId`,
  the frozen `BrightnessController` trait, the brightness **continuum**
  (hardware level + overlay alpha with a seamless floor handoff), pure
  debounce/coalesce state machines, the hot-plug `DisplayManager` (twin
  slotting, replug restore, unresponsive marking), sync groups, format-
  preserving config with chained migrations + atomic writes, a total MCCS caps
  parser, the quirk DB, and the reusable cross-backend **contract suite**.
  ~96 % line coverage; 3×1 M fuzz executions clean.

### P3 — Windows hardware slice (`m3-win-hw`)
- **`duja-ddc`**: in-house dxva2 backend. `VcpTransport` seam + `DdcController`
  owning all policy (quirk-driven pacing, retry with backoff, verify-by-
  readback, `max_brightness` override, caps fallback, `ddc_broken`).
  Enumeration bridges HMONITOR → CCD (`QueryDisplayConfig`) → SetupAPI registry
  EDID, defeating the NVIDIA `Default_Monitor` stub that mislabels connectors
  on this machine. Verified against the real MSI MP273QP EDID.
- **`duja-platform`**: hidden top-level window pump (`WM_DISPLAYCHANGE`, monitor
  device notifications, suspend/resume, session unlock) → normalized events.
- **`duja-panel`**: WMI internal-panel backend (raw COM); graceful empty
  enumeration on panel-less desktops.
- **`duja-app` engine**: controller actor + per-monitor workers (std threads +
  crossbeam, ADR-0005), latest-wins per-feature coalescing, 5 s stuck-driver
  watchdog (detach, never join a hung GPU driver), `catch_unwind` supervision.
- Gate review found **5 real seam defects** (restore-level clobbering, a
  debouncer double-poll that dropped enumerations, COM apartment misuse across
  threads, un-seq-gated acks, twin `-slot<n>` misrouting) — all fixed test-first.

### P4 — Windows MVP (`m4-win-mvp`)
- **`duja-ui`**: Slint flyout with **pure-Rust view-models** (zero Slint types in
  signatures), presentation-only `.slint` (light/dark, `@tr`, keyboard + a11y,
  no timers/animations).
- **`duja-dimmer`**: pure `plan` diffing kernel + a thread owning per-monitor
  click-through layered overlays; opt-in gamma with a safety floor;
  `ScreenStateGuard` + crash marker + `restore_all` — the "never brick a screen"
  mechanism (a Windows gamma ramp outlives a dead process); DXGI HDR detection.
- **Tray assembly**: the app owns the continuum (persisted user level → floored
  hardware target to the engine + declarative overlay/gamma batch to the
  dimmer; HDR ⇒ overlay-only). Single instance, config + debounced state
  persistence, crash-marker recovery, tray icon + four-edge flyout anchoring,
  `tracing` logging, PerMonitorV2, real `--restore`.
- Gate review found **2 real seam defects**: a leading-edge UI throttle could
  strand hardware brightness at a mid-drag value while slider/overlay/state all
  looked correct (throttle deleted — the engine coalescer is the single pacing
  authority *and* guarantees final-value delivery); and `dim_mode = "gamma"`
  never reached the gamma API (silently dead, along with the crash-marker
  machinery). Both fixed test-first.

### P5 — Windows feature-complete (`m5-win-full`)
- **Global hotkeys**: pure accelerator parser + conflict detection, **no default
  bindings** (commented examples in the emitted config), WARN-and-skip on
  registration failure; `brightness_up/down` (±5, all displays, same path as the
  flyout) and `toggle_flyout`.
- **Input switching (VCP 0x60)**: `Capabilities.allowed_inputs` = caps-string
  value list ∩ `input_source_allowed` quirk, cleared by `no_input_switch`.
  Double-gated (engine + controller); raw write, no verify-readback (ADR-0002 —
  monitors lie about 0x60 metadata). `dujactl input` is the documented recovery
  path; **no auto-revert**.
- **IPC transport**: per-user named pipe with an explicit user-only DACL *and*
  explicit owner, anti-squat first-instance flag, remote-client rejection,
  client PID + session verification, ≤4 instances / 2 handler threads,
  exchange-wide 5 s read deadline. Built on **overlapped I/O** with
  `CancelIoEx` cancellation. `dujactl` speaks IPC-first with silent fallback to
  the direct hardware backend; second instance forwards `ShowFlyout`.
- **Settings window**, **autostart** (in-house trait over the HKCU Run key), and
  an **opt-in update check** (off by default; one HTTPS GET over rustls, body
  capped at 64 KiB before buffering, conservative semver, opens the browser —
  **never downloads**).
- Gate: adversarial review + **security checklist §6 item-by-item** +
  **unsafe audit #2**. Results below.

## P5 gate results

**Security checklist §6** — every item PASS, each with a proving test: pipe
naming / DACL / explicit owner / anti-squat / remote-rejection / PID+session
check; 64 KiB pre-allocation cap, versioned envelope, strict validation,
connection cap, read timeout; no telemetry and no network by default; quirks
typed-serde + 1 MiB cap + bounded glob (no regex); never elevates (HKCU only,
`asInvoker`); overlay input-transparency flags asserted by a live test. Items 4
(release signing) and 8 (SECURITY.md) verdict on documentation only.

**Unsafe audit #2** — clean bill across `duja-platform`, `duja-ddc/win`,
`duja-panel/wmi`, `duja-dimmer/win`: every `unsafe` block carries a `// SAFETY:`
comment whose stated invariant is *true* (OVERLAPPED reap-before-drop, packed/
union access after tag checks, single-owner `Send` justifications matching
actual usage, RAII single-close on every handle).

**Adversarial review** — 2 confirmed defects, both fixed test-first:
1. The IPC read deadline was minted **per `read()` syscall**, so a same-user peer
   dribbling one byte every few seconds renewed it forever and pinned a handler
   thread (two such clients would deny the whole IPC server). The deadline is
   now armed **once per exchange**. The regression test was verified red against
   the old code (8 dribbled bytes kept the handler alive indefinitely).
2. The first pipe instance leaked on thread-spawn failure.

Earlier in P5, integration also caught a **deadlock** that the feature agent's
own green test runs had missed: the transport timed reads by polling
`PeekNamedPipe`, which is *not* guaranteed non-blocking — a silent read-only
client froze a handler inside the syscall forever. That triggered the overlapped
I/O rewrite, which in turn surfaced a second latent bug (the stop path returned
`ErrorKind::Interrupted`, which `read_exact` silently retries into an infinite
spin — now `ConnectionAborted`).

## What remains

### 1. One console-session QA pass (needs a human at the machine)

Everything CI-verifiable is green. This work ran in a **disconnected** Windows
session, where the OS exposes no real displays to any process — so live DDC,
tray visuals, and overlay rendering could not be exercised. Log in at the desk
and run:

**Hardware sign-off (P3):**
```powershell
$env:DUJA_HW_TESTS = "1"; cargo nextest run -p duja-ddc --run-ignored all
cargo run -p dujactl -- list                     # expect MSI MP273QP, id MSI-30B6-…
cargo run -p dujactl -- set all brightness 40    # panel changes; set it back
cargo run -p duja-app --release -- --stress 60   # writes << inputs, zero errors
```
Then: unplug/replug restores the level ≤ 2 s; sleep/resume restores; monitor
power-cycle recovers.

**Visual QA (P4/P5)** — run `target\release\duja.exe`:
1. Tray icon + "Duja" tooltip, legible on light **and** dark taskbars.
2. Left-click toggles the flyout near the tray, fully on-screen (try a
   top/left taskbar and a mixed-DPI setup).
3. Right-click menu: Open / Settings / Restore screen / Quit (clean exit).
4. Slider drives real brightness; below the hardware floor the overlay engages
   with **no visible jump** at the handoff; link-all fans out.
5. Overlays never intercept clicks; screenshots stay undimmed.
6. Unresponsive display greys out; hot-plug refreshes rows.
7. Settings window: general toggles, theme, per-monitor floor/dim-mode/input
   rows, hotkey list; Esc closes; palette matches the flyout.
8. `dujactl input <id>` lists HDMI-1/HDMI-2/DP; `dujactl input <id> hdmi2`
   switches the physical input (recover with `dujactl input <id> <prev>`).
   **No automated test ever writes VCP 0x60** — it would black out the screen.
9. Bind a hotkey in `config.toml`, confirm it fires globally; a combo another
   app owns logs a WARN and is skipped.
10. Double-clicked release exe opens no console; WARN log rotates under
    `%LOCALAPPDATA%`.

When this passes, tag `v0.1.0-alpha` (and `v0.2.0-beta` for the public beta).

### 2. Known gaps carried forward
- **Binary 17.21 MB > 16 MB budget** — P8 must recover it (ADR-0012 ledger).
- **WMI panel set-path** has never executed on real hardware (this box is a
  desktop): borrow a laptop for a 30-minute run before the beta.
- Suspend/resume does not re-push DDC levels when the display set is unchanged;
  `classify_failure`'s `GetLastError` assumption needs a live unplug.
- Quirk user-override file, sync-group UI, in-UI hotkey editing, OS theme
  detection — all tracked in [debt.md](debt.md).

### 3. Phases
- **P6 macOS** (hardware-blind: CI runners + community verification),
  **P7 Linux** (VM-assisted; GNOME Wayland dimming spike first),
  **P8 hardening** → fuzz burn-in, 72 h soak, packaging, size trims, 1.0.

## Notes & gotchas for whoever continues

- **Environment**: Rust pinned 1.96.1 (MSRV 1.94), MSVC, edition 2024.
  Smart App Control must stay **off** on the dev box (os error 4551 otherwise).
  Fuzzing on Windows needs the MSVC ASan DLL on `PATH` (see
  [fuzz/README.md](../fuzz/README.md)).
- **Session trap**: a disconnected session sees no displays — `duja-ddc`
  correctly returns nothing and `dujactl doctor` says so. Check `qwinsta`
  before blaming the code.
- **Linux rustdoc trap**: the rustdoc CI job runs on ubuntu; intra-doc links
  from cross-platform code to `#[cfg(windows)]`-only items break it. Use plain
  backticks there. (Broke PRs #8, #10, #17.)
- **Elevated-token trap**: an elevated process's default object owner is the
  Administrators group, not the user — the pipe's SDDL therefore sets the owner
  explicitly (`O:<sid>`), or the DACL owner assertion fails under CI.
- **Test-process hygiene**: if an `ipc_pipe-*.exe` lingers after a run, that is
  a hang, not noise — investigate it.
- **Workflow**: trunk-based, squash-merge PRs only, conventional commits
  (lowercase subjects ≤ 72 chars), milestone tags at phase exits. The
  commit-lint job is advisory; PR titles are what land on `main`.
- **Lesson worth keeping**: three separate defects (the peek-poll deadlock, the
  dribble slowloris, the P4 throttle) were invisible to per-crate test suites
  and to green agent reports. The phase-gate adversarial review — plus insisting
  every regression test be proven **red** before its fix — is what caught them.

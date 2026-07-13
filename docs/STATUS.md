# Duja — Project Status

_Last updated: 2026-07-13 (repo audit cleanup; Windows UI hardening #27–#30)._

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
| P6 macOS port | `m6-macos` / `v0.3.0-beta` | 🚧 in progress — wave 1 (backends) landed; wave 2 (app assembly + packaging) + gate remain |
| P7 Linux port | `m7-linux` / `v0.4.0-beta` | pending |
| P8 Hardening → 1.0 | `m8-hardening` / `v1.0.0` | pending |

**Release tags (`v0.1.0-alpha`, `v0.2.0-beta`) are still unset.** The **hardware
sign-off now PASSES on real hardware** (live console-session QA, 2026-07-11 —
see "Live hardware QA"); the alpha tag is gated only on the remaining
**pure-visual** checks (tray/flyout/overlay appearance), which need human eyes.

Health: **669 tests + doctests green on 3 OSes**, clippy `-D warnings` clean,
`cargo-deny` clean (advisories/bans/licenses/sources), 5 fuzz targets building
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

### P6 — macOS port, wave 1 (backends landed 2026-07-11)

Hardware-blind by design: Duja has no Mac, and CI's `macos-latest` runners are
virtualized. Everything here is proven by CI (the mac lanes actually **compile
and run** the FFI) and the pure cross-platform logic; real-hardware DDC/panel
behaviour is community-gated (see [debt.md](debt.md) and ADR-0013). Five crate
seams merged as PRs #21–#25, each green on all three OSes:

- **`duja-panel` — DisplayServices** (PR #21): the private
  `DisplayServices.framework` dlopen'd at first use (three symbols; missing
  framework/symbol ⇒ backend contributes nothing). Builtin-only enumeration
  gated by `CanChangeBrightness`; `StableDisplayId` synthesized from CG
  vendor/model/serial. 0.0–1.0 float ↔ integer-level mapping. Contract suite
  bound against a fake DisplayServices table.
- **Unix-socket IPC** (PR #22): the hardened named-pipe's unix twin
  (`#[cfg(unix)]`, serves P7 Linux too) — dir `0700` + socket `0600`, peer-euid
  check (`getpeereid`/`SO_PEERCRED`), stale-socket takeover, **exchange-wide**
  read deadline, `ConnectionAborted` (never `Interrupted`) on the stop path. The
  P5 IPC findings were handed to the agent as explicit non-regressions. 13
  integration tests **run live** on the ubuntu + macos lanes.
- **`duja-dimmer` — NSWindow overlays + gamma** (PR #23): reuses the pure `plan`
  kernel; per-display click-through borderless windows
  (`ignoresMouseEvents`, all-Spaces, shielding level), alpha marshalled to the
  **main dispatch queue** (solves the AppKit main-thread rule; documented
  divergence from the Windows *blocking* `apply`). Gamma via
  `CGSetDisplayTransferByFormula`; the crash-marker machinery is intentionally
  **absent** — macOS auto-restores gamma on process exit. A live window-server
  smoke test ran on the mac runner.
- **`duja-ddc` — DDC/CI** (PR #25): a pure, host-tested wire codec (`DdcWire`
  encodes both the Intel frame **and** the distinct Apple-Silicon I2C framing —
  they are *not* the same, a real trap) driving two transports — IOAVService
  (Apple Silicon) and IOI2C (Intel), private symbols dlsym'd. **All** controller
  policy (pacing/retry/verify/quirks) is reused. ADR-0013 records the
  own-vs-wrap decision (own a thin backend, don't wrap `ddc-macos`). 58 host-run
  codec tests + a 5th fuzz target `fuzz_ddc_packet`.
- **`duja-platform` — pump + single-instance + autostart** (PR #24): a dedicated
  `CFRunLoop` thread (`CGDisplayRegisterReconfigurationCallback` +
  `IORegisterForSystemPower` → `DisplaysChanged`/`Suspending`/`Resumed`); a real
  `#[cfg(unix)]` advisory-`flock` single-instance (serves P7 too); a `launchd`
  LaunchAgent-plist autostart. `SessionUnlocked` is unmapped (only a private
  notification exists) — re-apply leans on `Resumed` + `DisplaysChanged`.

**Traps surfaced (recorded so they are not re-learned):** macOS rejects
`SO_RCVTIMEO` on `AF_UNIX` with `EINVAL` (the unix IPC uses `poll(2)` instead —
caught only because the mac CI lane *ran* the tests); Apple-Silicon DDC framing
≠ standard MCCS; `sharingType = .none` no longer reliably excludes windows from
capture on macOS 15+ (best-effort on mac, unlike the guaranteed Windows
`WDA_EXCLUDEFROMCAPTURE`); mac `DisplayBounds`/`NSWindow` frames are in **points**
(y-flipped), not pixels. The mac **app assembly** (tray/flyout wiring, DMG +
universal2 packaging, UI-launch CI smoke) is **wave 2**, not yet started, so
`duja-app`/`duja-ui` still use their non-Windows stubs on macOS and there is no
`m6-macos` tag or `v0.3.0-beta` yet.

### Windows UI hardening (#27–#30, live-QA driven)

Four rounds of on-hardware visual QA (real console session, external monitor)
hardened the flyout and settings windows past what the automated suites could
see — each fix landed with a red-first regression test:

- **#27** — five P0 defects: window placement at fractional DPI; a slider that
  would not drag (the row model was rebuilt every render, destroying the mid-drag
  element — now diffed in place, never `set_vec`); theme/floor changes crashing (an
  edition-2024 borrow held across a re-render double-borrowed the view-model and
  aborted — now a re-entrancy-safe `with_app` dispatcher); missing dimming toggle
  and close affordances.
- **#28** — premium redesign: custom brightness slider, dimming pill toggle,
  gear/close buttons, functional editable hotkey rows.
- **#29** — root-caused the fractional-DPI "dead space" to a DPI-unaware
  `GetClientRect` measurement artifact (the "compensated layout" that chased it
  *was* the bug) and removed it; closed four live-QA regressions incl. the frozen
  "Link all" pill (now covered by a binding-layer test on the real widget).
- **#30** — the partial first paint: the winit software renderer presents only a
  non-empty damage region, and a post-`show()` resize aged an empty region that
  never presented (a transparent frame until clicked). Fixed by sizing + anchoring
  while hidden and showing once (**60/60 clean** vs 3-in-20 failing). The app now
  **adopts** the panel's current hardware brightness on launch and writes nothing
  (no launch-time dimming); slimmer footer pill; 1 px window-edge borders.

A follow-up repo audit (2026-07-13) then fixed the settings window not following
the Light/Dark theme (the shell never pushed the resolved palette to it — now
covered by a settings binding test), removed three unused dependencies, and
reconciled the docs in this file and the ADR index with the code.

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

## Live hardware QA (2026-07-11, console session, MSI MP273QP over DDC)

The build finally ran on a **connected** console session with the external
monitor attached, so the functional half of the long-pending QA is now done on
real hardware. **The P3 hardware sign-off PASSES.** What was exercised (all
against the physical MSI MP273QP, brightness restored to 70 afterwards):

- **DDC enumeration / control** — `dujactl list` finds the monitor
  (`MSI-30B6-PB6H013202527`, brightness/contrast/input); `get`, `set`, and
  `set all` round-trip on the panel; `doctor` reports the real quirks
  (min_gap=50 ms, caps_retry=3, verify_writes); `input <id>` lists
  hdmi1/hdmi2/dp1 with the current input marked **read-only** (no `0x60` write).
- **Hardware contract suite** — `DUJA_HW_TESTS=1 cargo nextest -p duja-ddc
  --run-ignored all`: **50/50 pass**, including `hw_enumerates_msi_monitor` and
  `hw_contract_suite_real_monitor` (the full cross-backend contract against the
  live panel, brightness restored by drop guard). First time the DDC backend has
  been proven on real hardware rather than fakes.
- **Coalescing under flood** — `duja --stress` at 20–25 Hz: ~300 inputs collapse
  to ~60–90 hardware writes (19–31 writes/100 inputs), exactly one calibration
  read, **zero** false-unresponsive. See the transient-error note below.
- **Full app → IPC → engine → hardware path** — with `duja --headless` up,
  `dujactl` reports **"served over ipc"** and drives the physical panel through
  the running app's engine (set 55 → readback 55 → restore 70); `doctor` shows
  the IPC server reachable; **clean exit (code 0)**. (Startup contract, changed in
  UI round 4 / item 5: the app now **adopts the panel's current hardware
  brightness** on launch — it mirrors reality into the UI and writes nothing. The
  old behaviour of force-pushing the *persisted* continuum level on startup dimmed
  the monitor to the last-saved level on every launch and was removed; only a
  genuine user action writes to hardware thereafter.)
- **Tray GUI stability** — `duja.exe` launches with the Slint software renderer +
  tray without crashing, **no console window**, and idle-samples **flat: RSS
  24.8 MB, 296 handles** (no leak) over the idle window — within the ≤ 35 MB
  budget. `duja --restore` clears overlays and resets identity gamma.

**Finding — transient DDC errors under sustained flood (recorded, not a
Duja bug).** Roughly one stress run in five to eight surfaces 1–2 hardware
errors out of ~300 inputs (the monitor NAKs/times-out a DDC exchange even after
the 3-retry budget). Duja degrades **correctly**: the error is surfaced, the
display is **not** marked unresponsive, all subsequent writes succeed, no
cascade, no panic. The `--stress` harness's strict "zero errors ⇒ PASS" gate
therefore reports an occasional FAIL that reflects real DDC/CI wire flakiness,
not a logic defect. Tracked in [debt.md](debt.md); a future harness change
should score an *error rate* threshold rather than absolute zero.

### 1. Remaining QA — the pure-visual checks (still need human eyes)

The functional path is proven above; these items are inherently visual and
cannot be automated — run `target\release\duja.exe` and eyeball:

1. Tray icon + "Duja" tooltip, legible on light **and** dark taskbars.
2. Left-click toggles the flyout near the tray, fully on-screen (top/left
   taskbar, mixed-DPI).
3. Right-click menu: Open / Settings / Restore screen / Quit.
4. Slider drives real brightness; below the hardware floor the overlay engages
   with **no visible jump** at the handoff; link-all fans out.
5. Overlays never intercept clicks; screenshots stay undimmed (the
   `WS_EX_TRANSPARENT` / capture-exclusion flags are asserted by unit tests, but
   the *visual* is unconfirmed).
6. Unresponsive display greys out; hot-plug refreshes rows.
7. Settings window: toggles, theme, per-monitor floor/dim-mode/input rows,
   hotkey list; Esc closes; palette matches the flyout.
8. `dujactl input <id> hdmi2` switches the physical input (recover with
   `dujactl input <id> <prev>`). **No automated test ever writes VCP 0x60** — it
   would black out the screen; the read-only `input <id>` listing was verified.
9. Bind a hotkey in `config.toml`, confirm it fires globally; a combo another
   app owns logs a WARN and is skipped.
10. Sleep/resume + unplug/replug restore the level ≤ 2 s; monitor power-cycle
    recovers. (Also visual/timing — needs a hand on the cable and the power
    button.)

With the hardware sign-off now passing, `v0.1.0-alpha` is gated **only** on
these visual checks; tag it once they look right (and `v0.2.0-beta` for the
public beta).

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

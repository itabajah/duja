# Duja — Project Status

_Last updated: 2026-07-18 (v0.1.2: multi-monitor & capability fix wave —
display identity, capability detection, and linked control, from real-hardware
laptop testing)._

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
| **First release** | **`v0.1.0` (Windows)** | ✅ shipped — installer + portable zip, signed, auto-update loop |
| **Deep-review fix wave** | **`v0.1.1` (Windows)** | ✅ shipped — 10 fix PRs, all confirmed defects fixed test-first |
| **Multi-monitor & capability fixes** | **`v0.1.2` (Windows)** | 🚀 shipping — 6 PRs (5 real-hardware bugs + 1 audit follow-up), test-first, audit + holistic reviewed |
| P6 macOS port | `m6-macos` / `v0.2.0` | 🚧 in progress — wave 1 (backends) landed; wave 2 (app assembly + packaging) + gate remain |
| P7 Linux port | `m7-linux` / `v0.3.0` | pending |
| P8 Hardening → 1.0 | `m8-hardening` / `v1.0.0` | pending |

_Version ladder re-mapped in [ADR-0019](adr/0019-version-ladder-and-release-trains.md):
v0.1.x Windows train, v0.2.0 macOS, v0.3.0 Linux, v1.0.0 hardening._

**`v0.1.0` is the first public release.** The hardware sign-off passed on real
hardware (2026-07-11, see "Live hardware QA") and the **pure-visual QA is now
signed off** (user, 2026-07-16), which were the two gates. Shipping as a clean
**stable** `v0.1.0` (not `-alpha`) so the built-in update checker — which only
prompts on newer *stable* releases via GitHub's `/releases/latest` — works end to
end from day one. Distribution is a tag-triggered
[`release.yml`](../.github/workflows/release.yml): an Inno Setup installer + a
portable zip, each with `SHA256SUMS`, a minisign signature, and a build-provenance
attestation.

Health: **803 tests + doctests green on 3 OSes**, clippy `-D warnings` clean,
`cargo-deny` clean (advisories/bans/licenses/sources), 5 fuzz targets building
on stable, adversarial gate reviews at **P2, P3, P4, P5** plus a full post-v0.1.0
**deep review** (14 module reviewers, every non-low finding adversarially
verified) with every confirmed finding fixed test-first.

### v0.1.1 — deep-review fix wave (2026-07-17)

After v0.1.0 shipped, a 14-module line-by-line review (Opus reviewers, findings
double-checked by adversarial verifiers — final tally **45 confirmed, 6 refuted,
1 uncertain**) audited the whole codebase. `duja-core`, the IPC stack, and the
Windows event pump/single-instance/autostart came back **verified-clean**; the
confirmed defects were fixed across **10 PRs (#46–#55)**, each landed test-first
(red-first regression proven) and reviewed by a separate adversarial agent before
merge — the same discipline that has caught every real seam defect. Highlights:

- **Concurrency/lifecycle** (`engine`, ADR-0017): a bounded shutdown that no
  longer hangs app-exit on a wedged driver call; a generation + `retired` backbone
  so a detached worker can never become a second writer to a panel; a failed
  controller open now greys-and-recovers instead of silently losing control.
- **Never-brick** (`dimmer`, `app`): overlay windows destroyed on error (no leak),
  capture-exclusion failure degraded not fatal, the gamma crash marker preserved on
  a failed restore (including the clean-quit path), the overlay apply bounded so a
  wedged worker can't freeze the UI, and the HDR gamma verdict re-probed live
  instead of frozen at launch.
- **User-facing correctness**: `dujactl set all` over IPC; a hot-plug during a
  slider drag no longer retargets the drag to the wrong monitor; a zero-max DDC
  reply can no longer drive a panel dark; EDID identity keyed on the base block so
  per-monitor config isn't lost.
- **Security / supply chain**: release-pipeline script-injection closed and the
  publish gated on the full CI; LF checksums; `dujactl` verifies the pipe server's
  SID; the installer detects a running instance.

Refactor/test debt this surfaced (tray.rs split, per-display HDR, a CI headless
E2E smoke, the throttle-at-tray regression test, `ddc_broken` routing) is tracked
in [debt.md](debt.md); ADRs **0017–0020** record the new contracts.

### v0.1.2 — multi-monitor & capability fix wave (2026-07-18)

Real-hardware testing on a laptop (internal panel + one external monitor,
including Windows *Duplicate*/mirror mode and *Link all*) surfaced five defects in
the one configuration the desktop dev box never exercised. Each was fixed
test-first (red-first regression) and reviewed by a separate adversarial agent;
after all five merged, an **audit sweep** + a **holistic integration review** ran
on the combined result — the holistic came back **INTEGRATION CLEAN** across six
cross-cutting paths, and the audit found two seam issues (an over-broad
enumeration probe and a reattach-recovery gap) that a sixth PR fixed, its own
review in turn catching and reverting a regressive over-eager cache drop before
this tag. Six PRs (#57–#62):

- **Display identity** (`ddc`): internal laptop panels are classified via
  `outputTechnology` and deduped against WMI (no more "External" mislabel, no
  duplicate row); *Duplicate*/mirror mode emits one controllable row per physical
  panel via a paced-retried handle probe, bounded so a silent internal handle
  can't stall enumeration.
- **Capability detection** (`engine`): a monitor with no working DDC brightness
  auto-downgrades to full-range software dimming — a retried verify-first-write
  distinguishes a slow panel from a dead one, an overlay-based `software_forced`
  flag survives a silent re-enumeration, and a poll-driven self-heal (plus a clean
  replug re-detection) restores hardware control if the panel later proves live.
- **Linked control** (`ui`): *Link all* preserves each monitor's offset
  (drift-free `SyncGroups`) instead of snapping to one value, and the passive
  linked sliders track instantly instead of gliding.

Residuals (all narrow, self-bounding, hardware-conditional) are tracked in
[debt.md](debt.md); the `ddc_broken`→SoftwareOnly routing deferred from v0.1.1 is
now delivered by the capability-detection work.

Measured (headless, P4/P5 gates): idle RSS **23.3 MB** (budget ≤ 35),
idle CPU **0 ms over 20 s** — zero wakeups, by construction.
`duja.exe` is now **~19 MB** (release, thin LTO): over the 16 MB budget — the
update-check TLS stack plus the WinRT toast bindings added by the v0.1.0 smart
update loop (`UI_Notifications`/`Data_Xml_Dom`). Tracked in
[ADR-0012](adr/0012-binary-size-budget-variance.md)/[debt.md](debt.md); P8 owns
the trim (fat LTO, feature-gating the update stack). `dujactl.exe` ~0.8 MB.

### v0.1.3 — internal-panel fallback fix (2026-07-19)

The v0.1.2 identity fix assumed the WMI panel backend (`duja-panel`) owns every
internal panel, so DDC enumeration skipped internal targets. Real-hardware laptop
testing then found the built-in screen **vanishing entirely** on a machine whose
backlight is GPU/OEM-driven: Windows exposes no `WmiMonitorBrightness` for it, so
the panel appeared in neither backend. Fixed in one adversarially-reviewed PR
(#64), red-first:

- **ddc**: `correlate` now surfaces internal targets (flagged `is_internal`)
  instead of dropping them, and the Windows enumeration binds them **only to a
  physical-monitor handle left over after external pairing** — so the v0.1.2
  mirror-mode routing (external → the DDC-responsive handle) is bit-for-bit
  unchanged.
- **backend**: the discovery merge keeps a DDC-fallback internal panel **only when
  WMI lists no panel** (WMI stays authoritative when it can drive the panel);
  `open_controller` prefers WMI, then DDC-over-eDP, then the engine's software
  overlay. The built-in screen is now always present and controllable.

The review verified handle ownership (the external/internal handle-index sets are
provably disjoint), the unchanged mirror probe count, and that the red-first
`correlate` guard bites against the shipped bug. **Hardware confirmation on the
reporting laptop is pending** (tracked as a QA gate); the fix is strictly additive
— it can only restore the panel, never remove more than before.

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
  the **update check** — one HTTPS GET over rustls, body capped at 64 KiB before
  buffering, opens the browser, **never downloads**. Promoted to a smart-notify
  loop for v0.1.0 (see "v0.1.0 release" below): on by default, once-a-day
  background check piggybacked on interaction (zero idle wakeups), surfaced in the
  tray + a toast, with SemVer-correct precedence.
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

Live QA after the audit showed the partial first paint still recurring on tray
re-open (blank window until a click repainted individual widgets), so #30's
size-while-hidden fix reduced but did not eliminate it. Root-caused one level
deeper in the vendored backend: the winit software renderer presents only the
non-empty dirty region and cures a cleared surface via `WindowEvent::Occluded` —
which **winit 0.30 never emits on Windows** — while Windows discards a hidden
window's redirection surface on hide. So a re-shown window could present a blank
or stale-partial first frame that only repaired when a later click dirtied a
widget. `request_redraw()` only *schedules* a frame; it does not dirty anything.
The cure is a full-window **repaint anchor** (`present-nonce`, bound to the
window-edge Rectangle that fills each window) flipped by the shell immediately
after `show()`: the flip marks the whole window dirty, so the next present covers
it completely. Applied symmetrically to the flyout and settings windows, each
covered by a binding test (proven red against a non-flipping present).

### Perceptual brightness continuum (v2, ADR-0014)

The slider is now **perceptual**: the position *is* perceived brightness, so
"20 % looks 20 % bright" regardless of the hardware floor or panel. Each hardware
display carries a per-display `min_perceived_pct` anchor (default 25, tunable
5–60 in Settings) that sets where hardware zero sits on the slider (line A) and
where hardware hands off to software dimming (line B, at the floor). The floor is
now a **write limit**, not a scale change, so a mid-run floor/anchor change
retargets the hardware without moving the thumb. Consequences: the old
20 %-seed hack behind the "Software dimming" toggle is gone (floor 0 now has a
real software zone below the anchor); the toggle just switches the dim mode; and
launch adoption reflects the live hardware reading through `reverse_map` so the
slider mirrors reality with no first-touch jump.

**External-change reflection.** While the flyout is open the engine polls each
responsive display's hardware level (a new `SetLevelPolling` command; off by
default, so the idle engine keeps its zero-wakeup guarantee), and a reading that
drifts from what Duja last recorded surfaces as `EngineNotification::LevelRead`.
The app reflects it onto the perceptual slider via `reverse_map`, so turning the
monitor's own buttons (or another app changing brightness) moves the thumb
within ~2 s. Two echo gates keep Duja's own writes from bouncing back: the engine
suppresses readings that match its recorded level (and skips a display with a
write in flight), and the app suppresses a reading that matches the hardware the
current slider already drives — which also covers the pinned-floor/overlay case,
so the thumb never jumps to the transition. The reflection path writes no DDC.

**Premium slider.** The flyout slider now draws **two reference lines** — line A
(hardware zero, quiet) and line B (the hardware/software handoff, primary) — which
collapse to one when the floor is 0. It has a gradient accent fill, an accent
thumb glow on hover/press, a value bubble while dragging, hover labels on the two
lines, and a **glide** animation when the level changes externally (the reflection
path). The glide honours the OS "animation effects" accessibility setting
(`SPI_GETCLIENTAREAANIMATION`) and is forced to 0 while the window is hidden or
during a drag; only the rendered thumb glides, so the DDC-never-animates rule is
untouched.

### UI layout & ruby theme (2026-07-14)

A visual/layout pass driven by direct user requests, in four small PRs:

- **Ruby theme.** The shared `Palette` accent moved from blue to a ruby red
  (`accent`/`accent-hover`/`accent-soft`/`thumb-glow`/`focus-ring`), and every
  neutral grey took a subtle warm tint at the same lightness so the whole surface
  reads warm rather than cool. `danger` shifted lighter/pinker (dark) and deeper
  (light) to stay distinct from the accent. One `Palette.dark` bool still drives
  both themes; the two std-widget settings sliders keep their neutral look.
- **Flyout header & inline toggle.** Each row's "Software dimming" toggle now sits
  inline to the right of its slider (was a separate row beneath it); the "Link
  all" toggle moved into the header (after the wordmark), retiring the footer and
  reclaiming a row of height. The manual refresh affordance was dropped from the
  UI — hot-plug auto-refreshes, and the rescan stays wired (`refresh-requested`)
  but unsurfaced, since the software renderer would not ink its glyph. The flyout
  widened 320→360 px.
- **Flyout scroll.** The rows now live in a `ScrollView`: the window still grows
  with the display count up to its max, but beyond that (or on a small screen —
  the height is capped to the work area) the rows scroll instead of clipping,
  matching the settings window.
- **Resizable settings.** The settings window is user-resizable both ways via
  custom frameless edge/corner grips (winit `drag_resize_window`) with
  `preferred`/`min` sizing, and the horizontal scrollbar is gone (the long
  calibration label wraps, the sliders stretch, the ScrollView's horizontal bar
  is off). The `Resized`→`desired` capture keeps the DPI re-assert tracking the
  user's chosen size. The frameless-resize *behaviour* and all the theme/layout
  aesthetics are the pending visual-QA items (below); the code compiles, lints,
  and its binding/geometry logic is unit-covered.

All four areas landed as a single squashed PR (**#39**), following several rounds
of live-QA refinement (the per-row dimming toggle now sits level with the slider;
a light-theme contrast pass deepens the light neutrals so white cards, hairline
borders and off-state pills separate cleanly) and a three-way adversarial
pre-merge review (Slint/UI, Rust lint-wall, cross-file regression). The one Medium
finding — a fractional-DPI scale race in the settings `Resized` capture, which
read the window's provisional scale instead of the monitor's — was fixed (it now
queries the monitor scale, as `enforce_physical_buffer` does).

### v0.1.0 release (2026-07-16)

The first public release turns the Windows-complete build into something users can
install and stay current on:

- **Smart update loop.** The P5 notify-only checker became a real retention loop:
  **on by default** (opt-out via `general.update_check = false`), a **once-a-day**
  background check that piggybacks on user interaction (tray/hotkey events and
  startup) so the **zero-idle-wakeup** guarantee holds — no timer, no poll. A
  newer release surfaces as a prepended **"Update available"** tray item, a tray
  tooltip, and a **WinRT toast** (no new crate — extra `windows` features; AUMID
  `io.github.itabajah.duja`, matched by the installer shortcut). The version
  compare is now full **SemVer** precedence (pre-release ordering, build metadata
  ignored) — future-proofing the alpha/beta line — while GitHub's
  `/releases/latest` keeps betas from prompting stable users. Still **never
  downloads or installs**.
- **Distribution.** A tag-triggered
  [`release.yml`](../.github/workflows/release.yml) on `windows-latest` builds
  `--release --locked`, stages a portable zip via the (dependency-free)
  `xtask dist`, compiles an **Inno Setup** installer
  ([`packaging/windows/duja.iss`](../packaging/windows/duja.iss); per-user, no
  UAC, optional launch-at-login writing the *same* HKCU Run value as the in-app
  autostart), then emits `SHA256SUMS`, **minisign** signatures, and a
  **build-provenance** attestation, and publishes the GitHub Release with
  git-cliff notes. A tag/version guard fails fast on a mismatched tag; a
  `workflow_dispatch` runs the whole thing as an artifacts-only dry run.
- **Docs & brand.** A premium README (hero rendered from the faceted-gem
  whirlpool mark, badges, install/verify sections), a social-preview card, and the
  threat-model/SmartScreen/verification notes in [SECURITY.md](../SECURITY.md).
- **Not signed.** No Authenticode certificate yet, so SmartScreen warns on first
  run; authenticity is via the checksums + minisign key + provenance. Binary size
  regressed to ~19 MB (P8 trim).

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

### 1. Pure-visual QA — SIGNED OFF (user, 2026-07-16)

The functional path was proven on hardware (above); these inherently-visual
items were verified manually in the UI by the user and **signed off for the
`v0.1.0` release**. Retained here as the per-release visual smoke list — run
`target\release\duja.exe` and eyeball:

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

Both gates (hardware + visual) now pass, so `v0.1.0` ships. Add to this list per
release: the tray **"Update available"** item + toast appear when a newer release
exists, and clicking either opens the releases page.

### 2. Known gaps carried forward
- **Binary ~19 MB > 16 MB budget** — P8 must recover it (ADR-0012 ledger; the
  v0.1.0 WinRT toast bindings widened the P5 17.21 MB overage).
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

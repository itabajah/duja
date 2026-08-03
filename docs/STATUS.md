# Duja — Project Status

_Last updated: 2026-08-01 (**the P6 gate has run**. Four adversarial reviews of the
cumulative `v0.1.5..main` diff returned three APPROVE-WITH-FIXES and one BLOCK; six
PRs closed it out. The blocker was real and had been shipping since the macOS DDC
work began: every Apple Silicon DDC/CI request was malformed, so no external monitor
on an M-series Mac could be read or driven. `m6-macos` is tagged. **The release is
deliberately held** — no v0.2.0 until someone has launched `Duja.app` on real
hardware. One post-gate item: a long-standing CI flake in `tests/engine.rs` finally
fired on `main` and is fixed in `#113` — though the first draft of that fix
mis-diagnosed it, and the review that caught it is written up under
[After the gate](#after-the-gate--a-ci-flake-and-a-confident-wrong-diagnosis-113))._

Duja is an ultra-lightweight, cross-platform (Windows/macOS/Linux) system-tray
monitor brightness & display controller in Rust — a no-Electron Twinkle Tray
replacement. This file is the human-readable snapshot of where the build stands.
The authoritative plan is the phase roadmap; architecture decisions live in
[docs/adr/](adr/). For a wide, post-1.0 brainstorm of everything Duja *could*
grow into beyond brightness (the full DDC/CI feature set, OS-level color and
display control, automation, and integrations), see
[docs/future-vision.md](future-vision.md) — a triage menu, not a commitment.

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
| **Multi-monitor & capability fixes** | **`v0.1.2` (Windows)** | ✅ shipped — 6 PRs (5 real-hardware bugs + 1 audit follow-up), test-first, audit + holistic reviewed |
| **Internal-panel fallback fix** | **`v0.1.3` (Windows)** | ✅ shipped — the built-in panel no longer vanishes on a GPU/OEM-driven backlight |
| **Dark rebrand + mirror/software-only** | **`v0.1.4` (Windows)** | ✅ shipped — the dark brand identity plus the two laptop-reported issues (#66, #67) |
| **Sticky software-only fix** | **`v0.1.5` (Windows)** | ✅ shipped — a live monitor no longer sticks as "software-only"; tray Restart. Release verified: 6 assets, SHA256SUMS, minisign, SLSA provenance, `/releases/latest` → v0.1.5 |
| P6 macOS port | **`m6-macos`** (v0.2.0 **held**) | ✅ **gate passed, phase closed** — wave 1 (backends), wave 2 the whole app assembly: hardware wiring (#90), the anchor contract + macOS geometry (#91, ADR-0021), event-loop-first tray construction (#94), the mirror-surface token split (#98), the OS hooks' macOS half (#99), the macOS gamma sink (#100), the tray on macOS (#102), the per-platform gamma captions (#103), packaging — universal `Duja.app` + DMG (#104) — and the built-in panel's geometry (#105). Then **the gate** (#106–#111): see [P6 gate results](#p6-gate-results). **No v0.2.0 until a real Mac has launched `Duja.app`** — a deliberate hold, not a blocker; ADR-0013 additionally keeps macOS DDC experimental until ≥3 independent community confirmations per architecture |
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
[`release.yml`](../.github/workflows/release.yml): an Inno Setup installer, a
portable zip, and — from `v0.2.0` — a macOS universal disk image, all under one
`SHA256SUMS`, each with a minisign signature and a build-provenance attestation.

Health: **1,049 tests on the Windows CI lane plus 11 doctests (1,060 in a local `cargo test --workspace --all-features`), green on 3 OSes** — the
per-OS count differs because the `#![cfg(windows)]` and `#![cfg(unix)]`
integration suites compile out on the other lanes; clippy `-D warnings` clean,
`cargo-deny` clean (advisories/bans/licenses/sources), 5 fuzz targets building
on stable, adversarial gate reviews at **P2, P3, P4, P5, P6** plus a full
post-v0.1.0 **deep review** (14 module reviewers, every non-low finding
adversarially verified) with every confirmed finding fixed test-first.

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
in [debt.md](debt.md); ADRs **0017–0020** record the new contracts. The split and
the E2E smoke have since landed, and the throttle pin moved to `duja-ui` — see
the structural wave below.

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

### v0.1.4 — dark rebrand + the mirror/software-only pair (2026-07-19)

Two issues filed from laptop testing (#66, #67) both traced to **one modelling
gap**: "software-only" was a `DisplayKind` *variant*, conflating a display's
physical provenance with its runtime control mode. Fixed across two
adversarially-reviewed PRs, each of whose reviews caught a real change-blocking
bug:

- **`DisplayKind` is physical-only** (`ExternalDdc`/`InternalPanel`) and the
  runtime verdict rides a separate `software_only` flag threaded through the
  snapshot, the IPC DTOs, and the view-models. The flyout therefore labels a
  display *Internal*/*External*, never "Software", and a software-only display's
  dimming pill is forced on and disabled instead of being freely toggleable
  (turning it off used to strand the slider). The IPC protocol bumped **v1 → v2**
  for the removed variant plus the new required field.
- **Mirrored displays merge into one control** (`bin_support/clone_group.rs`). In
  Windows *Duplicate* mode N panels share one framebuffer, so per-panel rows
  stacked two overlays on the same pixels. A mirror set is now grouped by its
  shared GDI device and driven once, with the group anchor a pure function of the
  member *set* (never enumeration order). Group state is keyed on that anchor,
  and a hot-plug that *moves* the anchor migrates the state across via the stable
  shared GDI device, so the user's level cannot be silently orphaned.

Alongside, the **dark brand identity**: the whirlpool inverted to near-black gems
whose spiral seams glow in the four accent hues, with the exe icon, README hero,
and social card all regenerating from one code source (`dark_whirlpool_rgba`) and
drift-tested. That drift test surfaced a **cross-platform libm determinism** trap
— glibc rounds `exp`/`powf`/`atan2` differently from MSVC on a handful of 262k
pixels — so the assertion pins the integer supersampled alpha bit-for-bit and
allows a bounded RGB delta, which still catches a genuinely stale asset.

### v0.1.5 — the sticky "software-only" probe fix (2026-07-26)

Continued real-world use surfaced the **twin** of a defect class this codebase
had already hardened against once. In v0.1.2 (#59) the *write*-path detector was
fixed so a single retried-but-still-failing DDC read could not make a permanent
binding decision; the *probe* path was the un-fixed twin:

- The runtime "no working hardware" downgrade is **sticky** (`software_forced`).
  It is re-evaluated by a **fresh worker** probe — on sleep/wake, a display-change
  re-enumeration, or an unresponsive→responsive recovery, all of which occur
  routinely in a long session — or by the **poll-driven self-heal**
  (`engine.rs`), which additionally requires the panel to have actually *moved*
  to Duja's last written raw level while the flyout is open and polling, so a
  dead panel merely sitting at that value stays flagged. Neither path is
  guaranteed to fire in a given session. `probe_by_reads` (the
  fallback taken when the capability string cannot be read) turned a **failed**
  brightness read into a *successful* `Ok(caps { hardware_range: false })`, so a
  live external monitor that merely was not answering at probe time — waking from
  DPMS, or a busy bus, exactly the ~60–70% unpaced-read failure rate the P1 spike
  measured — was mislabeled software-only until the process restarted.
- **Brightness is now load-bearing**: a failed brightness read is surfaced as an
  error (inconclusive), never a false "absent". A definitive `hardware_range ==
  false` comes only from a *successful* caps read omitting `0x10` or the
  `ddc_broken` quirk; a genuinely dead panel is still caught by the retried
  first-write check, the intended detector. Contrast stays optional. The residual
  (a DDC panel that answers *nothing at all* stays hardware-backed with an inert
  slider until the user flips the flyout pill) is recorded in
  [debt.md](debt.md) — a rare edge with a user-accessible workaround, traded
  against a false positive that needed a full restart.
- Also a tray **Restart** item: the replacement instance is spawned *before* the
  outgoing one quits (so a failed spawn leaves Duja running rather than gone) and
  waits, bounded, for the single-instance lock; the outgoing instance takes the
  normal clean-quit path so gamma and overlays are restored and state flushed
  before the fresh instance adopts the same levels.

**The lesson worth carrying**: a reliability fact the codebase already encodes
gets silently re-violated whenever a new code path performs the same primitive.
When adding a read/write path, confirm it uses the project's paced/retried
wrapper rather than a raw one-shot call making a decision.

### Structural wave — the tray.rs split + test infra (2026-07-26, post-v0.1.5)

Two PRs on top of v0.1.5, both behaviour-neutral, clearing P6's documented
prerequisite. No release; these land on `main` ahead of the macOS port.

**`#81` — the tray.rs split (2,821 → 612 lines).** The one debt row P6 wave 2 was
explicitly blocked on: [debt.md](debt.md) named it the first task of that
session, because the macOS assembly doubles the `cfg` surface of that file. Seams
that already existed in the file became five modules: `tray/policy.rs` (the pure
decision helpers and most of the tests), `tray/state.rs` (`AppState` + its impl),
`tray/wiring.rs`, `tray/update_flow.rs`, and `tray/hotkey_os.rs` (the
`global-hotkey` registrar and accelerator conversion — the one whose `cfg`
surface the macOS port touches directly); `tray.rs` keeps `run()`,
`ReentrantCell`, `with_app` and `with_app_ref`. The full existing suite passing
unchanged was the contract.

The re-entrancy invariant came out **stronger** than it went in. `ReentrantCell`
is the single serialising access path to `AppState` — it cures the `RefCell`
double-borrow that aborted through Slint's FFI (`0xe06d7363` → `0xc0000409`) in
live QA — but `wiring.rs` initially imported the raw `APP` thread-local for two
setup borrows, widening the invariant's blast radius across a module boundary.
The review caught it; the fix was to make `with_app_ref` a **real function**,
which the `APP` doc comment had already falsely claimed existed. `APP` now
appears in `tray.rs` alone, so no submodule can name it and take a raw borrow.

**`#82` — the throttle pin + an E2E smoke.** A headless end-to-end test now
drives the assembled app (real `Engine`, real IPC, real `PipeServer`, fake
hardware) under the existing required `test` job, and the IPC handler moved
bin→lib so the path serving every `dujactl` request is reachable from tests.

The important part is what the review **rejected**. The throttle-final-value
contract (a leading-edge UI throttle drops a drag's final sample, stranding the
hardware mid-drag — the P4 gate's Finding 1, caught before the first release)
was pinned against a freshly extracted `LevelForwarder`: a stateless three-line
loop. The red-first evidence looked impeccable and protected nothing, because the
bug had been inserted into the one function the test called directly. The
reviewer dug the original defect out of git history (`git show 1902e13^` — the
throttle lived **inside** `AppState::set_user_level`, wrapping the `engine_tx`
send), re-created it *there*, and the whole suite passed. Same at the `duja-ui`
end. The PR would also have deleted the debt row warning about this gap while its
text was still literally true.

The pin now lives at `duja-ui`'s Slint binding
(`slider_drag_burst_emits_the_released_value_last`, verified to fail at both
`duja-ui` re-introduction sites — the shell's `slider-changed` handler and
`FlyoutVm::slider_changed`), and the app-layer gap between UI and engine is documented
as open rather than falsely claimed closed — `AppState` owns a concrete
`tray_icon::TrayIcon` and two live Slint shells, so it cannot be constructed in a
test without a refactor of its own. That refactor was deliberately not smuggled
into a test-infra PR.

**Two rules this cost us, now standing:**

1. **Red-first is necessary but not sufficient — insert the bug where it
   *historically occurred*, not where the test can reach it.** The question is
   "if I re-add this defect at the line where it actually lived, does the suite
   fail?", not "does the test fail when I break the thing it calls?" Find the
   commit; reproduce the defect there.
2. **A false assurance is worse than an open gap.** Deleting a debt row while its
   warning is still true, or adding a comment asserting protection that does not
   exist, converts a tracked gap into a lie in the exact file a maintainer reads
   before re-introducing the bug. Under-promise in comments; never over-promise.

### P6 wave 2 — the macOS foundation, and two defects in a live install (2026-07-30)

Seven PRs, 871 → 951 tests. Three carry the macOS port; two fix things a real
user was hitting; two are hygiene. Every one went through an adversarial review
that found something real, and in four cases the review's finding was worth more
than the PR it reviewed.

**The macOS foundation.** `#90` wires the wave-1 backends into `duja-app` *and*
`dujactl`, so `duja --once` and every `dujactl` verb reach real macOS hardware
instead of stubs. `#91` gives `duja-platform` a macOS `cursor_anchor` and settles
the unit question `#87` deliberately left open, in **[ADR-0021](adr/0021-tray-anchor-coordinate-contract.md)**:
the anchor space stays "the platform's own window-positioning unit" (physical
pixels on Windows, points on macOS) and an `AnchorUnit` plus two derived factors
(`logical_to_anchor`, `anchor_to_physical`, product always `scale`) put the
conversion where a test can see it. `#94` moves tray, hotkey, `AppState` and IPC
construction **inside** the running event loop, which both `tray-icon` and
`global-hotkey` require on macOS; the mechanism is a zero-duration
`slint::Timer::single_shot`, chosen over `invoke_from_event_loop` because that
demands `FnOnce + Send` and the payload is emphatically `!Send`.

What remains for P6 is now the gamma caption, packaging and the gate.
`bin_support/mod.rs` no longer gates `tray` on macOS, and `toast` is
cross-platform with a documented no-op there — `UNUserNotificationCenter` needs a
signed bundle Duja will not have until packaging *and* a runtime authorization
prompt, so the update surfaces through the tray menu item and tooltip, which were
always the guaranteed path.

The un-gating was the smallest piece of the wave, and scouting it rather than
estimating is why that was knowable: flipping the `cfg` and cross-compiling gave
28 errors, 25 of them the two missing crates. Past adding `tray-icon` and
`global-hotkey` for the macOS target, only three things were absent, and two were
gates rather than code — `ipc::TrayBridge` compiled verbatim once its
`cfg(windows)` attributes were widened, exactly as `#90`'s openers did.

What is genuinely macOS-shaped is small and specific:
`setActivationPolicy(.accessory)` so Duja has no Dock tile, which must run **from
inside the running loop** — the intuition says "before any window exists", and that
is exactly wrong here, because winit's `applicationDidFinishLaunching` forces
`Regular` on an unbundled process and would overwrite an early call. It runs
*before* `StartCause::Init`, so `#94`'s loop-time assembly is not merely an
acceptable home for this but the only one that survives. (C6's signed bundle will
let `LSUIElement` say the same thing declaratively, at which point winit stops
overriding and this becomes belt-and-braces.) Then: a 36px status icon for the menu bar's 18pt slot rather than
reusing Windows' 32px and letting `AppKit` resample by 1.125× on a glyph designed
for legibility at tiny sizes; and `menu_on_left_click(false)` so a left click
reaches the flyout instead of dropping the context menu. Windows keeps
`tray-icon`'s default there, unchanged and still hardware-verified.

Narrowing the twelve `cfg_attr(not(windows), allow(dead_code))` module allows to
`not(any(windows, target_os = "macos"))` is the part that paid: it immediately
found one genuinely dead item on macOS — `retain_failed_engagements`, a
Windows-only reconciliation the macOS `restore_all` has no use for — which a
broader allow would have kept hiding. That is the same failure this codebase had
once before, when the P4 gate found `dim_mode = "gamma"` was a silent no-op.

~~Still open before the gate: the per-platform gate on the gamma caption, which is
Windows-specific copy shown on every platform~~ — **closed in `#103`**, below.
ADR-0013 keeps the macOS DDC path labelled experimental until there are ≥3
independent community confirmations per architecture, which no amount of code
closes.

### `#103` — the gamma captions stop being Windows sentences (2026-07-31)

The last code item before packaging, and the only knowingly-wrong string in the
tree: *"Gamma dims to at most 50% on Windows"*, rendered on every platform by a
settings window that `#102` had just made live on macOS.

The interesting decision was **a number, not a bool**. `docs/debt.md` proposed a
`gamma_capped: bool` and, separately, "decide whether the figure should be plumbed
rather than duplicated". Those look like two follow-ups; they are one. A bool
gates the caption and leaves the hardcoded `50` — a second copy of
`duja_dimmer::MIN_ACCEPTED_GAMMA` that no test can catch drifting, in a crate that
depends on neither `duja-dimmer` nor `duja-platform` and so cannot check it.
Plumbing the percentage gates it *for free*, because "this OS imposes no cap" and
"there is nothing to disclose" are the same fact. So `MonitorSection` and
`SettingsMonitorData` gained one `Option<u8>`, sourced from
`dimming::gamma_cap_pct_for_platform()` — the same seam shape as
`plan_for_platform`, so the choice of `min_gamma_factor()` over a literal is pinned
by a test instead of living at an `AppState` call site no test can reach. The copy
became "at most {}% **on this system**": the platform is now carried by whether the
caption appears at all, so naming one in the sentence would be the same mistake in
a new place.

Two things this shape forced that are worth recording.

**A fixture that agrees with the bug proves nothing.** The first version of the
view-model tests passed `Some(50)` — the real Windows figure. Sabotaging
`build_section` to ignore its argument and re-emit a hardcoded `50` left them
**green**, because the wrong answer and the right one coincide. The fixtures are
now `62`, and the same sabotage reds all three. The one test that legitimately
asserts `50` is in `duja-app`, where 50 is a *derivation* from
`min_gamma_factor()` rather than an input. This is the second time in this wave
that fixtures agreeing on an incidental property hid a live mutation.

**One test still cannot see the platform.** `gamma_cap_pct_for_platform` is pinned
per-lane, but on Windows the hardcoded-50 sabotage passes it too — 50 is the right
answer there for the wrong reason. Only the non-Windows lanes red. That is stated
in the test rather than left for a reader to discover.

**The behaviour was pinned by nothing, and the reason given was false.** The first
draft of this PR asserted — in a test comment, in this file, and in the PR body —
that the `gamma-available && gamma-cap-pct > 0` guard on the `Text` element "is not
observable from Rust, and no Slint API exposes a rendered `if` branch". The review
disproved it by writing the test, using an idiom already present 45 lines below in
the same file: `ElementHandle` matched on `accessible_label`, which works because
the compiler's accessibility pass gives every builtin `Text` a default
`accessible-label: text`, and the element walk visits only *instantiated* elements —
so a suppressed `if` branch contributes nothing and an empty result **is** the
observation. Deleting `&& monitor.gamma-cap-pct > 0` from the `.slint` — restoring
the exact defect this PR exists to fix — had left 986/986 green.

That is also where the fixture lesson had *actually* landed rather than been fixed:
moving the pure fixtures to 62 was right, but every fixture in the crate that
reaches the Slint boundary still passed `None`, so the caption `Text` was
instantiated by no test at all — a net coverage regression, since the old
single-term guard was at least satisfied by the two existing binding tests. The
caption now has a binding test that drives both terms of the guard and the `@tr`
interpolation through the real `.slint`, and both sabotages red it.

Separately verified against pinned `i-slint-core` source rather than assumed: `@tr`'s
`{}` is a positional argument substituted by `translations::formatter` (this is the
codebase's first `@tr` with an argument), and `shared_string_from_number(50.0)`
renders `50`, not `50.0`. A placeholder/argument-count mismatch turns out to be a
compile error, so that half was already covered.

**The second caption, and a deferral that did not survive its own argument.** The
first draft of this PR deliberately skipped option (c) of the macOS
silent-gamma-failure debt row — a hazard caption for
`CGSetDisplayTransferByFormula` returning success without applying the ramp — and
justified it: `SetDeviceGammaRamp` is documented with the *same* failure shape, so
a plumbed `gamma_may_silently_fail` would be `true` on both platforms, and the
"per-platform gate" would put a hazard caption in front of every Windows user.

The review disproved it from the quoted source. Microsoft's sentence carries a
conditional the draft dropped: the function *"implements heuristics to check
whether a provided ramp will result in an unreadable screen. **If a ramp violates
those heuristics**, then the function fails silently"* — and the heuristic is
quantified on the DXGI page as *"any entry in the ramp must be within 32768 of the
identity value"*. `MIN_ACCEPTED_GAMMA` is **derived from that bound and pinned at
it** by `win/gamma.rs`'s own tests, and the planner substitutes an overlay below
it, so every ramp Duja sends complies by construction. `MIN_ACCEPTED_GAMMA`'s doc
had already said exactly this — *"staying at or above it is the only reliable
protection"* — and the draft contradicted a sentence already in the tree without
noticing.

So the two platforms differ in **mechanism**, not likelihood: Windows states a rule
and Duja satisfies it; macOS fails on *valid* triples, with no rule to satisfy and
no readback that detects it. That asymmetry is precisely what makes the disclosure
macOS-only — which is to say the correction did not merely reword the deferral, it
removed the reason for it. So (c) landed here: a `duja_dimmer::gamma_is_advisory()`
and a second caption, plumbed beside the cap through a `GammaLimits` struct. The
struct exists because `advisory` would otherwise have sat next to `gamma_allowed`
as a second `bool` on the same call, where swapping them compiles, suppresses the
HDR guard, and shows the hazard to everyone — the `#98` lesson, applied before it
had a chance to bite.

The two captions are tested against *each other's* fixture, so deriving either
guard from the other flag reds. Windows shows the cap and no hazard; macOS shows
the hazard and no cap; the HDR guard suppresses both.

The residual on the Windows side is unchanged and stays where it was — a driver or
GPU with a tighter, undocumented rule is a hardware unknown, not a mode the
platform has, and `gamma_is_advisory`'s doc says that rather than claiming Windows
is safe. Options (a) and (b) of the same row — plan the overlay instead of the ramp
on macOS, or detect the auto-brightness setting — remain open and still need a Mac.

The `#90` row's prediction that the sink would need "a `ScreenStateGuard` twin and
a crash-marker policy — a design decision, not a port" is **drained, and it was
wrong in the useful direction**: it needed neither, and what it actually needed was
the correlation half, which is a port. That row now says so.

`#90`'s **standing rule** — `BoundsMap::device_for` must not be fed into
`clone_group` on macOS until the surface token maps through
`CGDisplayMirrorsDisplay` — is **discharged**, and the fix arrived larger than the
rule anticipated. Stamping the mirror-set master does give `clone_group` the
shared-framebuffer identity it needs, but it cannot also be the value gamma
addresses: a clone's token is *another display's* id, and on the commonest Mac
mirror layout — a MacBook mirroring its built-in screen to a projector — that
master is the built-in panel, which `enumerate` filters out with
`CGDisplayIsBuiltin`. Driving `CGSetDisplayTransferByFormula` through it would dim
the laptop screen instead of the monitor whose slider moved. So the one opaque
token is now **two** named fields on `DisplayGeom` (`gamma_token`,
`surface_token`) behind two `BoundsMap` accessors; Windows puts the same GDI
device name in both, because there one device *is* the clone set.

**Two defects found by reading a real install.** Neither was in new code, and
neither was reachable from any test:

- **Windows caps gamma dimming at 50 %** (`#96`). The reporting user's log held
  **349** `SetDeviceGammaRamp failed` warnings over 15 days with
  `dim_mode = "gamma"`, each reading *"failed: The operation completed
  successfully"* — `windows-rs` builds that error from `GetLastError`, and the API
  returns `FALSE` without setting one. A sweep on their display **bracketed** the
  boundary (every step down to 0.50 accepted, every step from 0.45 refused), and
  Microsoft's documented rule pins the value the measurement alone cannot: *"any
  entry in the ramp must be within 32768 of the identity value"*. The integer ramp
  at `f = 0.50` deviates by exactly **32767**, one unit inside the limit under
  either a `<` or a `<=` reading — which is why 0.5 is the smallest `f32` that
  satisfies every reading, and why **`GAMMA_FLOOR = 0.3` is unreachable on
  Windows**. Worse, the app recorded the display as engaged anyway, so the planner
  never substituted an overlay and the slider did nothing below the transition.
  Now the planner asks `duja_dimmer::min_gamma_factor()` and plans an overlay
  below it, and `#96` also removed a **bright flash** the fix would otherwise have
  introduced: crossing the boundary mid-drag used to destroy the overlay to
  completion before engaging the ramp, so the batch is now ordered engage → overlay
  diff → restore. Log volume went from 349 warnings to one per transition. The
  level the overlay delivers is identical; its coverage is not (an overlay cannot
  cover exclusive-fullscreen, the secure desktop, or later-created topmost
  windows), and the settings window says so. **The user chose to keep the mode with
  the cap disclosed** rather than retire it. Two routes to "nothing dims" remain
  open and are tracked: a refusal for any reason *other* than a sub-minimum factor
  still leaves no overlay, and Microsoft documents a violating ramp as failing
  *silently by returning `TRUE`* — verify-by-readback (ADR-0002's idiom for lying
  hardware) is the real cure.
- **`dujactl doctor --report` did not exist** (`#95`). `CONTRIBUTING.md` and the
  monitor-quirk issue template both instruct users to run it; `doctor` was routed
  through `end()`, which rejects trailing arguments, so the one command asked of
  bug reporters exited with a usage error. Fixing it exposed that the template
  also promised "probed capabilities" the command never gathered, so `--report`
  now probes: raw MCCS capability string, feature set, live range,
  `hardware_range`, allowed inputs, and the probe **error** when there is one
  (previously "enumerates but DDC is dead" looked identical to a healthy monitor).

**A documentation sweep found four more claims no code backed**, all fixed in
`#95`: `bug.yml` pointed at a "tray menu → Open log folder" item and an "About
dialog", neither of which exists (and the GUI cannot display its own version at
all — `grep -i version` across every `.slint` finds nothing); `README.md` and
`docs/qa-checklist.md` claimed `dujactl doctor` checks Linux i2c permissions,
making that qa item a release gate that could never pass; and `paths.rs`'s own
module doc gave the wrong log directory (`%LOCALAPPDATA%` where the real path is
roaming `%APPDATA%`), which is now pinned by a test because `bug.yml` quotes it to
strangers.

**Hygiene.** `#92` brings `dujactl`'s discovery in line with the app's, fixing a
Windows defect where a WMI-drivable built-in panel was listed twice and, if
serial-bearing, collided into `-slot0`/`-slot1` so `set` on one of them failed
outright while the other wrote VCP `0x10` over eDP. `#93` replaces the live
overlay test's `FindWindowExW` cursor walk — whose `hwndChildAfter` is Z-order
relative, so a concurrent overlay elsewhere could make it revisit a window *or*
truncate when the cursor window died — with an `EnumWindows` snapshot, plus a
serialization gate so `cargo test` (the workflow `CONTRIBUTING.md` lists first)
passes as well as nextest.

**`#94` was verified on real hardware**, since no test covers the assembled tray.
The load-bearing check is a `dujactl list` **served over IPC** — `--verbose` says
so, and only the app's table carries the `mode` column, whereas a plain `list`
falls back to the direct backend and would exit 0 with no app running at all.
Because `ipc::start` is the *last* statement of `assemble_with_loop_running`, an
IPC-served answer implies `build_tray`, `init_hotkeys` and the `AppState` literal
all ran before it. Window enumeration then showed `tray_icon_app`,
`global_hotkey_app`, `DujaPlatformEvents`, a hidden `Duja Settings` and a visible
`Duja Brightness` anchored bottom-right, and the continuum round-tripped correctly
(user 88 → hardware 80 exactly, user 95 → hardware 92 by rounding, at
`min_perceived_pct = 40`). No new warnings or errors in the log (`#94` adds one
`info` line by design), `private = 5.2 MB`, `cpu = 0.27 s`.

**One thing the smoke also exposed, the hard way.** Running the new
`dujactl doctor --report` repeatedly *while the tray app was running* left the
display marked `software_only` in the app, even though hardware writes kept
working throughout. `--report` opens its own controller and probes over DDC by
design, so two processes were driving one monitor's I2C; a timeout on the app's
side is enough to trip the no-hardware detector that `#59` and `#78` hardened. The
CLI's own runs were clean, which is why the PR that added probing did not catch it
— it checked the wrong side. Tracked in [debt.md](debt.md); the fix direction is
for `--report` to prefer IPC, or to say plainly that it is about to contend.

### `#104` — macOS packaging: a universal `Duja.app`, in an image (2026-07-31)

C6. `xtask dist` was hard-wired to Windows — a staging directory and a PowerShell
`Compress-Archive`. It now picks a target from the host and grew a macOS branch:
`lipo` the two thin release builds into universal binaries, assemble `Duja.app`
around them, seal it with `codesign`, and wrap it in a drag-to-install `.dmg`. The
release workflow gained a `macos` job that runs first and hands its image to the
Windows job, so the release keeps **one** `SHA256SUMS`, **one** minisign pass, one
provenance attestation and one Release rather than two jobs racing to create the
same one.

**The split that made this testable at all.** None of `lipo`, `codesign` or
`hdiutil` exists off macOS, and Duja has no Mac. The temptation is to write the
whole branch behind `cfg(target_os = "macos")` and call it unverifiable — which is
the exact shape `#103` had just been burned by: a platform fact inside a
`cfg`-gated module is unreachable by every test *and* by every lane's clippy. So
the module boundary follows what is actually platform-bound rather than what is
platform-*themed*. Every **decision** — the `Info.plist`, the bundle layout, the
artifact names, the accepted version alphabet, the host→target mapping — is pure
code in `xtask`'s new `bundle` and `version` modules and is unit-tested on all
three lanes, Windows and Linux included. What remains in `dist.rs` is filesystem
plumbing plus four `Command` invocations — not `cfg` blocks, so they still compile
into every lane's clippy run. This is the same division `duja-platform`'s
`autostart::plist` already made for the `LaunchAgent` document, and it is the
reason a Windows box could develop macOS packaging with tests that bite.

**Two constants that live in three files, pinned by reading the other two.**
`CFBundleIdentifier` must equal the `launchd` job label `duja-platform` registers
for launch-at-login, or one program carries two identities; `LSMinimumSystemVersion`
must equal the `MACOSX_DEPLOYMENT_TARGET` the workflow compiles *both* slices
against, or the bundle advertises a floor nothing was built for. Neither coupling
is expressible in the type system across a crate and a YAML file, so the tests read
the other files: one parses `LABEL: &str = "…"` out of `autostart/plist.rs`, the
other pulls `MACOSX_DEPLOYMENT_TARGET` out of `release.yml`. Both were written
red — the deployment-target test failed until the workflow line existed.

**What the tests cannot reach, CI checks on the shipped bytes.** The `macos` job's
*Verify the bundle* step runs `plutil -lint` on the written plist, asserts
`LSUIElement` is actually `true` in it, proves both binaries carry an arm64 **and**
an x86_64 slice, reads `minos` back out of `LC_BUILD_VERSION` for each slice and
compares it to the advertised floor, validates the code signature, and mounts the
image. That is the half a unit test cannot see, checked where it exists.

**Signing is the last mutation, and that is not an arrangement choice.** `lipo`
rewrites the Mach-O and the bundle seal covers the `Info.plist` and everything under
`Contents`, so the order fuse → assemble → sign → image is forced. Nested code
(`dujactl`) is signed before the bundle that encloses it, because sealing the bundle
records the signatures it finds inside. The default identity is ad-hoc (`-`): enough
for macOS to *execute* the binary — Apple Silicon refuses an unsigned one outright —
but not notarized, so Gatekeeper blocks the first open of a downloaded copy and the
user must allow it in System Settings → Privacy & Security → **Open Anyway**. That
is the exact macOS twin of the SmartScreen prompt on the unsigned Windows installer,
and `SECURITY.md` now says both. `--sign <identity>` takes a real Developer ID and the
hardened runtime is applied on **both** paths, so turning signing on is a repo
variable, not a code change — the same posture as the inert Azure Trusted Signing
block.

**`LSUIElement` closes a loop from `#102`.** `become_accessory_app` sets the
activation policy imperatively because winit stops overriding it only for a bundled
app; the bundle now declares the same thing, so for an installed copy the call is
belt-and-braces. It stays for the reason that is actually checkable: a `cargo run`
or portable copy has no `Info.plist` at all. Not for the reason it is tempting to
give — a `launchd`-exec'd copy *inside* the bundle is still bundled, because
`NSBundle` resolves upward from the executable path, which is the same question
winit's `is_bundled` branch asks.

**One tightening that was not packaging.** `--version` used to be interpolated raw
into a single-quoted PowerShell literal; it now reaches the same place through a
`Version` newtype whose alphabet is the set of characters inert in *all* the
contexts it lands in — XML text, a volume name, a file name, a shell literal. The
macOS branch is what made that necessary (an `Info.plist` is a document, not a
string), but the Windows path was the one that was already exposed.

Two things ship knowingly incomplete and are recorded rather than papered over: the
bundle has **no icon** (the art is drawn in code and no raster asset exists in the
tree, so an `.icns` needs a PNG encoder, a Slint dependency in the build tool, or a
Mac-only pipeline — none of which belongs in a packaging PR), and the packaging path
has **no PR-time CI coverage** (its only automated exercise is the release workflow's
`workflow_dispatch` dry run). Both are in [debt.md](debt.md) with the option they
should be fixed by.

#### What the review changed

The adversarial pass blocked the first version, and three of its findings changed
the code rather than the prose.

**A pin between two string literals is not a pin.** The deployment-target test
compares a Rust constant to a value in `release.yml` — and *neither of them is what
reaches the compiler*. The recipe this PR itself documented for packaging locally
omitted `MACOSX_DEPLOYMENT_TARGET` entirely, so following it produced an `x86_64`
slice at rustc's default inside a bundle advertising 11.0: exactly the drift the
test exists to prevent, invisible to it. The fix moves the invariant onto the
artifact. `xtask` now reads the Mach-O header itself — a new `macho` module, ~180
lines of bounds-checked integer reads — and `Verified::checked` refuses to sign a
bundle whose slices are not all present and all built for the advertised floor.
Parsing it in Rust rather than shelling to `otool` is the point: the check then
runs **identically on a maintainer's Mac and in CI**, and is unit-testable on every
lane against synthetic fat binaries. The workflow keeps its `otool` check as an
*independent* implementation, since a second opinion is the only thing that catches
the first one being wrong.

**A justification that was simply false.** The 11.0 floor was defended with "a
universal binary cannot honestly advertise a release that predates Apple Silicon".
That is not how universal binaries work: the deployment target is per-slice, Launch
Services gates on `LSMinimumSystemVersion` alone, and an `x86_64` slice built for
10.13 inside a 10.13 bundle runs on a 10.13 Intel Mac with the `arm64` slice's floor
never consulted. 11.0 survives as a **support decision** — the lowest floor both
slices share from one setting, and the lowest one that does not assert support for
releases nothing tests — with the real cost (Macs older than ~2013–2014) and the
real remedy (a per-arch deployment target) written into `debt.md` instead of a
wrong claim written into the constant.

**Shipping a DMG created a new way to lose the login item.** The `LaunchAgent`
plist records a *path*, and a disk image invites one specific sequence: mount,
run `Duja.app` from the volume, enable "start with the system", eject. The plist
then names `/Volumes/…` forever, `launchd` fails to exec it at every login, and
`is_enabled`'s presence policy keeps reporting it as **on**. A setting that says
yes and does nothing, permanently, is the "vanished" failure the degrade rule
forbids — and it was newly reachable because of this PR. `set_enabled(true)` now
refuses when the executable is on a mounted volume, with a message naming
`/Applications`; the app's existing `apply_autostart` re-reads the real state, so
the toggle springs back rather than lying. The rule is a pure path predicate in its
own module, tested on all three lanes; that `set_enabled` consults it can only be
checked on the macOS lane, and the test says so.

Two smaller corrections worth recording because they were both *confident and
wrong*: `hdiutil verify` only checks the image's stored checksum and never
attaches, so three sentences claiming CI proved the image "mounts" were false —
the step now actually mounts it, checks `Duja.app` and the `Applications` symlink
are there, and detaches. And the one user-facing instruction the PR added, "first
launch needs right-click → Open", stopped being true in macOS 15: Sequoia removed
that Gatekeeper override, and the user must go to System Settings → Privacy &
Security → Open Anyway. It was wrong everywhere it appeared, on every macOS this
release targets.

Two tests were deleted for being tautologies (`assert_eq!(BINARIES, [MAIN, HELPER])`
where `BINARIES` is *defined* as that), and the coupling they pretended to check is
now real: the executable names are read out of each crate's `[[bin]]` section, the
architecture list out of the workflow's `cargo build --target` lines, and both
cross-file readers now assert that **every** occurrence agrees rather than the
first.

#### And what the second review changed

Round 2 blocked it again, and the best finding was that the mounted-volume guard
**does not fire in the scenario it was written for**. A downloaded `.dmg` is
quarantined, and macOS will not run a quarantined app in place: **App
Translocation** mounts a throwaway read-only mirror and runs it from
`/private/var/folders/…/AppTranslocation/…`. So `current_exe()` never starts with
`/Volumes/`, the guard passes, the plist is written — and that mount is destroyed
when the app *quits*, which is sooner than ejecting. The fix catches the marker as
a path component too, and the test for it is the one that would have failed. This
is the second time in this PR that a guard was written against the case that was
easy to imagine rather than the case that actually happens.

Second: the Mach-O fixtures wrote `sdk == minos`, so **reading `sdk` instead of
`minos` left all 31 tests green** — and that specific mistake would compare the
build machine's SDK against the advertised floor and refuse to package *every*
release. The fixtures now use a distinct SDK, and the two offsets are pinned by a
test that names why. While there, the parser stopped taking the first
`LC_BUILD_VERSION` it finds: a zippered binary carries a Mac Catalyst one too, so
it now selects `PLATFORM_MACOS`.

What could not be fixed is recorded rather than glossed: the fixtures import the
same constants the parser compares against, so **no test constrains the constants
themselves** — a wrong one would block every release while the suite stayed green.
Every value was read off Apple's `cctools` and dyld and is cited at its definition,
and the dry run feeds the parser a real `lipo` output; the debt row names the fix
(capture a real fat header as a byte fixture) and why it needs a Mac.

Three more corrections of confident prose: `hdiutil create -srcfolder` does **not**
default to APFS — it inherits the source volume's filesystem, which is the actual
reason to name `HFS+` explicitly; the `Verified` token makes a *signed-but-unchecked*
artifact uncompilable, not every omission (deleting the check and the seal together
still compiles, and `dead_code` under CI's clippy is what catches that); and the
"re-run recovers the tag" note gave the wrong reason, since with `needs:` the
publishing job never ran and there is no Release to update.

### `#106` — every Apple Silicon DDC request was malformed (2026-07-31)

Found by the **P6 phase-gate review**, and the reason `#105` was not in fact the
last P6 code item. On an M-series Mac no external monitor could be read or driven
over DDC: `frame_request`'s Apple Silicon arm emitted a spurious byte and an
off-by-one length prefix, and used the wrong checksum seed for a Get.

    Get VCP 0x10   was  83 02 01 10 AF      now  82 01 10 FD
    Set VCP 0x10   was  85 04 03 10 hi lo   now  84 03 10 hi lo

`0x02` is *VCP Feature Reply*, a display→host op-code, so the frames were
semantically garbage rather than corrupt — the old checksum was self-consistent,
which is part of why nothing rejected them loudly.

One root cause. The arm was transliterated from MonitorControl's
`packet = [0x80 | (send.count + 1), UInt8(send.count)] + send`, whose `send`
**excludes** the DDC op-code — so that second element *is* the op-code, and only
looks like a second length field because a Get sends 1 byte (op `0x01`) and a Set
sends 3 (op `0x03`). Duja read it as a length and prepended its own op-code as
well. The same misreading shifted the seed branch, which the reference keys on
`send.count == 1`, i.e. duja's Get.

Cross-checked against four implementations before a byte changed. They do not all
agree, and the write-up says so: **fastfetch** is the most useful (it emits both
arms from one file), MonitorControl and m1ddc agree with it but **share an
author** so they are one source rather than two, and `ddc-macos` genuinely
dissents on the Get checksum. Duja follows the three field-dominant ones.

Why nothing caught it: `FakeI2cBus` read the length at index 1 and the body at
`2..`, documented as true for both framings. That was true only of the *malformed*
frame, whose spurious second byte happened to equal the body length — so the fake
decoded the bug into the right answer and every round trip closed. It now keys off
`DdcWire`, and validates the request checksum as a real display would, which the
gate's review of this PR showed was the remaining half: before that, inverting the
seed reddened only the two exact-byte unit tests and **zero** transport tests.
Both properties were verified by restoring the defect under the corrected fake.

Note this is a reading of the wire, not an observation — Duja has still never run
against Apple Silicon hardware, so the whole `duja-ddc` `mac/` row in
[debt.md](debt.md) stands unchanged.

### `#105` — the built-in panel gets a position, so it can be dimmed (2026-07-31)

The last P6 code item. A macOS built-in panel reached the app as `(id, None, None,
None)` — no bounds, no gamma token, no surface token — the same shape as a Windows
WMI panel. `dimming::plan` emits a `DimCommand` only for a display it can place, so
the panel got none: below the backlight's floor there was nothing to dim it with,
on the one screen a laptop user actually looks at.

On Windows that shape is honest. WMI exposes no rectangle for the panel it drives,
so `None` means what it says. On macOS it was never true: a `DisplayServices` panel
is an ordinary CoreGraphics display, and `CGDisplayBounds` answers for it exactly
as it does for a monitor.

The fix is where the debt row asked for it — on `duja-panel`'s own API. `enumerate`
now reports a `PanelGeometry` beside each panel, `Some` on macOS and `None` on
Windows, and the app folds it into the same `DisplayGeom` a DDC display produces.
No `cfg` in the app, no display FFI in the app binary, and nothing re-parsing
`instance_name`, which is documented opaque and would have been the tempting
shortcut.

**Bounds alone would have been a regression.** Give the panel a rectangle and
nothing else, and a `MacBook` mirroring its screen to a projector has the panel (a
`None`-token singleton) and the monitor at *identical* bounds in two different
groups — two overlay windows stacked on one framebuffer. That is `#66`, arrived at
from the opposite direction, and it would have shipped as a fix. So all three
values move together: bounds, the surface token that groups the mirror set, and the
gamma token that addresses the panel itself.

That put the same rule in two crates, which is the arrangement that invites drift:
`duja-ddc` computes the external clone's token, `duja-panel` the master's, and the
app merges them by comparing the two strings. A rule two crates must agree on, kept
in two copies, agrees until it does not — so `CGDisplayMirrorsDisplay`→surface-token
and `CGRect`→`DisplayBounds` both moved into a new pure `duja_core::macos`. Both
stay FFI-free and tested on every lane, as they were before; there is now one of
each rather than two.

Sixteen tests on the Windows lane and a seventeenth on the macOS one (the
`CGRect` flattening can only compile there), red-first where a red was available:
the app-side fold was extracted with its shipped all-`None` body first and the new test failed against it
(`left: None`, `right: Some(DisplayBounds { .. })`), then each token assertion was
proven load-bearing by mutation — crossing the two reds one, reading one twice reds
the other.

Three of the sixteen are **not** evidence of anything and say so in their own doc
comments, per the precedent `#92` set: `a_macos_panel_entry_reports_its_bounds_and_both_tokens`,
`an_internal_panel_with_bounds_gets_an_overlay_command` and
`a_macos_panel_and_the_external_mirroring_it_form_one_group` all pass against the
code that shipped before this change. `BoundsMap`, the planner and `group_clones`
needed no edit — the planner never had a kind-based exclusion to remove, and
`group_clones` does not consult `kind` at all. They are guards against a future
panel-specific special case, not demonstrations of the fix, which lives one layer
up in `backend.rs`.

What no test can reach is that macOS *reports* any of this: the two new
CoreGraphics calls have never run, like every other macOS path here. One hazard
they surface *is* handled at the source — `CGDisplayBounds` answers `CGRectNull`
for a display it considers invalid, and this backend reads the *online* list, which
can hold a built-in that is not the active drawable, so `panel_geometry` refuses any
rect that is not finite or encloses no area rather than planning an overlay from it.
Both arms earn their place: `CGRectInfinite`'s extents convert to `u32::MAX` and
would sail past an emptiness check alone.
The rest is in the debt row, including the one the review found: because the anchor
of a group is its lowest id string and an Apple panel's `APP-…` sorts ahead of
nearly every monitor id, a mirrored set's single overlay is now usually **placed**
from the panel's rect. Grouping was documented as unable to mis-address anything;
placement is a third consumer, and that argument never covered it.


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

**`CFRunLoopStop` is not latched** (found 2026-07-26, fixed in the pump). It
no-ops when the loop's `_currentMode` is `NULL`, i.e. when the loop is not
*currently running* — the request is dropped, not remembered. `Pump::spawn`
returns as soon as the pump thread pushes into a **buffered** `sync_channel(1)`,
which happens several statements before it enters `CFRunLoopRun`, so an owner
that shuts down promptly lands in exactly that window: the stop does nothing, the
liveness source holds the loop open by construction, and the unbounded `join()`
never returns. The Windows backend has no equivalent race because `PostMessageW`
queues into a message queue that already exists when `spawn` returns.

The cure is to signal a `CFRunLoopSource` instead — *that* is latched, as a flag
on the source object honoured on the loop's first pass — and let its callback
call `CFRunLoopStop` from inside the loop. The keep-alive and stop sources are
now one source: unsignalled it plays its old liveness role, signalled it ends the
loop.

**The process lesson is the sharper one.** This shipped as a *documented flake*:
`drop_shuts_down_without_hanging` was written off as virtualized-runner noise,
with standing advice to rerun the job and not suspect your diff. Three CI runs
(`29649998260`, `29824307450`, `29825848280`) were cancelled by hand with
`test (macos-latest)` as the only genuine failure, one of them ending with the
runner reaping an orphaned `duja_platform` test binary. A hang has no assertion
text, so it produced no evidence trail and nobody looked twice for five days. The
suite now carries a nextest `slow-timeout`/`terminate-after` guard
([`.config/nextest.toml`](../.config/nextest.toml)) so the next wedge — in any
crate, on any OS — is reported as a *failed test with a name* rather than a job
someone has to cancel. **A flaky test is a finding, not noise**; that rule was
already written down here, and it was not applied.

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
- **Docs & brand.** A premium README (hero rendered from the dark whirlpool
  mark — *duja* is Arabic for darkness, and the brand leans into it: near-black
  gems with the swirl glowing in the four accent hues — badges, install/verify
  sections), a social-preview card, and the threat-model/SmartScreen/verification
  notes in [SECURITY.md](../SECURITY.md). The mark, exe icon, and banners all
  regenerate from `dark_whirlpool_rgba` via `gen_exe_icon` +
  `scripts/gen-social-preview.py`, drift-tested in `tests/exe_icon.rs`.
- **Not signed.** No Authenticode certificate yet, so SmartScreen warns on first
  run; authenticity is via the checksums + minisign key + provenance. Binary size
  regressed to ~19 MB (P8 trim).

## P6 gate results

Four adversarial reviewers over the cumulative `v0.1.5..main` diff (23 commits, 96
files, +17,171/−3,555), split as: macOS backends + pure rules; app/tray/platform/
UI/CLI; packaging/CI/release docs; and a holistic cross-crate + rubric pass.
**Three returned APPROVE-WITH-FIXES, one BLOCK.** Six PRs closed it out (#106–#111).

### The blocker

**Every Apple Silicon DDC/CI request was malformed** (#106), and had been since the
macOS DDC work began. `frame_request` emitted a spurious byte, an off-by-one length
prefix, and the wrong checksum seed on a Get:

    Get VCP 0x10   was  83 02 01 10 AF      correct  82 01 10 FD
    Set VCP 0x10   was  85 04 03 10 hi lo   correct  84 03 10 hi lo

`0x02` is *VCP Feature Reply*, a display→host op-code. One root cause: the arm was
transliterated from MonitorControl's `[0x80 | (send.count + 1), UInt8(send.count)] +
send`, whose `send` **excludes** the DDC op-code — so that second element *is* the
op-code, and only looks like a length field because a Get sends 1 byte (op `0x01`)
and a Set sends 3 (op `0x03`).

**Why nothing caught it.** `FakeI2cBus` read the length at index 1 and the body at
`2..`, documented as true for both framings. That was true only of the *malformed*
frame, whose spurious second byte happened to equal the body length — so the fake
decoded the bug into the right answer and every round trip closed. The exact-byte
"regression corpus" then pinned the broken bytes as the baseline, which made the
tests worse than absent: a correct fix reds the suite and looks like the regression.

The lesson generalises past this defect, and `debt.md`'s `duja-ddc mac/` row now
carries it: *"the pure packet codec is fully CI-verified"* was true and worthless.
Purity buys host-testability, not conformance to a wire someone else defined. Only a
**primary source** does that — and you need more than one, because they disagree.
Four macOS DDC implementations were consulted; fastfetch is the most useful (it
emits both arms from one file), MonitorControl and m1ddc **share an author** and are
one source rather than two, and `ddc-macos` dissents on the Get checksum.

### The other findings that changed code

- **A stale `Panicked` ack retired the fresh worker** (#107) — **Windows-affecting,
  shipping today.** It was the only worker-exit ack of three carrying no
  `generation`, while both siblings gate on one *and* have a regression test. A
  driver panic racing a replug greyed the healthy replacement and spent one of the
  two stuck marks that abandon a display for the session.
- **Gamma was never re-asserted after wake** (#109). ADR-0003 offers gamma "only
  where verified safe (Windows SDR, macOS **with re-apply-on-wake**, wlroots)"; the
  macOS sink shipped without it. `engage_phase` diffs against its own record, which
  cannot observe the OS discarding a ramp — and a gamma-mode display has
  `overlay_alpha == 0` by construction, so nothing else dimmed it. Not macOS-only:
  Windows self-healed only *incidentally*, when the event also removed the display.

### The near-regression

**#108 was blocked by its own review, correctly.** The macOS `enumerate_displays`
skips a display with an unreadable EDID *before* consuming its I2C service, which
reads as a queue desynchronisation. It is not: an unreadable EDID means a **virtual**
display (Sidecar/AirPlay/DisplayLink), which has no `DCPAVServiceProxy` and never had
a slot to spend. The "fix" would have handed a real monitor's service to a display
that cannot use it and then released it — losing DDC control of that monitor
entirely. What landed instead is the invariant at the call site plus a debt row
naming the wrong fix, so it is not attempted a third time.

### Documentation the phase falsified

Six claims (#110), the sharpest being that `sha256sum` — the documented verification
command in README, `SECURITY.md` and the release checklist — **does not exist on a
stock macOS**, on the one platform where the binary carries no publisher identity.
Also: the release gate does not run before the macOS job builds and signs; the
attestation covers three artifacts, not two; and the support matrix still called the
macOS tray "planned".

### Rubric

Clean on: typed errors with no `unwrap`/`expect`/`panic` outside tests; every
`#[allow]` carrying a `// RATIONALE:`; all new `unsafe` behind a `// SAFETY:`;
`duja-core`/`ipc`/`dujactl` genuinely unsafe-free under `forbid`; no new idle
wakeups; CHANGELOG/debt/ADR discipline. Two deviations recorded rather than
papered over: `duja-panel` keeps FFI in its backend modules rather than a `sys`
submodule (its own long-standing convention, and restructuring untested COM code
blind is what `debt.md` row 27 warns against), and `duja-platform` established
`platform` as a second name for the same role.

### Deliberately still open

The remaining macOS items are hardware-blind, and writing blind FFI to close them
is the trade this project has repeatedly declined: the built-in panel's fallback
carrier, the `mac/mod.rs` token-assembly hoist (a swap there is proven undetectable
— it leaves the suite green), mixed-DPI flyout placement (whose "needs hardware"
deferral the gate *disproved* from pinned `dpi` source, so it is now a real
candidate), and the unix-socket hardening that became live on macOS. All carry rows.

### After the gate — a CI flake, and a confident wrong diagnosis (`#113`)

The push build for `#112` — the docs-only commit that closed this phase — went red
on `test (windows-latest)`: `worker_panic_does_not_kill_engine` missed a 2 s wait,
taking 2.850 s. Re-running the identical commit `f96e4bd` turned it green at
0.069 s. That is the **fourth** time this test has gone red on CI; three of the
four were re-run unchanged and all three passed. The fourth (run 30504679809,
2026-07-30, 2.804 s) was never re-run — which is worth stating, because it is the
only one of the four whose outcome could have contradicted the environmental
reading, and it went unrecorded until review counted the runs.

What settled it was a test with no panic in it: in the same red job,
`loop_time_assembly`'s zero-duration single-shot went 0.363 s → **4.308 s** →
0.777 s on the re-run, while the median test ran at 1.04x its green time. A stall
that hits an unrelated event-loop test twelvefold is environmental.

The first draft of `#113` said otherwise, and it is worth recording why. It
diagnosed the failure as the panic runtime symbolizing a backtrace inside the
worker's `catch_unwind` ahead of the ack — a real cost, correctly located, and
removed by that PR. But it then asserted `RUST_BACKTRACE=1` was "the single
environmental difference" between 3,014 passing local runs and the red CI run.
[`docs/debt.md`](debt.md)'s own row — written by the same author two commits
earlier — already recorded that **960 of those 3,014 runs had `RUST_BACKTRACE=1`
and passed**, and that the symbolization measures 30–200 ms against multi-second
failures. Five green Windows runs paid the same cost at 0.057–0.184 s. The review
blocked it on exactly that arithmetic.

So the fix shipped as two separable things, which is how the debt row had
prescribed it all along: `LIVENESS_BUDGET` (10 s) for **every** positive wait in
`tests/engine.rs` — the half that actually addresses a runner stall — and the
panic-hook mute as a ~50 ms cleanup that also de-noises the log. The **14**
negative waits keep their own short literals, since an assertion of absence
elapses in full every run. The hook keeps each muted panic's thread, message and
location, because that header is what made the cost measurable in the first place.

The generalisable lesson, and the reason this is in STATUS rather than only in the
ledger: **a mechanism you can measure is not thereby the cause.** The measurement
was sound, the location was right, the fix was worth making — and the causal claim
was off by a factor of ~15–50, refuted by the author's own prior notes. The free
experiment that settles environment-vs-code was a re-run button, and it was not
pressed until review demanded it.

The corrected version then failed a second review, and the second lesson is
narrower but cheaper to act on: **quantifiers were asserted without counting.**
"the third red run" (four), "the one negative wait" (fourteen), "all 53 positive
waits" (53 of ~76), "reached from 14 tests" (13 call sites across 10), and a
`logging.rs` hazard called live when the two tests are in different binaries. Two
of those a `grep -c` settles outright; the red-run count needed the GitHub API,
and the binary claim needed the PDB sizes. A sixth, the PDB figure itself, was
made *less* accurate by a "correction" that silently compared MiB against MB.
Worth noting how the last one arrived: `grep -c` is what produced "14 tests", by
counting the function's own definition line — the checking tool used carelessly is
its own failure mode.
The counting error was not cosmetic: the omitted red run is the only one of the
four that was never re-run, so the one data point that could have falsified the
environmental reading was also the one missing from the record. Both rounds share
a root — writing the sentence before doing the arithmetic that would check it.

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
  **P7 Linux** (VM/WSL-assisted; the GNOME Wayland dimming spike is now
  verification of ADR-0011's runtime probe rather than an input to it, so it
  follows the decision instead of preceding it),
  **P8 hardening** → fuzz burn-in, 72 h soak, packaging, size trims, 1.0.

**P7 waves.** The ADRs and commit messages refer to these by number, so they are
written down here rather than left implicit:

| wave | scope |
|---|---|
| 0 | unix IPC + lock-directory hardening (shared with macOS) — `#114` |
| 1 | the two ADRs reserved for P7 (0010 tray, 0011 dimming), plus 0022, which the tray decision and two other P7 features forced |
| 2 | DRM/sysfs enumeration + EDID identity, `/dev/i2c` bus, backlight (logind primary, sysfs fallback) |
| 3 | event pump (`NETLINK_KOBJECT_UEVENT` direct, no libudev) + autostart, desktop, geometry |
| 4 | software dimming: X11 overlay + XRandR gamma, Wayland layer-shell + `wlr-gamma-control`, and the ADR-0011 capability probe |
| 5 | un-gate the tray (ksni as the third arm) + `dujactl doctor`'s Linux diagnostic |
| 6 | `xtask dist --target linux`, the release job, and the docs |
| 7 | phase gate, adversarial review, tag `m7-linux` |

**Wave 2 shipped the two Linux hardware backends.** External monitors come from
the DRM connector tree (`/sys/class/drm/card<N>-<TYPE>-<INDEX>`: `status`, `edid`,
and the `ddc` symlink's `i2c-dev/i2c-<N>` child, which is what proves
`/dev/i2c-<N>` exists rather than merely naming an adapter), driven over
`/dev/i2c` with one `I2C_SLAVE` ioctl and the **existing** cross-platform DDC/CI
codec in its Intel framing — `i2c-dev` carries the slave address out of band
exactly as `IOI2CSendRequest` does, so not a byte of protocol was written. The
built-in panel comes from `/sys/class/backlight`, written through logind's
`SetBrightness` with a direct sysfs write as the fallback (ADR-0022), and takes
its identity from the internal DRM connector's EDID because a backlight device
has none.

The scan itself lives in **`duja_core::linux::drm`**, beside `duja_core::macos`
and for the same reason: `duja-ddc` needs it for external monitors, `duja-panel`
needs it for the panel's identity, and neither crate may depend on the other. It
takes an injected filesystem root, so its rules are unit-tested on all three CI
lanes rather than on the one machine that has a `/sys`. What is genuinely
Linux-only is one ioctl and one D-Bus call.

**Wave 2 left both backends reporting no geometry**, honestly — sysfs does not
know where the desktop puts a monitor — so the planner planned no overlay and the
continuum stopped at the hardware floor. **Wave 4 supplies the rectangle.** The
display server enumerates its outputs (X11 `RandR` output plus CRTC rectangle;
Wayland `wl_output` name plus `xdg_output` logical geometry) and every connector
is joined to one of them.

Wave 2 recorded that connector-name equality holds for the modesetting DDX and
DRM-backed Wayland compositors, is reported not to hold for the NVIDIA
proprietary X11 driver (its own indexing: `DP-0`) or the legacy
`xf86-video-intel` DDX (`eDP1`, no hyphen), and that wave 4 owed it a fallback.
The EDID is that fallback: both sides read it off the same monitor and neither
invents it. Only the base block is compared, because sysfs publishes the whole
blob and an X11 driver may publish only the first 128 bytes.

**The first draft joined by name first, and its review showed that is exactly
backwards.** The NVIDIA case is not "the names do not match" — that driver
indexes from zero where DRM indexes from one, so the two namespaces *overlap and
are offset by one* and sysfs `DP-1` is the server's `DP-2`. A name-first rule
placed two of three displays on their **neighbour's** screen and stamped the
result "matched by name": a silent wrong answer, in the exact configuration the
fallback was added for. The passes now run strongest-evidence-first — name and
EDID agreeing, then EDID alone, then a bare name only where no EDID could have
checked it — and a name match that a present EDID contradicts is not taken at
all. Every Wayland placement is the third kind, because Wayland publishes no
EDID (there is no protocol for it).

**Ambiguity refuses rather than guesses, in both directions.** Two identical
monitors with no serial number are byte-identical to both sides, so neither is
placed: an overlay on the wrong screen is a silent wrong answer, where an
unplaced display is the state Linux was already in. Checking only that *one
connector matches one output* is half the rule, and the review found the missing
half is reachable — two identical monitors with one **disabled** leave a single
output both connectors match equally well, and a multi-GPU machine produces two
connectors both called `DP-1` once the `card<N>-` prefix is stripped. A pair is
claimed only when the match is unique both ways.

Both lists are joined **together, from one enumeration**: the monitors and the
built-in panel draw from one pool, so they cannot be handed the same output, and
a display event costs one connection rather than two. The rule is pure — the
outputs are an argument — so it runs on all three lanes; only the enumeration
itself is Linux-only.

**Geometry without a surface is still geometry without a surface.** Linux's
`PlatformDimmer` is `StubDimmer` until the overlay lands, so the planner now
produces overlay commands that are recorded and discarded. The visible result is
unchanged — the continuum still stops at the hardware floor — with one exception
worth knowing about: surface tokens also switch **mirror grouping** on, and the
group rule pins a software-only group's hardware members to maximum on the
premise that one shared overlay does the dimming. That premise is false until the
overlay exists. `debt.md` carries it, with why withholding the token instead
would be worse.

**Wave 4 landed ADR-0011's capability probe first, on purpose.** The rule that
decides what a Linux session can dim is pure — environment and Wayland registry
contents in, a per-mechanism report out — and it is the largest surface this
feature has that any test can falsify: real X11 windows and real `wl_surface`s
cannot run on a headless runner at all. It names no `wayland-client` type, which
is the constraint that lets it compile and run on the Windows and macOS lanes too
rather than only on ubuntu.

Two things it gets right that a compositor-name table could not represent:
layer-shell without gamma-control (the common wlroots configuration) still dims,
because ADR-0003 makes the overlay primary; and gamma is **necessary-not-sufficient**
on registry presence, because `wlr-gamma-control` grants one client exclusive
access per output and refuses the bind afterwards — so a session running
`wlsunset` advertises the protocol and still says no. The report is therefore a
value that can be downgraded after startup, which is the same rule `#96` and
`#109` established one layer up.

No compositor is named anywhere in it, and a test asserts that: every reason
string is checked for the absence of `gnome`, `kde`, `mutter`, `kwin`, `sway` and
`plasma`. The X11 and Wayland surfaces themselves are the rest of the wave.

**Then building the surface found a defect in the rule, and it was the dangerous
kind.** ADR-0011 as first written said an X11 overlay needs no extension and that
a successful connection was the whole requirement. X11 has no per-window
translucency: the server draws a window's colour bytes at full coverage and
ignores its alpha, which only a **compositing manager** reads and blends. Duja's
overlay is black, so on a bare X session every alpha paints the same thing, and
the first drag below the hardware floor would have turned the monitor solid black
with no visible way back. The overlay arm now also asks whether a compositing
manager owns `_NET_WM_CM_S<n>` — the selection the window-manager spec requires
every one of them to take, which is what `gdk_screen_is_composited` asks too. That
keeps it a capability question about the live session rather than an identity one,
and leaves the RandR gamma ramp available on exactly the sessions that have no
compositor.

The review of that fix then established that the check is **necessary and not
sufficient**, and both gaps belong to the wave that builds the window. A
compositing manager that stops mid-session leaves an already-mapped overlay
unredirected, and nothing re-resolves the report — the event pump carries kernel
uevents and suspend, not the death of an X client — so the overlay has to watch
the selection itself and tear down on an owner change, which is the exact analogue
of `refuse_gamma`. And every compositing manager unredirects fullscreen windows,
which an always-on-top overlay is, so it must carry
`_NET_WM_BYPASS_COMPOSITOR = 2` to forbid that. The ADR amendment states both,
`debt.md` carries both, and the QA checklist runs the black-screen cases **before**
the happy path.

**Wave 3 gave Linux a real event pump, an autostart entry and a browser.** Display
hot-plug comes from the kernel's `NETLINK_KOBJECT_UEVENT` socket directly, with no
libudev: that is a C library and a system dependency, to receive the same messages
`rustix` reads through safe wrappers, and it would not have worked in the
containers and `ssh` sessions where the D-Bus half is absent anyway. Suspend and
resume come from logind's `PrepareForSleep`, which is the only source there is —
the kernel offers userspace no equivalent. A machine with no system bus gets
hot-plug alone and no error, which is the split ADR-0022 chose.

**The whole pump contains no `unsafe`.** The netlink socket, its address type, the
`poll` that waits on it and the self-pipe that ends it are all `rustix` safe
wrappers, so `duja-platform` still confines `unsafe` to its Windows and macOS `sys`
modules. Doing it through `libc` would have meant a hand-rolled `sockaddr_nl` and
four unsafe blocks for no capability the safe path lacks.

Autostart is one XDG `.desktop` file in `~/.config/autostart`, not a systemd user
unit: the unit starts Duja on *login*, including an `ssh` login with no display
server and no tray to put an icon in, where the spec starts it when a **desktop
session** starts. `open_url` uses `xdg-open` rather than the portal's `OpenURI`,
which wants a parent-window handle Slint cannot produce under Wayland. Dark mode
comes from the portal's `color-scheme`, the one cross-desktop key.

Two Linux gaps stay open on purpose and are in `debt.md`: `SessionUnlocked` has no
honest source (logind's `Lock`/`Unlock` are requests, not state), and
`cursor_anchor` is still the placeholder, because answering it needs the
display-server connection wave 4 builds — and on Wayland there is no global cursor
position at all, so it is not a port of the Windows path.

## Notes & gotchas for whoever continues

- **Environment**: Rust pinned 1.96.1 (MSRV 1.94), MSVC, edition 2024.
  Smart App Control must stay **off** on the dev box (os error 4551 otherwise).
  Fuzzing on Windows needs the MSVC ASan DLL on `PATH` (see
  [fuzz/README.md](../fuzz/README.md)).
- **Session trap**: a disconnected session sees no displays — `duja-ddc`
  correctly returns nothing and `dujactl doctor` says so. Check `qwinsta`
  before blaming the code.
- **Cross-platform rustdoc trap**: rustdoc only resolves links in code the
  target actually compiles, so an intra-doc link from cross-platform code to a
  `#[cfg]`-gated item breaks on every lane that cannot see it. Use plain
  backticks there. (Broke PRs #8, #10, #17 on the Linux lane.) Since #85 the
  hazard is symmetric — `cargo doc` runs on all three OSes — so a link into
  `cfg(windows)` code now also breaks the macOS lane, and vice versa.
- **rustdoc silently skips private items**, and it strips them *before*
  resolving intra-doc links, so a plain `cargo doc` compiles a private module
  and then checks almost nothing in it. Every macOS backend is a private
  `mod mac;`, which is why 15 broken links sat undetected until #85 turned on
  `--document-private-items`. Note the asymmetry that hid this: Cargo passes
  that flag automatically for **binary** targets, so `duja-app`'s tray was
  covered by accident while the library crates were not. Both CI doc
  invocations now pass it explicitly and must stay in sync.
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

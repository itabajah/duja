# Duja - build history

The narrative record: what each release and each wave actually did, what its
review found, and which of its stated reasons turned out to be wrong.

This is deliberately long and deliberately not pruned. It is not the status of
the project - that is [STATUS.md](STATUS.md), which is a snapshot and stays
short. It is not the plan either - that is [plan.md](plan.md). This file exists
because the most expensive thing this project has produced is not its code but
its corrections, and a correction is only worth what it cost if it is written
down where the next person hits the same wall.

Sections are in the order they were written, which is roughly chronological.
Everything here was moved out of `STATUS.md` verbatim when that file passed
1,900 lines and stopped being a snapshot of anything. Exactly two sentences were
reworded in the move, both of them sentences that said "this is in STATUS" and
were falsified by the move itself. That count is stated because a blanket
"nothing was reworded" would have been the kind of claim this file is full of
corrections to.

## Contents

Every section carries an explicit `<a id>` anchor, and this list links to
those rather than to a heading slug - the slug rule is GitHub's and is not
verifiable from a checkout, which is the sort of thing this project prefers not
to assert.

  - [v0.1.1 — deep-review fix wave (2026-07-17)](#s01)
  - [v0.1.2 — multi-monitor & capability fix wave (2026-07-18)](#s02)
  - [v0.1.3 — internal-panel fallback fix (2026-07-19)](#s03)
  - [v0.1.4 — dark rebrand + the mirror/software-only pair (2026-07-19)](#s04)
  - [v0.1.5 — the sticky "software-only" probe fix (2026-07-26)](#s05)
  - [Structural wave — the tray.rs split + test infra (2026-07-26, post-v0.1.5)](#s06)
  - [P6 wave 2 — the macOS foundation, and two defects in a live install (2026-07-30)](#s07)
    - [`#103` — the gamma captions stop being Windows sentences (2026-07-31)](#s08)
    - [`#104` — macOS packaging: a universal `Duja.app`, in an image (2026-07-31)](#s09)
    - [What the review changed](#s10)
    - [And what the second review changed](#s11)
    - [`#106` — every Apple Silicon DDC request was malformed (2026-07-31)](#s12)
    - [`#105` — the built-in panel gets a position, so it can be dimmed (2026-07-31)](#s13)
- [What is done](#s14)
  - [P0–P2 — foundation, spikes, and the pure core](#s15)
  - [P3 — Windows hardware slice (`m3-win-hw`)](#s16)
  - [P4 — Windows MVP (`m4-win-mvp`)](#s17)
  - [P5 — Windows feature-complete (`m5-win-full`)](#s18)
  - [P6 — macOS port, wave 1 (backends landed 2026-07-11)](#s19)
      - [Windows UI hardening (#27–#30, live-QA driven)](#s20)
  - [Perceptual brightness continuum (v2, ADR-0014)](#s21)
  - [UI layout & ruby theme (2026-07-14)](#s22)
  - [v0.1.0 release (2026-07-16)](#s23)
- [v0.1.6: the checkpoint that shipped the ports](#s59)
  - [The decision, and the loop it broke](#s60)
  - [What the reviews found](#s61)
  - [The correction that was the defect](#s62)
  - [What was verified rather than assumed](#s63)
  - [What it cost, and what it did not do](#s64)
- [P8 waves, as planned and as they went](#s52)
  - [Wave 1 - the binary](#s53)
  - [Wave 2 - the fuzz and coverage lanes](#s54)
  - [Wave 3 - `--soak`](#s55)
  - [Wave 4 - the debt drain](#s56)
  - [Wave 5 - the security pass](#s57)
  - [Wave 6 - the gate](#s58)
- [P8 gate results](#s47)
  - [What this gate was](#s48)
  - [The finding no single-PR review could have found](#s49)
  - [The pattern across all six reviews](#s50)
  - [What was checked and found correct](#s51)
- [P8 wave 5: the SECURITY.md checklist, item by item](#s46)
- [P7 waves, as planned and as they went](#s41)
  - [Wave 5 - the tray, and what it turned out to own](#s42)
  - [The one architectural item worth scheduling](#s43)
  - [Wave 6 - packaging](#s44)
  - [Wave 7 - the gate](#s45)
- [P7 gate results](#s36)
  - [What this gate was, and what it was not](#s37)
  - [The one finding that changed nothing, and why that is the result](#s38)
  - [The finding that became a row](#s39)
  - [What was checked and found correct](#s40)
- [P6 gate results](#s24)
  - [The blocker](#s25)
  - [The other findings that changed code](#s26)
  - [The near-regression](#s27)
  - [Documentation the phase falsified](#s28)
  - [Rubric](#s29)
  - [Deliberately still open](#s30)
    - [After the gate — a CI flake, and a confident wrong diagnosis (`#113`)](#s31)
- [P5 gate results](#s32)
- [Live hardware QA (2026-07-11, console session, MSI MP273QP over DDC)](#s33)
  - [1. Pure-visual QA — SIGNED OFF (user, 2026-07-16)](#s34)
  - [2. Known gaps carried forward](#s35)

---


<a id="s01"></a>
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

<a id="s02"></a>
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

<a id="s03"></a>
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

<a id="s04"></a>
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

<a id="s05"></a>
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

<a id="s06"></a>
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

<a id="s07"></a>
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

<a id="s08"></a>
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
macOS-only **at the time** — which is to say the correction did not merely reword the deferral, it
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

<a id="s09"></a>
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

<a id="s10"></a>
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

<a id="s11"></a>
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

<a id="s12"></a>
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

<a id="s13"></a>
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


<a id="s14"></a>
## What is done

<a id="s15"></a>
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

<a id="s16"></a>
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

<a id="s17"></a>
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

<a id="s18"></a>
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

<a id="s19"></a>
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

<a id="s20"></a>
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

<a id="s21"></a>
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

<a id="s22"></a>
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

<a id="s23"></a>
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

<a id="s59"></a>
## v0.1.6: the checkpoint that shipped the ports

Four PRs, `#150`-`#153`, and a release. What it was for: a clean checkpoint -
the app verified, the docs made true, the code tidied where tidying was owed,
and a release at the end.

<a id="s60"></a>
### The decision, and the loop it broke

Three phases had closed with a tag and no release, all waiting on the same
condition: nobody has run the macOS or Linux build on the hardware it targets.
That was recorded as a decision rather than a blocker each time, and each time
it was defensible.

**It was also self-blocking, and nobody had said so.**
[ADR-0013](adr/0013-macos-ddc-wrap-vs-vendor.md) keeps the macOS DDC path
experimental until three independent community confirmations per architecture
exist. Those come from other people's hardware. The artifact those people would
run had never been on the Releases page. The condition for releasing was
confirmation and the mechanism for confirmation was releasing, so holding
forever was the default outcome, and three closed phases with nothing shipped is
what that looks like from outside.

**And the hold had no mechanism.** `release.yml` has no version-line gating at
all - every `if:` in the file was read to confirm it - so the `macos` and `linux`
jobs run on any `v*` tag and their artifacts land in the same `SHA256SUMS`,
minisign pass, attestation and Release. The only thing implementing "held" was a
human not pushing a tag, which is one command away from happening by accident.

[ADR-0024](adr/0024-preview-artifacts-on-the-patch-train.md) is the decision that
came out of that: previews ship on the patch train, `v0.2.0` and `v0.3.0` are
re-mapped to mean *hardware-confirmed*, and every release carrying an
unconfirmed platform says so from a committed file rather than from whoever cut
it. A label is weaker than a hold, so it gets a mechanism: a Preflight step that
fails the release before anything is built if the preamble is missing or empty.

<a id="s61"></a>
### What the reviews found

**Four PRs, four adversarial reviews, and every one found something that would
otherwise have shipped.** That is now nine consecutive phases and checkpoints
where the same has been true. Worth recording per-PR rather than in aggregate,
because the pattern in what they found is more useful than the count:

| PR | the finding that mattered most |
|---|---|
| `#150` | the preamble told users `duja --restore` recovers a Wayland session. It deliberately does not connect at all; killing the process *is* the recovery there. Wrong for the majority of the Linux audience, in the file that replaced the hold |
| `#151` | the test's **name** claimed the one thing the experiment did not measure - it ran on a live session, not headless, and headless is the CI question the four debt rows actually need |
| `#152` | two false superlatives about coverage, in opposite directions and mutually inconsistent; plus a link the reorganisation broke, in a file that had zero before |
| `#153` | a **correction** that turned a true claim into a false one, and had to be reverted |

**Three of the four are the same species**: a claim that read as verified and
was not. The fourth is worse and is the one worth keeping.

<a id="s62"></a>
### The correction that was the defect

`#153`'s changelog said `duja.exe` is "19 % smaller than at `v0.1.5`". Reading
[ADR-0012](adr/0012-binary-size-budget-variance.md)'s ledger suggested that was
wrong - 19,446,784 is labelled the **P7** baseline, and v0.1.5 predates both
ports - so the number was removed and replaced with a paragraph explaining that
no v0.1.5 binary had ever been measured and the figure could not be known.

Every part of that was wrong, and a reviewer settled it by **building v0.1.5**:

```
v0.1.5  duja.exe   19,333,120 bytes   (lto = "thin", opt-level = 3)
v0.1.6  duja.exe   15,729,664 bytes   (lto = "fat",  opt-level = "s")
                   -3,603,456         = 18.64 %, which rounds to 19 %
```

Independently reproduced before the revert. The `~19 MB` figure was also
recorded in `docs/STATUS.md` **at the v0.1.5 tag** the whole time, so "never
measured" was falsifiable by `git show`. And 19 % is both numbers at once, for a
reason the ledger does not make obvious: P6 and P7 added almost nothing to the
*Windows* binary, because their code is `cfg`-ed out of it.

This is [`#132`'s lesson](#s33) happening rather than being quoted - a review
round whose later findings are defects that earlier corrections introduced. The
rule that came out of `#132` was to weight code over prose. The rule this adds
is narrower and sharper: **when a correction turns on a number, measure the
number.** Reasoning from a ledger about what a binary used to weigh is not
measuring it, and the build takes ninety seconds.

<a id="s63"></a>
### What was verified rather than assumed

Named because a checkpoint reporting only findings cannot distinguish "checked"
from "not looked at":

- **[D-102](debt.md#d-102)'s experiment, run at last.** `build_tray` succeeds in
  a test process and all three tray-seam verbs work, so the sentence four rows
  defer on is false in both halves. It is *not* the headless answer and the row
  now says so. It also falsified its own prediction that three rows would then
  close with no refactor: because the constructor succeeds, a naive test depends
  on the session it ran in, so what the rows need is a fakeable tray.
- **The release build and both budgets, on the tree being tagged.** `duja.exe`
  15,729,664 against 16,777,216. A 90-second soak on the release binary: peak
  RSS 16,936,960 against 35,000,000, zero growth, flat GDI and USER, `PASS` -
  and a 20-second run returning `UNMEASURABLE` with exit 1, which is the first
  observation of that guarantee on a release build.
- **The full pipeline, by `workflow_dispatch` before tagging.** Both non-Windows
  packaging jobs green, and the new Preflight step exercised for the first time
  by exactly the dry run it was designed to be reachable from.
- **Every relative link and anchor across ten documents**, mechanically.
- **The changelog's entry accounting**: 70 commits since `v0.1.5`, 9 of them
  `chore`/`ci`/`build` and skipped, 61 bullets.

<a id="s64"></a>
### What it cost, and what it did not do

`plan.md` shed 244 of its 351 lines to `history.md`, which is this file. The
[D-114](debt-archive.md#d-114) row was added rather than paid: both `xtask`
subcommands take `std::env::Args`, so their argument parsing is unreachable from
a unit test by construction. (Half of that was false, and `#156` established it
by measuring rather than by reading: `dist` delegates to a parser that has been
generic since it arrived, and four tests in that file already drove it. The row
was written from two signatures rather than from what was behind them - and the
correction then took three review rounds to get right, which
[debt-archive.md](debt-archive.md#d-114) records rather than tidies away.) The
fix is an hour and it was deliberately not taken here, because `xtask` is the
tool that *performs* the release, including the size
gate, and cutting `v0.1.6` with an `xtask` no release had ever used is the wrong
trade on the wrong day.

<a id="s52"></a>
## P8 waves, as planned and as they went

Moved here from [plan.md](plan.md) at the `v0.1.6` checkpoint, exactly as
P7's table was when P8 opened. That file's own opening rule is that anything
already done is described here rather than there; six completed waves were
244 of its 351 lines, which is the weight it exists to shed. The text is
unchanged from what it said while the work was in flight, including the
wave-4 `partial` verdict and the reasoning that turned out to be wrong.

**Four things were reworded in the move, and the first draft claimed nothing
was** - which is the same slightly-too-clean claim the P7 preamble below had to
qualify, made again one section up. Three sentences pointed at `history.md`,
the file they now live in, and one pointed at `plan.md` for a specification that
had moved here; a fifth, wave 3's link on the words "false assurance", was a
*broken* link on arrival, because the `## How work lands` heading that resolved
it stayed behind in `plan.md`. All five are repointed. No other prose changed.

**Read the imperatives below as P8's, not as yours.** "Do not start with the
levers", "apply each lever alone", "it should be the next thing anybody does
here" - all were addressed to whoever was executing P8, and several are now
done. D-102's experiment in particular, which wave 4's text calls the next thing
anybody should do, was **run** at the `v0.1.6` checkpoint. Note also that this
file now contains two `### Wave 5` and two `### Wave 6` headings, ~450 lines
apart: the near ones are P8's, the far ones P7's.

| wave | scope | state |
|---|---|---|
| 1 | binary size: measure first, then trim, then gate it ([D-011](debt-archive.md#d-011), [ADR-0012](adr/0012-binary-size-budget-variance.md)) | done - `#144`, D-011 drained |
| 2 | the fuzz and coverage lanes ([D-002](debt-archive.md#d-002), [D-023](debt-archive.md#d-023)) | done - `#145` |
| 3 | `--soak`, the harness two perf budgets already cite | done - `#146` |
| 4 | the debt drain (`refactor:` PR, the rubric's ~15% time-box) | **partial** - `#147`, see below |
| 5 | the security pass and the docs-truth sweep | done - `#148` |
| 6 | the phase gate - **the multi-reviewer one** - and `m8-hardening` | done - `#149` |

Waves 1, 2 and 3 are independent of each other and can land in any order. Wave 4
is **not** independent of wave 3: one of its rows ([D-005](debt.md#d-005)) is
deferred until the soak produces a real error-rate threshold, so that row waits
even though the rest of the wave does not. Wave 5 wants 1 through 4 landed,
because half of what it checks is whether the docs still describe what those
waves left behind. Wave 6 is last by definition.

<a id="s53"></a>
### Wave 1 - the binary, and checking the ADR's reasoning before following it

[ADR-0012](adr/0012-binary-size-budget-variance.md) raised the budget to 16 MB
at P4, P5 blew through it, and the ledger has said "P8 must recover it" for two
releases. `duja.exe` is **19,446,784 bytes** today. That is the whole of the debt.

The ADR lists levers in expected-payoff order **twice**, and the two lists are
not the same list: one in its *Decision* section (fat LTO, Slint image formats,
`env-filter`, and `panic = "unwind"` marked explicitly as not a lever) and a
second in its *Ledger* (feature-gate the update check, fat LTO, `env-filter`,
Slint image formats). Every reference below is to the **Decision** list, by name
rather than by number, because "lever 2" means different things in the two and
striking the wrong one would remove fat LTO.

**Do not start with the levers, and do not start with `cargo tree` either.** The
baseline attribution is already done, and it cost one wrong answer on the way,
which is the part worth writing down.

`cargo tree -p duja-app -e normal --target x86_64-pc-windows-msvc -i resvg`
appears to report that `resvg` reaches the graph only through
`i-slint-compiler` behind `slint-macros` - a proc macro, so host code, so not one
byte of `duja.exe`. That answer is **false**: `cargo bloat` puts `usvg` at
470.9 KiB of `.text` and `resvg` at another 141.6.

The mechanism is not deduplication, which is what this section said first and
what a reviewer disproved by running the command. **Adding `--target` makes
`cargo tree` print one root tree per feature-resolution universe** - the host
one, holding build scripts and proc macros, and then the target one - and it
prints them back to back under the same heading with only a blank line between.
The proc-macro tree comes first. Reading the first tree and stopping is the whole
of the error, and `--no-dedupe` does not fix it because nothing was deduplicated:
both trees were always there. `cargo tree -e normal -i resvg` **without**
`--target` prints the runtime path first and does not have the problem at all.

So the rule is: count the roots before reading the branches, and treat any
dependency question answered from the tree as a hypothesis until a linker
confirms it.

So ADR-0012's list of causes is right where it was doubted. What is genuinely
absent from the binary is the set nobody suspected: `ravif`/`rav1e` (an AV1
*encoder*, by some distance the largest thing in `Cargo.lock`), `exr`, `tiff`
and `qoi` - all of them reaching only the compiler, because `image-default-formats`
is already off.

**And one of the ADR's levers does not exist.** The Slint image-format one -
"the flyout uses no SVG/EXR/animated images - investigate disabling the decoder
stack Slint pulls by default" - has an answer:
`slint/std` implies `i-slint-core/std`, which implies `image-decoders` **and**
`svg`, with no seam between them. The formats that *were* optional are already
disabled. Removing the rest means patching Slint, which is not a hardening
change. The lever gets struck from the ADR rather than left there for the next
person to spend a day on.

That leaves three levers, and only one of them is a dependency change:

| lever | what it removes | measured `.text` |
|---|---|---|
| drop `env-filter` | `regex-syntax`, `regex-automata`, `matchers` | 345 KiB |
| feature-gate the update check | `rustls`, `ring`, `ureq`, `rustls-webpki`, `webpki-roots` | 724 KiB |
| fat LTO | nothing; it is a profile change | n/a, see below |

The middle row has a catch that decides whether it is worth doing at all.
[D-011](debt-archive.md#d-011) frames it as "so a *lite* build drops both" the TLS stack
and the WinRT toast bindings - and a feature that is **on by default in the
shipped build saves the shipped build nothing**. It creates the possibility of a
smaller artifact nobody currently builds. So it is not a lever against this
budget unless a lite artifact is also a decision, and this wave is not the place
to make that one. The 724 KiB stays in the table as the size of a choice, not of
a saving.

Fat LTO has no `.text` figure because it removes no crate; the ADR records -1.0
MB at P4 and the wave re-measures it. `.text` is 11.3 MiB of an 18.5 MiB file,
so a crate that leaves takes its read-only data with it and the *file* delta
should exceed the `.text` column - a prediction to check rather than assume.

**Apply each lever alone, with a number beside it**, because one combined diff
that lands several megabytes teaches nothing about which lever to reach for
next time.

What the wave owes when it is done:

- ADR-0012 **corrected in place**: lever 2 struck with the reason, the
  `cargo bloat` attribution in the ledger, and the `cargo tree` dedupe trap
  recorded where the next person will hit it. The ADR is what they read first.
- **Per-lever deltas**, in bytes, so the ledger stops being a list of guesses.
- The unit ambiguity settled. The budget says "16 MB" and the ledger's rows say
  14.9 and 17.21 with no unit named. 16 MiB and 16 MB differ by 5%, which is
  larger than the smallest lever on the list, so a budget that does not say
  which one it means cannot be missed *or* met on purpose.
- **A size gate that fails a build on a regression.** This is the part that
  matters after the wave ends. Size drifted from 14.9 to 19.4 across two
  releases with nothing to notice it, and a lever pulled once is a lever any
  dependency bump can give back.

  Where it runs is a real trade rather than a detail. The measurement is only
  meaningful on the profile that ships - fat LTO, one codegen unit - and that
  build is roughly twenty minutes on a hosted Windows runner, against a PR
  matrix that finishes in a fraction of that. Gating the *release* costs nothing
  and makes shipping over budget impossible; gating every PR catches the
  dependency bump on the PR that lands it and slows every other PR to do it.
  Whichever the wave picks, the half it does not pick is a debt row rather than
  an unstated gap - and if it does add a PR job, note that branch protection's
  required checks are a repository setting, so a new job is advisory until
  somebody turns it on.

<a id="s54"></a>
### Wave 2 - the fuzz and coverage lanes

[D-002](debt-archive.md#d-002) has been open since P2 and names two workflows;
[D-023](debt-archive.md#d-023) names the sixth fuzz target and says to land it with
them, which is right - a target nothing runs is a file, not coverage.

- **`fuzz_config_toml`.** `config.toml` is user-editable and parsed through
  chained `toml_edit` migrations. It is the one untrusted-parse surface with no
  fuzz target; caps, EDID, quirks, IPC frames and DDC packets all have one.
- **`fuzz.yml`.** A weekly nightly burn over all six targets, *plus* a cheap
  PR-time `cargo check` of the fuzz workspace. The second half is the part worth
  arguing for: `fuzz/` is a separate workspace, so nothing in the normal CI
  matrix compiles it, and a rename in `duja-core` breaks a target silently until
  the next Sunday. Checking it on the PR that breaks it costs seconds.
- **`coverage.yml`.** The rubric asks for core >= 90% and ipc/view-models >= 85%.
  Wire `cargo llvm-cov` and report per-crate. Whether the thresholds *gate* is a
  decision for the wave to make against the first real number, not now - and
  either way it must not become a twelfth required check that a flaky
  instrumentation run can block a merge on.

<a id="s55"></a>
### Wave 3 - `--soak`, which the budgets already assume exists

[perf-budgets.md](perf-budgets.md) names `--soak` twice: as the instrument for
"Idle RSS (flyout closed)" and as the whole of "Soak (24 h) RSS growth < 5 MB;
flat GDI/USER handle counts". **There is no `--soak` flag.** `--stress` exists
and does something else (a DDC input flood).

That makes two hard budgets unmeasurable by the method their own row cites,
which is the [false-assurance](plan.md#how-work-lands) shape this project has a rule
against: a maintainer reads the row, believes the budget is checked, and it
never has been. Either the harness exists or the rows are wrong; building it is
the better half of that choice, because ADR-0019 puts a soak in the definition
of `v1.0.0` and something has to produce that number.

Scope it to what a fake backend can drive unattended: RSS and handle counts
sampled on a fixed cadence, a growth verdict against the budget, and an exit
code. It runs on the dev box for the long burn, and a short one belongs in CI.

<a id="s56"></a>
### Wave 4 - the debt drain

The rubric time-boxes this at ~15% of the phase. Rows are picked for being
*fixable without hardware*, per the scheduling rule P8's plan stated up front and that did not travel with this text: **nothing in P8 should be sequenced behind hardware**, because a wave that cannot finish without a machine this project does not have is a wave that closes by being re-triaged.

- **[D-108](debt-archive.md#d-108) first**, because it is the one whose damage lands on
  a bystander: every clean quit writes identity gamma to *every* display, so
  quitting Duja flattens f.lux, redshift or a calibration curve it never
  touched. Test-first, red proven before the fix, and the defect re-inserted
  where it historically occurred rather than where the test can reach it.
- **[D-102](debt.md#d-102)'s cheap experiment.** One `#[ignore]`d test that
  constructs `PlatformTray` headless. If it passes, three of the four rows that
  defer on "`AppState` cannot be constructed in a test"
  ([D-016](debt.md#d-016), [D-040](debt-archive.md#d-040), [D-059](debt.md#d-059),
  [D-065](debt.md#d-065)) close with no refactor at all. It is an afternoon, and
  it decides whether a wave-sized job exists. Run it *before* planning any
  refactor, not after.
- **[D-005](debt.md#d-005)** - the `--stress` gate reports FAIL on a run with a
  single transient DDC error, which real hardware produces 1-2 of per ~300
  inputs. Its own deferral says to revisit "when the P8 soak numbers set a real
  threshold", and wave 3 is where those numbers come from, so this row is
  sequenced after it rather than beside it.
- **[D-093](debt.md#d-093)** - `WAYLAND_SOCKET` is not consulted, so a client
  handed a compositor socket is classified X11. Pure, testable, no session
  needed.

**What wave 4 actually landed, and what it did not.** `#147` drained
[D-108](debt-archive.md#d-108) and re-opened [D-093](debt.md#d-093) with a cause
rather than closing it. It did **not** do the other two items above, and marking
the wave "done" while two of its four rows were untouched is the kind of drift
this file exists to prevent - so the table says **partial**.

The omission that matters is [D-102](debt.md#d-102)'s experiment, because it is
not independent of D-108: that fix is tested at its seam and **not** at
`begin_quit`, and the reason given for that is the very sentence D-102 records as
out of date. The experiment is an afternoon and it decides whether the gap is
real. It should be the next thing anybody does here.

**[D-021](debt.md#d-021) is explicitly not in this wave.** Unifying `windows`
0.58 to 0.62 in `duja-panel` means rewriting VARIANT-era COM against an API that
changed, in the one module whose set-path has never executed on real hardware.
Its deferral note already says to do it in the pass that also runs WMI on a
borrowed laptop. That laptop is the same one three other rows wait on; the row
stays open and the reason stays written down.

<a id="s57"></a>
### Wave 5 - the security pass and the docs-truth sweep

The [rubric](review-rubric.md) singles P5 and P8 out for the **full SECURITY.md
checklist, item by item** rather than the summary skim every other phase gets.
Do that, and record what was checked rather than only what failed.

Then the truth sweep, which is cheap and catches the class of drift this project
keeps paying for. The instance this wave started from: `SECURITY.md` described
the release as three artifacts and invited provenance verification on "any of the
three", when P7 wave 6 had made it four. Nobody edited the security policy,
because the tarball landed in `xtask` and the release workflow and there was no
reason to look there - and `release.yml`'s own comment carried the same stale
count, which is how far that kind of drift travels. Both fixed in `#148`; the
write-up is below.

<a id="s58"></a>
### Wave 6 - the gate

**Run.** Three independent reviewers over the cumulative diff, each with a
distinct lens, on top of a per-PR adversarial review of all six waves. Nine
reviews; every one found something that would otherwise have shipped. The
write-up is below, including the one finding no single-PR
review could have seen - `--soak` printing to a console a release build does not
have - and the honest gap: no mutation-based test-strength pass ran at the gate,
because it needs to write to the tree the other reviewers were reading.

What it asked for, for the record: P7's was one targeted pass,
and both this file and its tag message say so at the top rather
than in a footnote. That was a defensible call for a phase whose code cannot
execute anywhere; it is not defensible for the phase whose entire subject is
whether this is ready to be called 1.0.

So: several independent adversarial reviewers over the cumulative
`m7-linux..main` diff, each finding verified by a reviewer that did not raise
it, weighted toward code over prose - the `#132` lesson, where a 28-round review saw
a growing share of its later findings become claims that *earlier corrections*
had introduced, with the code done around round nine.

Then `m8-hardening`, and `v1.0.0` held.

<a id="s47"></a>
## P8 gate results

The hardening phase gate, over the cumulative `m7-linux..main` diff: 6 commits,
42 files, +3,707/-383.

<a id="s48"></a>
### What this gate was

**The multi-reviewer gate the plan specified, run properly** (that specification is now [the wave-6 entry above](#s52) rather than in `plan.md`). P7's was
one targeted pass and said so at the top of its own write-up; that was
defensible for a phase whose code cannot execute anywhere, and it was not
defensible for the phase whose entire subject is whether this is ready to be
called 1.0.

Three independent reviewers over the whole diff, each with a distinct lens -
cross-wave correctness, a claims audit, and safety/supply-chain - and none
briefed on what the others were looking at. That is on top of a per-PR
adversarial review of **every one of the six waves**, which is itself more than
any previous phase received.

What it did not include: a mutation-based test-strength pass, which was scoped
and not run because it needs to modify the tree while the others were reading it.
Individual waves got mutation testing where a regression test was the deliverable
(wave 4's D-108 proof, wave 5's cap enforcement), so this is a gap in the gate's
coverage rather than in the phase's.

<a id="s49"></a>
### The finding no single-PR review could have found

**`--soak` printed to a console that a release build does not have.**

`main.rs` sets `windows_subsystem = "windows"` on a Windows release binary, so
the shipped `duja.exe` has no console. `GetStdHandle` returns an invalid handle,
std maps `ERROR_INVALID_HANDLE` to `Ok(len)`, and every line the harness printed
was silently discarded - no panic, no diagnostic. The shell does not wait for a
GUI-subsystem process either, so the exit code, which is the whole of the
"UNMEASURABLE is not a pass" guarantee, was unobservable too.

This is the shape a phase gate exists for. Wave 3 built the harness and reviewed
it hard - three blockers were found and fixed in its own PR. Wave 1 is what made
the *release* build the one that must be measured, because that is what the
35 MB budget is written against and what `opt-level = "s"` changed. Neither
review could see it, because the defect is in the seam between them. And
`main.rs` had documented the fact since P4: *"under a `windows_subsystem =
"windows"` release binary the CLI subcommands cannot write to a console"*. The
fact was in the tree; two waves wrote past it.

The report is now written to `soak-report.txt` beside the rotating log as well,
and [qa-checklist.md](qa-checklist.md) carries the `start /wait` invocation that
also yields the exit code.

<a id="s50"></a>
### The pattern across all six reviews

Nine reviews ran in this phase - six per-PR, three at the gate - and **every one
found something that would otherwise have shipped**. Worth recording as a
measurement rather than a sentiment, because it is the strongest evidence this
project has produced for its own review discipline:

| review | the finding that mattered most |
|---|---|
| wave 0 | the `cargo tree` mechanism written into the plan was false - `--target` prints two root trees, and the first is the proc-macro one |
| wave 1 | dropping `tracing-log` silently stopped capturing every `log::` record; `SubscriberInitExt::try_init` installs the bridge itself |
| wave 2 | the corpus seed the branch documented was never committed - `.gitignore` covers `fuzz/corpus/` |
| wave 3 | the Linux lane did not compile, and two verdicts reported PASS on runs where the budgeted thing measurably grew |
| wave 4 | the `WAYLAND_SOCKET` fix made its own target case *worse*: the variable is single-use |
| wave 5 | the security wave wrote two new false claims into the security policy |
| gate: cross-wave | the soak's output goes nowhere on the build it exists to measure |
| gate: claims | `perf-budgets.md` named "tracing span" as the instrument for three rows, and there is no `span!` in this repository |
| gate: safety | `None` handle counts meant two different things, and the doc claimed one |

**Three of those nine are the same species**: a claim that read as verified and
was not. That is the failure mode this project's first rule names, and the honest
conclusion is that writing the rule down does not prevent it - only a reader who
did not write the text does.

<a id="s51"></a>
### What was checked and found correct

Named because a gate reporting only findings cannot distinguish "verified" from
"not looked at":

- **Both new `unsafe` blocks are sound**, checked against the `windows 0.62`
  bindings on disk and Microsoft's contracts rather than against the comments
  that describe them: `&raw mut` forms no reference so there is no `noalias`
  claim to violate, `cb` is the pointee's own size so the callee cannot overflow
  it, the pseudo-handle needs no `CloseHandle`, and the last-error window is
  thread-correct because last-error is thread-local.
- **Every arithmetic claim in ADR-0012's ledger** - all seven rows' deltas, the
  superadditivity figure, the exemption cost, the percentage - verified
  independently. So were the two binaries on disk, to the byte, and the
  25,600-byte targeted/untargeted gap the ADR itself flags.
- **`cargo deny` clean on all four checks**; the lockfile *gained* nothing (its
  entire diff is 23 deleted lines); no RUSTSEC ignore went stale; all 35 `uses:`
  across four workflows are SHA-pinned.
- **`coverage.yml`'s threshold script traced as a program** - `set -e`
  interactions, the `local` declaration order that would have masked a failing
  `jq`, the awk comparison sense at exactly the floor, the zero-files guard, and
  the reachability of `exit "$fail"`.
- **Every caller of the global `duja_dimmer::restore_all()` judged individually**
  (five of them), and the crash-marker paths confirmed to be the same file, so
  wave 4's change cannot have made two of them disagree.
- **The `#[cfg]` matrix of every new module**, so that "tested on all three
  lanes" is true where it is claimed.
- **`SECURITY.md` end to end**, including which artifacts are actually attested
  versus minisigned and which steps are tag-gated.

<a id="s46"></a>
## P8 wave 5: the SECURITY.md checklist, item by item

[review-rubric.md](review-rubric.md) singles out P5 and P8 for the **full**
`SECURITY.md` checklist rather than the summary skim every other phase gets. This
is that pass, and it records what was **checked and found true** as well as what
was not - a security review that lists only its findings does not distinguish
"verified" from "not looked at", which is the distinction the reader needs.

### The one claim that was false

**"Config & quirks files: typed parsing only, size caps..."** The quirk DB had
`MAX_QUIRKS_LEN` (1 MiB). `config.toml` and `state.toml` had **none**:
`persist::read_to_string_opt` was `fs::read_to_string`, which allocates whatever
the file is. The threat is modest - a process that can write there is already the
user - but the shape is not: a policy document asserting a control that does not
exist is the serious kind of wrong, and the cheap fix is to make the sentence
true rather than to edit it out.

Capped at 1 MiB. Two checks rather than one, which is the part worth keeping: the
metadata length is only a pre-check, and `read_capped` is the enforcement,
because a file can grow between the two calls and because `/proc` reports a
length of zero for files with content - so a metadata-only cap is one a symlink
walks straight past.

**Three things about that were wrong in the first draft, and a review found all
three.** The enforcing branch had *no test*: deleting it left the suite green,
because the metadata pre-check shadows it for every ordinary file. It returned
the wrong error when it did fire - cutting at the limit can land mid-UTF-8, so
`Take::read_to_string` failed as `InvalidData` and the over-cap file came back as
`Io`, defeating the entire reason `TooLarge` exists. And the claim that "both
callers fall back to defaults" was false in three places: `ConfigDocument::load`
and `StateFile::load` **propagate**, and among *their* callers,
`settings_apply::persist_config_change` propagates too. `read_capped` is now a
separate function taking a reader, so the enforcement is testable and is pinned
by a mutation; it reads bytes and length-checks before converting; and the error
field is `at_least` rather than `bytes`, because the bounded read genuinely does
not know how big the file was.

The quirk DB is a different thing and the first draft conflated them. It is
`include_str!`-compiled, so there is no runtime file to cap and its
`MAX_QUIRKS_LEN` guards a parser rather than a read. Writing "1 MiB each for
config.toml, state.toml and the quirk DB, enforced before the file is read into
memory" made the policy *more* specific and thereby false - in the wave whose job
was to stop exactly that.

### The one that had gone stale

`SECURITY.md` described the release as "the Windows installer `.exe`, a portable
`.zip`, and (from `v0.2.0`) a macOS universal `.dmg`", and invited the reader to
verify provenance on "any of the **three** artifacts". P7 wave 6 made it four.
Nobody edited the security policy, because the tarball landed in `xtask` and the
release workflow and there was no reason to look there. Now four, with the note
that two of them are tagged and held.

### What was checked and found true

- **"The only network code is the update check."** Verified by exclusion:
  `TcpStream`, `UdpSocket`, `reqwest`, `hyper`, `curl` appear nowhere in
  `crates/`, and the only `ureq` call site is `bin_support/updates.rs`.
- **Its stated bounds all hold.** `MAX_RESPONSE_BYTES = 64 * 1024` applied
  through `Read::take` before the buffer is filled; `NETWORK_TIMEOUT_SECS = 5` on
  connect, read *and* write; `UPDATE_CHECK_INTERVAL_SECS = 24 * 60 * 60`. It
  parses `tag_name` and opens a page; nothing downloads or executes.
- **"No user-supplied regex"** - now stronger than the sentence claims. There is
  no regex *engine* in the binary at all: P8 wave 1 removed the last one with
  `tracing-subscriber`'s `env-filter`, which was pulling `regex-automata` and
  `regex-syntax` for a grammar Duja never used.
- **"Duja runs unprivileged."** No `runas`, no `AdjustTokenPrivileges`, no
  `CreateProcessAsUser`, no `setuid`. The single `setuid` string in the tree is a
  comment in `unix_dir` saying Duja never wants that bit on its own state
  directory.
- **The IPC controls are all present**: `MAX_FRAME_LEN = 64 * 1024` checked
  before any body buffer is allocated, `SO_PEERCRED`/`getpeereid` euid checks on
  unix and a PID/session check on Windows, `FILE_FLAG_FIRST_PIPE_INSTANCE` and
  `O_NOFOLLOW` against squatting, a 5 s per-read timeout and a connection cap.
- **Supply chain**: `cargo deny check` clean on all four (advisories, bans,
  licenses, sources); every third-party action in every workflow pinned by commit
  SHA.

### What this pass did not do

It read the code against the policy. It did not attempt exploitation, it did not
review the `unsafe` blocks for soundness beyond their `// SAFETY:` comments, and
it is a single reviewer rather than the independent pass the phase gate runs. The
IPC transport in particular has never been exercised by a hostile client - only
by `dujactl` and its own tests.

<a id="s41"></a>
## P7 waves, as planned and as they went

Moved here from [plan.md](plan.md) when P8 opened. That file's own rule is that
anything already done is described here rather than there, so it stays short
enough that reading it is never a research task - and a table of eight completed
waves is exactly the weight it is meant to shed.

**Exactly two sentences were reworded in the move, and both for the same
reason**: they pointed at the file they now live in. One said "history.md opens
the write-up with that distinction" and now points at
[the write-up below](#s37); the other said `#136` was "larger than **this file**
said it would be", where "this file" was the plan that had made the prediction,
and now names it. No other *prose* changed. Two **link targets** did, later: both
D-108 references in the wave-7 table and its prose were repointed from `debt.md`
to `debt-archive.md` when P8 wave 4 drained that row. The count is stated, and
now qualified, because a blanket "nothing was reworded" is the kind of claim the
section above this one exists to correct - and a P8 gate reviewer caught this
paragraph making a slightly smaller version of it.

**Read the imperatives below as P7's, not as yours.** "Read those four before
wave 6", "the first tool wave 6 reaches for", "do not fold this into wave 5" -
every one of those numbers is a *P7* wave, and all of them are closed. P8 has a
wave 5 and a wave 6 of its own and they are unrelated.

The ADRs and commit messages refer to these by number, so they are written down
rather than left implicit.

| wave | scope | state |
|---|---|---|
| 0 | unix IPC + lock-directory hardening (shared with macOS) | done - `#114` |
| 1 | the two reserved ADRs (0010 tray, 0011 dimming), plus 0022 | done - `#115`, `#117` |
| 2 | DRM/sysfs enumeration + EDID identity, `/dev/i2c` bus, backlight (logind primary, sysfs fallback) | done - `#116` |
| 3 | event pump (`NETLINK_KOBJECT_UEVENT` direct, no libudev) + autostart, desktop, geometry | done - `#118` |
| 4 | software dimming: X11 overlay + `RandR` gamma, Wayland layer-shell + `wlr-gamma-control`, and the ADR-0011 capability probe | done - `#119`, `#121`, `#122`, `#123`, `#124`, `#130`, `#131` |
| 4b-5 | the X11 cursor anchor, so the flyout has somewhere to open | done - `#132` |
| 5 | un-gate the tray (ksni as the third arm) | done - `#134`, `#136` |
| 6 | `xtask dist --target linux`, the release job, and the docs | done - `#140`, `#141` |
| 7 | phase gate, tag `m7-linux` | done - one finding, [D-108](debt-archive.md#d-108) |

**Two corrections to the original table, made at the 2026-08-07 checkpoint.**
Wave 5 was written as "un-gate the tray **+ `dujactl doctor`'s Linux
diagnostic**"; the diagnostic half shipped early, in `#120`, because a user with
no visible monitors needed to be told why before anything could be tested on
Linux at all. And wave 4 grew a **4b-5** sub-wave that the table never had: the
tray flyout needs a cursor anchor, `duja-platform` had none for X11, and that is
a wave-4-shaped job (a display-server query) blocking a wave-5 one. The table
now says so rather than leaving two PRs unaccounted for.

<a id="s42"></a>
### Wave 5 - the tray, and what it turned out to own

**Done**, in two PRs. `#134` landed the **seam**: `AppState` no longer names a
tray library, it holds one `PlatformTray` with three methods phrased as outcomes
(`set_accent`, `set_tooltip`, `announce_update`) rather than as menu edits. That
shape is what [ADR-0010](adr/0010-linux-tray-ksni.md) asked for, because
`tray-icon`'s menu model is imperative and ksni's is declarative, and a seam
written in `tray-icon`'s verbs would have forced the Linux backend to fake
handles it does not have.

`#136` landed the arm, and it was **larger than the plan said it would be**,
which is worth recording rather than smoothing over. Un-gating `mod tray` made
three things reachable on Linux for the first time, and each had to be built or
widened before the lane would compile:

- **`bin_support::gamma` had no Linux arm** - roughly the size of the macOS one.
  It could not be stubbed: a sink that refused every engage would re-introduce
  the failure `#96` fixed, because `dimming::plan` substitutes an overlay from
  `min_gamma_factor()` *ahead* of the engage rather than in response to one.
- **`ipc::TrayBridge` and `autostart::system()`** carried gates that had been
  proxies for "wherever the tray is".
- **`main.rs` still refused to launch the tray**, which is the one that matters:
  everything above compiled and tested green on the ubuntu lane for two rounds
  while the binary printed "not available on this platform". See
  [STATUS.md](STATUS.md)'s note on it - the technique that catches it is
  removing blanket `allow(dead_code)`s in the same PR as the un-gate.

Two things it deliberately did **not** do. Linux registers no global hotkeys
(`global-hotkey`'s backend there is X11-only) and now says so through a new
`RegisterResult::Unsupported` rather than half-working; that is
[D-103](debt.md#d-103). And it drained no debt rows except the one its own
deferral note demanded it drain - [D-098](debt-archive.md#d-098), the X11
crash guard, which landed in the same PR as the sink because a sink without a
guard ships without a net.

**The four remaining rows wave 5 owed are re-triaged rather than closed**, and
three of them changed state: [D-094](debt.md#d-094), [D-095](debt.md#d-095),
[D-096](debt.md#d-096) and [D-097](debt.md#d-097). The pattern is the same in
each - "deferred until Linux has a gamma sink" was the reason, that sink now
exists, and what is left is the actual work rather than the wait. Read those
four before wave 6: [D-097](debt.md#d-097) in particular now means "Wayland
gamma dimming does not work", where it previously meant "a gate refuses a
channel nothing was going to call".

**The constraint that shaped this wave has not gone away.** `duja-app` cannot be
built for Linux on the Windows dev box, so anything that links it is a CI-only
loop. The thing that made wave 5 affordable is in [STATUS.md](STATUS.md) and
should be the first tool wave 6 reaches for: an **isolated crate** pulling one
module in through `#[path]` *can* be cross-checked, clippy'd and rustdoc'd for
`x86_64-unknown-linux-gnu` locally, in seconds.

<a id="s43"></a>
### The one architectural item worth scheduling

Four debt rows ([D-016](debt.md#d-016), [D-040](debt-archive.md#d-040),
[D-059](debt.md#d-059), [D-065](debt.md#d-065)) all defer on "`AppState` cannot
be constructed in a test", and the 2026-08-07 checkpoint found that reason is
out of date. `#134` removed the `tray_icon::TrayIcon` half, and the "two live
Slint shells" half was never the blocker it was written as - `duja-ui` builds
both shells headless in its own tests today, under a test backend that is
already a workspace dependency.

[D-102](debt.md#d-102) carries the re-triage and, importantly, what is *not* yet
verified. The cheap experiment it names should come before any refactor is
planned: one ignored-by-default test that calls `PlatformTray`'s constructor
headless. If it succeeds, three of those four rows close with no refactor at
all. That is an afternoon, and it decides whether a wave-sized job exists.

Do not fold this into wave 5. It touches the same file the ksni un-gate does,
and `#82` is this project's standing example of what happens when a refactor is
smuggled into a PR that was about something else.

<a id="s44"></a>
### Wave 6 - packaging

**Done** (`#140`). `xtask dist` has a third target, the release workflow has a
third job, and the docs say what a Linux user gets.

The artifact is a **portable tarball**, `duja-<ver>-linux-x64.tar.gz` - the
Windows zip's twin, with a `.desktop` entry and an icon added. It is deliberately
not an AppImage or a `.deb`, and the reason is worth reading before anyone
"finishes the job": a package declares a dependency set, and that declaration is
exactly what cannot be checked from a machine which has never run this binary
([D-107](debt.md#d-107)). The tarball is what unblocks the answer rather than a
placeholder for it, because the gate below needs something a human can extract
and run.

Two things this wave got for free by following the wave-5 split. The artifact
*names* moved into `xtask`'s `bundle` module, where all three are asserted
together on every lane - a mislabelled archive builds, uploads and checksums
exactly like a correct one, so a name is the packaging decision no runner can
catch. And `--target linux` refuses on a non-unix host rather than staging a
tarball whose binaries would extract without their permission bit, which is the
worst shape a packaging bug takes: clean everywhere except the user's machine.

<a id="s45"></a>
### Wave 7 - the gate

**Run, and narrower than this section used to ask for.** What it asked for was
several independent adversarial reviewers over the cumulative diff, each finding
verified by a separate agent. What happened was one targeted pass, scoped by hand
to the Linux code that has never executed and to the cross-crate invariants no
per-crate suite sees. [The write-up below](#s37) opens with that distinction
rather than burying it.

One finding changed the tree: [D-108](debt-archive.md#d-108), every clean quit writing
identity gamma to displays Duja never touched. One suspected finding turned out
to be already guarded, and is recorded as such - the token a Linux display is
addressed by is stamped in one crate and parsed in another, with every fixture in
both written in a shape the parser rejects, which is precisely the P6 blocker's
shape and is held by a round-trip test that already exists.

**The multi-reviewer gate remains available and unrun.** It is the obvious thing
to spend effort on if `v0.3.0` is ever to ship without hardware verification -
but the hardware run is the cheaper and larger of the two, because every accept
path in the tray and the gamma sink is still unexercised on every lane.

<a id="s36"></a>
## P7 gate results

The Linux phase gate, run over the cumulative `m6-macos..main` diff (28 commits,
132 files, +31,135/-3,199).

<a id="s37"></a>
### What this gate was, and what it was not

**It was one reviewer's targeted pass, not the gate `plan.md` specifies**, and
that is stated first because the difference matters when reading what follows.
P5's and P6's gates were several independent adversarial reviewers over the whole
diff, each finding verified by a separate agent before it was accepted. This one
was a single pass, scoped by hand to the surfaces most likely to hold a defect
nothing else could catch: the Linux code that has never executed, and the
cross-crate invariants no per-crate suite sees.

So the honest claim is **not** "P7 passed a gate of the same strength as P6's".
It is: the P6-shaped failure was looked for deliberately and not found, two
specific questions were answered, and the multi-reviewer gate remains available
and unrun. `m7-linux` is tagged on that basis, with `v0.3.0` held - which is the
same posture P6 took for a different reason, and the reason both are held is the
same one: **nothing here has run on the hardware it targets.**

<a id="s38"></a>
### The one finding that changed nothing, and why that is the result

The first thing looked for was P6's blocker by shape rather than by subject: *a
fake or a fixture that decodes a bug into the right answer, on a wire someone
else defined.* P7 has an obvious candidate. `linux::outputs` stamps the X11 gamma
token, `bin_support::gamma::gamma_address` parses it back, the two are in
different crates, and **every token fixture in both crates is written `crtc-42`
-- a shape the production parser rejects outright.**

That is the right thing to be suspicious of, and it turned out to be already
held: `linux::outputs` stamps through `linux_gamma::crtc_token` rather than
`to_string`, with a comment saying why, and `crtc_token_round_trips` joins that
function to `crtc_from_token` across `[1, 63, 4096, u32::MAX]`. A test was written
to join the app's end as well, proven red by mutating the stamp to match the
fixtures -- and then **deleted**, because the dimmer's round trip reds on the same
mutation and the app's `gamma_address` tests already pin the delegation. Recorded
because a redundant test with a long comment is the `#132` pattern this project
has a rule against, and because "the gate found nothing here" is a weaker
statement than "the gate looked for exactly this and found it already guarded".

<a id="s39"></a>
### The finding that became a row

**Every clean quit writes identity gamma to every display**
([D-108](debt-archive.md#d-108)). `begin_quit` restores what the session engaged and then
calls the *global* `duja_dimmer::restore_all()` unconditionally, and that call
means three different things: macOS reloads the user's colour profile (benign),
Windows and X11 write identity to every display or CRTC they can enumerate
(a flatten), and Wayland releases only this process's controls (benign).

It is **not P7's defect** -- the call predates the Windows train -- but P7 is what
makes it reach the platform where the victims are common, and, more sharply, what
makes it *redundant* there: leftovers from a dirty run are the crash marker's job,
and P7 is the wave that gave Linux a marker. Left as a row rather than fixed in
the gate, because the fix changes shipped Windows quit behaviour and belongs in a
PR whose subject that is.

<a id="s40"></a>
### What was checked and found correct

Named because a gate that reports only its findings does not distinguish "looked
at and fine" from "not looked at":

- **The tray pixmap's byte order**, against `ksni::Icon`'s own doc comment
  (`"ARGB32 format, network byte order"`) rather than against the belief that
  produced it. `rgba_to_argb32` writes A, R, G, B positionally, which is correct
  and would stay correct on a big-endian host.
- **Menu parity with the Windows backend**: identical item set and order, with the
  update row rendered from state rather than prepended -- which is the one place
  the two backends *must* differ, since ksni re-renders on every host refresh.
- **`spawn_relaunch`** is plain `std::process`, so the Linux "Restart" item is not
  a menu entry with a Windows-only handler behind it.
- **The toast's Linux arm** is a documented no-op with a debt row, and is honest
  about being blocked on scope rather than on the platform -- unlike macOS, which
  is genuinely blocked.
- **The Wayland flyout anchor** returns the fallback deliberately, with the
  `StatusNotifierItem.Activate(x, y)` route recorded as the real answer. Not an
  omission: Wayland has no global cursor query and no client-side toplevel
  positioning, so there is nothing to port.

<a id="s24"></a>
## P6 gate results

Four adversarial reviewers over the cumulative `v0.1.5..main` diff (23 commits, 96
files, +17,171/−3,555), split as: macOS backends + pure rules; app/tray/platform/
UI/CLI; packaging/CI/release docs; and a holistic cross-crate + rubric pass.
**Three returned APPROVE-WITH-FIXES, one BLOCK.** Six PRs closed it out (#106–#111).

<a id="s25"></a>
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

<a id="s26"></a>
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

<a id="s27"></a>
### The near-regression

**#108 was blocked by its own review, correctly.** The macOS `enumerate_displays`
skips a display with an unreadable EDID *before* consuming its I2C service, which
reads as a queue desynchronisation. It is not: an unreadable EDID means a **virtual**
display (Sidecar/AirPlay/DisplayLink), which has no `DCPAVServiceProxy` and never had
a slot to spend. The "fix" would have handed a real monitor's service to a display
that cannot use it and then released it — losing DDC control of that monitor
entirely. What landed instead is the invariant at the call site plus a debt row
naming the wrong fix, so it is not attempted a third time.

<a id="s28"></a>
### Documentation the phase falsified

Six claims (#110), the sharpest being that `sha256sum` — the documented verification
command in README, `SECURITY.md` and the release checklist — **does not exist on a
stock macOS**, on the one platform where the binary carries no publisher identity.
Also: the release gate does not run before the macOS job builds and signs; the
attestation covers three artifacts, not two; and the support matrix still called the
macOS tray "planned".

<a id="s29"></a>
### Rubric

Clean on: typed errors with no `unwrap`/`expect`/`panic` outside tests; every
`#[allow]` carrying a `// RATIONALE:`; all new `unsafe` behind a `// SAFETY:`;
`duja-core`/`ipc`/`dujactl` genuinely unsafe-free under `forbid`; no new idle
wakeups; CHANGELOG/debt/ADR discipline. Two deviations recorded rather than
papered over: `duja-panel` keeps FFI in its backend modules rather than a `sys`
submodule (its own long-standing convention, and restructuring untested COM code
blind is what `debt.md` row 27 warns against), and `duja-platform` established
`platform` as a second name for the same role.

<a id="s30"></a>
### Deliberately still open

The remaining macOS items are hardware-blind, and writing blind FFI to close them
is the trade this project has repeatedly declined: the built-in panel's fallback
carrier, the `mac/mod.rs` token-assembly hoist (a swap there is proven undetectable
— it leaves the suite green), mixed-DPI flyout placement (whose "needs hardware"
deferral the gate *disproved* from pinned `dpi` source, so it is now a real
candidate), and the unix-socket hardening that became live on macOS. All carry rows.

<a id="s31"></a>
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

The generalisable lesson, and the reason this is written up here rather than only
in the ledger: **a mechanism you can measure is not thereby the cause.** The measurement
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

<a id="s32"></a>
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

<a id="s33"></a>
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

<a id="s34"></a>
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

<a id="s35"></a>
### 2. Known gaps carried forward
- **Binary ~19 MB > 16 MB budget** — P8 must recover it (ADR-0012 ledger; the
  v0.1.0 WinRT toast bindings widened the P5 17.21 MB overage).
- **WMI panel set-path** has never executed on real hardware (this box is a
  desktop): borrow a laptop for a 30-minute run before the beta.
- Suspend/resume does not re-push DDC levels when the display set is unchanged;
  `classify_failure`'s `GetLastError` assumption needs a live unplug.
- Quirk user-override file, sync-group UI, in-UI hotkey editing, OS theme
  detection — all tracked in [debt.md](debt.md).

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

**The X11 overlay is the surface that rectangle was for.** One
override-redirect, depth-32 window per dimmed display, filled with premultiplied
black at the planner's alpha, on a dedicated thread that owns every window — the
same shape as the Windows backend, diffing through the same pure `plan` kernel.

Three of its decisions are arithmetic rather than windowing, and all three fail
*invisibly*, so they live in a pure module tested on every lane. **Which visual**:
a depth-24 visual is what a naive `create_window` inherits from the root, and it
has no alpha channel, so the overlay would be created, mapped, and opaque. **Where the alpha
goes**: it is the *top* byte of the pixel and the colour bytes must be zero;
anywhere else and the overlay is invisible or a coloured wash. (The first draft
justified this as premultiplied-versus-straight alpha, and its review pointed out
that black is `(0, 0, 0)` in both, so no premultiplication mistake is possible
here — `linux_caps` already said so.) **Whether the rectangle fits**: X11 geometry is
16-bit, and doing that conversion with `as` would wrap a monitor past 32767
pixels onto a display nobody asked to dim.

A second thread holds `XFixesSelectSelectionInput` on `_NET_WM_CM_S<n>`, tears
every overlay down the moment the owner goes to `None`, and **latches** — which
is the `refuse_gamma` analogue `#121`'s review asked for, both halves of it. The
first draft had only the teardown, and its review pointed out that the very next
slider sample would then map fresh windows onto a session that could no longer
blend them: the same black screen, one frame later. The overlay also sets
`_NET_WM_BYPASS_COMPOSITOR = 2` against fullscreen unredirection, which is the
only standard lever there is and is recorded as a mitigation rather than a
settlement, because a compositor evaluates unredirection against the window it is
considering and picom unredirects the whole screen.

**X has no always-on-top**, which the same review caught. `CreateWindow` places a
window above its siblings and every top-level is a sibling of an
override-redirect overlay, so raising once at map time means the first window the
user opens sits *undimmed* on top of the dimming. The watcher selects
`SubstructureNotify` on the root and the worker re-raises anything that is not
its own — coalescing the burst a dragged window produces, and damped to one raise
per 100 ms so that trading raises with another always-on-top client is a visible
flicker rather than two pegged CPUs. No X client can do better than bound that;
`debt.md` says so rather than pretending otherwise.

**A crashed compositor is not a disowned one**, which round two caught and which
would have defeated the whole guard. `XFixesSelectionNotify` fills its `owner`
field from the selection record, and for the *crash* subtypes
(`SelectionWindowDestroy`, `SelectionClientClose`) that record still names the
window that just died — so the owner is non-zero and a check for `NONE` reads it
as "a restart that already has a new manager". `picom` segfaulting would have
left every overlay up and unredirected: solid black, exactly the release-blocker
case. The watcher now ignores the field and re-asks the server, which costs one
round trip on a rare event and is right under any reading of who fills it.

**Every request that could trap a user is checked**, not merely queued. An x11rb
void request returns a cookie meaning "sent"; a protocol error arrives later, on
the event queue. So a failed input-region call would have read as success and
left a mapped, full-screen window that swallows every click — with the flyout you
would use to turn it off underneath it. Window creation and the input region take
a round trip each; the property writes and the map do not, because neither can
trap anybody.

Input passes through by the **`XFixes` empty input region** — SHAPE's own
`ShapeInput` could express the same thing, so this is the mechanism chosen rather
than the only one available — and the backend refuses to start where the
extension is absent rather than mapping a window that would swallow every click. It refuses on a
server with no ARGB visual for the same reason, and reports `Unsupported` — not a
fault — where there is no compositing manager or no display server at all.

**`PlatformDimmer` is not a type alias on Linux**, unlike the other two
platforms. Which mechanism exists is a property of the session rather than the
build, so `LinuxDimmer` picks when it starts. Since `#130` a Wayland session
gets `WaylandDimmer` — a `zwlr_layer_shell_v1` surface per dimmed **output**,
sized by the compositor and filled by scaling one pixel through a
`wp_viewport`. It reports `Unsupported` when the compositor is missing **any
one** of the three interfaces `linux_caps` names, not only when it has none of
them: on GNOME that one is `zwlr_layer_shell_v1`, Mutter implementing the other
two.

`#130` **moved** the `#122` mirror-pin consequence off Wayland rather than
narrowing it there again. The pin needs a clone group with two members, and a
Wayland session cannot produce one: the surface token is the `wl_output` name,
`linux_outputs::resolve` places a connector only where the match is mutually
unique, so no two displays ever carry the same token, so `group_clones` only
ever yields singletons and `fan_out_hardware` writes nothing for a lone
software-only member.

**X11 is where it actually bites, and that is not new.** Its token is the CRTC,
which two outputs in a `--same-as` mirror genuinely share, so an X11 group *can*
have two members — and the overlay the pin assumes is **not** unconditional
there: `X11Dimmer::spawn` refuses outright when nothing owns `_NET_WM_CM_S<n>`,
because without a compositing manager every alpha paints the same opaque black
rectangle. So on a bare WM with no compositor, a mirrored pair with one member
latched software-only pins the other to 100% with nothing drawing. `debt.md`
carries that, re-scoped from "Wayland" to "X11 with no compositing manager",
along with the hypothetical Wayland residual.

**A Wayland session is refused twice, by two gates that cover each other.** The
environment check (`WAYLAND_DISPLAY` is set ⇒ not X11) is cheap and skips the
connect, but this crate had already written down that it misfires:
`Transport::X11`'s own docs name "a systemd user unit, a sanitised environment",
and `sudo`, `ssh -X` and a `tmux` server older than the session are the same
shape. A misfire is not a visible error — it is a ramp written to an Xwayland
CRTC, an `Ok(())`, and a screen that never changed. So the server is asked too,
with the `XWAYLAND` extension query X.Org added for exactly this: *"Only Xwayland
initializes this extension. Thus, if the extension is present, the X server is
Xwayland."*

The first draft of this paragraph called that second gate authoritative, and it is
not: only Xwayland **23.1 and later** register the extension, and the 22.1 branch
that Ubuntu 22.04 LTS (supported into 2027) and Debian bookworm ship carries no
`xwaylandproto` dependency at all. (A later draft argued that from release dates,
which was a non-sequitur — point releases backport, and 22.1.9 postdates the spec
by over a year. The source tree is the evidence.) Neither gate is a superset of the
other — environment catches an old Xwayland that kept `WAYLAND_DISPLAY`, protocol
catches a new one whose environment was stripped, and an old one from a stripped
environment is caught by neither. Nothing available to an X client closes that
last case, so it is written down rather than papered over.

**The sub-floor gamma channel is `RandR`'s per-CRTC table on X11**, and the CRTC
is also the surface token wave 4 already stamps on every placed display, so the
app's gamma sink can address a ramp without a second enumeration. Two outputs on
one CRTC are an X11 mirror and share both a framebuffer and a gamma table, so the
CRTC is the granularity the hardware actually has.

**Since `#131` a Wayland session has its own, and it is a sibling rather than a
port.** `zwlr_gamma_control_v1` is addressed per `wl_output` by connector name —
which is the token wave 4 stamps there, so the same "no second enumeration"
property holds — and it inverts almost everything about the X11 one. The table
travels over a **file descriptor** rather than in a request, which is why `#131`
opened by taking X11's `maximum_request_length` ceiling back out of the shared
ramp builder: it was a fact about `SetCrtcGamma`'s encoding wearing the name of a
fact about gamma tables, and left there it would have refused a legal Wayland ramp
for an X11 reason.

Three differences are worth stating rather than discovering.

**A wrong table length is fatal to the connection**, not to the request: wlroots
answers a short read with `INVALID_GAMMA`, which terminates the client — so the
length rules live in `linux_wlr_gamma`, tested on every lane, and the backend opens
a *second* Wayland connection so a fatal gamma bug cannot take the layer-shell
overlay down with it. The table is also handed over on a **rewound** descriptor,
which is not fastidiousness: `SCM_RIGHTS` shares the sender's file offset, and
wlroots read this fd with a plain `read()` until `15f2f664` (2023-06-05, so 0.17),
which is after the wlroots 0.15 that Debian bookworm and Ubuntu 22.04 LTS both
ship — and after every 0.16 too, since the fix was not backported to either
branch. An
un-rewound memfd is at EOF, so on those the session's *first* dim would have
killed the connection. The first draft argued from the newer `pread` alone that
rewinding "would be a no-op dressed as care".

**A restore is a `destroy`** — and what that is worth is narrower than the first
draft of this paragraph claimed. It said the compositor "kept the original table
and hands it back", so a running `gammastep`'s tint would survive. It does not:
`gamma_control_destroy` emits `set_gamma` with no control attached and the
compositor applies *no* transform, which is the same end state an X11 identity
write produces. What the destroy actually buys is the **release** — this protocol
grants one client exclusive access per output, and an identity write has no way to
say "I am finished", so on X11 there is nothing to hand over and here there is.

**Enumeration does not bind**, because a control claims its output exclusively and
a read-only call must not lock a colour-temperature daemon out of every monitor to
answer a question; the availability answer stays where ADR-0011 puts it, at the
attempt. The cost of that is honest and recorded: ADR-0011's step 5,
`SurfaceCaps::refuse_gamma`, therefore still has **no** production caller, because
the only report on Linux comes from a probe that binds nothing. `debt.md` carries
it.

**A Wayland session is refused by transport, not by whether a connection opens** —
which is the decision the whole module is built around. `DISPLAY` points at
Xwayland on almost every Wayland session, so every step of the XRandR gamma path
*succeeds* there: it connects, `RandR` is present, `GetCrtcGammaSize` answers, and
`SetCrtcGamma` writes a table into a virtual CRTC that is not on the path to any
monitor. A gate that asked "can I reach an X server" would have produced an
`Ok(())` behind a screen that never changed, and the coordinator above would then
record a live ramp, never retry, and never plan the overlay that would have dimmed
the display instead. The refusal reads the environment, exactly as ADR-0011's
capability rule does.

**On crash safety the two Linux transports land on opposite sides**, which `#131`
established rather than assumed. **X11 sits with Windows.** The X server holds each
CRTC's table as server state and does not reset it when the writing client
disconnects — which is precisely why `xrandr --output DP-1 --gamma 1:1:0.5` works
as a one-shot command that exits — so a crash mid-dim leaves a dark screen with
nothing running to undo it, and the marker-plus-guard machinery Windows carries is
genuinely needed. It is deliberately **not** built yet: nothing on Linux engages a
ramp until the tray does (the sink the tray owns is the only engage path), so a
guard now would have no caller and its tests would pin a lifecycle nothing drives.
`duja --restore` is the manual rescue and is un-gated for Linux; `debt.md` carries
the guard as owed to the ksni wave, together with the baseline-composition that
would stop a restore flattening a running `gammastep`'s tint.

**Wayland sits with macOS.** A `zwlr_gamma_control_v1` dim lives exactly as long
as the client's object, the compositor destroys every object a client holds when
its socket closes, and destroying drops this client's colour transform — so the
recovery is automatic and survives `SIGKILL`. What comes back is the output's
*default*: an earlier draft called this "a stronger guarantee than macOS has" on
the strength of a curve-restoration that does not happen, and the phrase went when
its justification did. There is nothing for a rescue pass to find, so `restore_all` on that
transport does not even open a connection: a `duja --restore` process holds no
controls, and an empty clean report is the truth rather than a shrug. `#131`
narrowed the `#124` debt row to X11 for exactly this, and checked the property
against wlroots' `types/wlr_gamma_control_v1.c` rather than the protocol's prose
alone — which mattered twice. That prose calls a `uint16_t` a "16-byte unsigned
integer"; and its "restored to its original value" reads as though a previous
client's curve comes back, when the implementation clears the transform outright.
The first is why the pure module pins the entry width. The second cost this PR a
review round, because the wrong reading had been written into six files.

**The HDR verdict is now one module rather than three.** Each platform probes its
own way — DXGI's colour space, `NSScreen`'s EDR headroom, and on Linux the
transport, because there is no query to make — but what the answer *means* is the
same everywhere and carries the safety rule that an uncertain probe reads as "no
gamma". That was two byte-identical copies before Linux would have made it three.
X11 answers `Some(false)`: the X protocol has no HDR path, so an X11 desktop is
SDR and its CRTC LUT is the SDR pipeline's. Wayland answers `Unknown`, because
that is where Linux HDR actually happens and there is no query to make.

**`#131` turned that from a free answer into a costly one, and it is recorded as
debt rather than papered over.** The argument for `Unknown` used to end "and it
costs nothing, because a Wayland session has no XRandR channel to use it with
anyway" — which was true until `#131` built the channel it was talking about.
`Unknown` does not allow gamma, so a caller that respects the verdict will plan an
overlay and the new backend will not be engaged. That is the safe direction (a
ramp under HDR is at best ignored and at worst a display Duja believes it has
dimmed and has not) and it is not the finished one. The remedy is a probe rather
than a better guess, and it is ADR-0011-shaped: the colour-management protocol
gives each output an image description whose `tf_named` names the transfer
function, so a PQ or HLG output is knowably HDR. It answers for fewer outputs than
that sentence first implied — the sibling `tf_power` event describes a pure power
curve and names nothing, so it stays `Unknown` there as well as on a compositor
with no colour management at all — and it needs the `staging` feature of
`wayland-protocols`, which this workspace does not enable, rather than nothing at
all. `debt.md` carries both corrections, owed by the same wave that owes the gamma
sink.

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

`SessionUnlocked` has no honest source (logind's `Lock`/`Unlock` are requests, not
state) and stays in `debt.md`.

`cursor_anchor` was the second gap and is now half closed. **X11 has a real
backend** (wave 4b-5): `QueryPointer` for the cursor, RandR's CRTC list for the
display under it, and the EWMH struts for that display's work area. Three things
about it are worth knowing before touching it.

The work area comes from struts rather than from `_NET_WORKAREA`, which EWMH
defines as one rectangle **per desktop** — on two monitors a panel on either
shrinks the single global rectangle and the other monitor inherits a gap it does
not have. Struts are in root-window coordinates and explicitly not relative to a
Xinerama monitor, so a 40px panel along the bottom of a short monitor beside a
taller one reserves 160, and the taller monitor must keep its full height.

The scale factor is a deliberate **mirror of winit 0.30's chain** — the
`WINIT_X11_SCALE_FACTOR` override, then XSETTINGS `Xft/DPI`, then the `Xft.dpi` X
resource, then a measurement from the display's pixels and millimetres quantised
to twelfths. It has to be winit's number rather than a defensible one, because
the consumer multiplies a logical size by it to get the box it clamps into the
work area.

The chain is mirrored faithfully. **Where it is evaluated is not, and that is a
live divergence rather than a latent one.** Only the chain's last step is
per-monitor, and there Duja reads the cursor's monitor while winit reads
`monitors[0]` — not by choice: `x11/window.rs` guesses a new window's monitor from
`XIQueryPointer`, whose `root_x` is `Fp1616` fixed point, and casts it to `i64`
without the `>> 16`, so no rectangle contains the pointer — except at the root
origin, where `0 << 16` is still `0` and winit's inclusive `contains_point`
matches — and the guess falls through to the first enabled CRTC. That guess is a
transient winit corrects on the first synthetic `ConfigureNotify`, so the cursor's
monitor is what it settles on, and reproducing the upstream bug to match would be
wrong the day it is fixed.

Reaching the divergence needs the chain to get to step 4 — either
`WINIT_X11_SCALE_FACTOR=randr`, or no override *and* no XSETTINGS manager *and* no
`Xft.dpi` resource — **and**, either way, two CRTCs of different densities with the
cursor not on the first. `debt.md` carries it, with the note to re-read
`x11/window.rs` when a Slint bump moves winit.

(Both qualifications were missing from the first draft of this paragraph, which is
the fourth copy of a claim three review rounds had already retracted elsewhere.
It was written into `STATUS.md`, the orientation document, so it was also the copy
most likely to be read as unconditional.)

**Wayland is not a port of any of that, and cannot be.** There is no global cursor
position for a client to ask for, a client cannot position its own toplevel at all
(`set_outer_position` is a no-op on winit's Wayland backend), and a layer-shell
panel's exclusive zone is known only to the compositor. The Wayland answer is a
different mechanism — the screen coordinates the tray host passes to
`StatusNotifierItem.Activate(x, y)`, feeding a compositor-side positioner — which
is ADR-0010's seam and arrives with ksni.

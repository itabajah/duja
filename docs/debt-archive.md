# Refactor debt - drained

Rows that have been paid off, kept verbatim rather than deleted.

They are here because *how* a row drained is usually worth more than the row
was: several record a deferral reason that turned out to be false, a fix that
arrived larger than the row anticipated, or a remedy the row proposed that was
not the best one available. Deleting them would delete the only written record
that the reasoning was wrong.

Nothing here is outstanding work. Open rows are in [debt.md](debt.md).

**These seventeen carry `A-` ids, and nothing after them will.** They were
drained before ids existed, so there is no open-list id to preserve and the
`A-` numbering is archive-local. A row drained from now on arrives here keeping
the `D-` id it had in [debt.md](debt.md), which is the whole point of that file
refusing to renumber.

## Index

| # | Added | Where | What |
|---|---|---|---|
| [A-001](#a-001) | ~~P5~~ | `duja-ui` | ~~Theme "Auto" resolves to dark: no OS dark-mode query is exposed by the pinned winit/slint~~ — **drained by the macOS… |
| [A-002](#a-002) | ~~P6 (audit 2026-07-13)~~ | `.github/workflows/` | ~~Add `release.yml` (SHA256SUMS + build-provenance + minisign)~~ — **done in v0.1.0** |
| [A-003](#a-003) | ~~P6 (audit 2026-07-13)~~ | `duja-ui` / `duja-app` | ~~De-duplicate: `physical` (`dpi.rs`) ≈ `anchor_dim` (`positioning.rs`, named `physical_dim` until `#91`) are… |
| [A-004](#a-004) | ~~post-v0.1.5 (#87)~~ | `docs/adr/` | ~~No ADR records the tray-geometry coordinate contract~~ — **drained in `#91`**:… |
| [A-005](#a-005) | ~~post-v0.1.5 (#91 review)~~ | `duja-platform` `mac_geometry.rs` | ~~The cursor hit-test bias trades a top-edge defect for a bottom-edge one at 3×~~ — **drained in `#91` before merge,… |
| [A-006](#a-006) | ~~v0.1.1 (`#54`)~~ | `duja-app` `worker.rs`/`engine.rs` | ~~Honor `ddc_broken` in the tray app~~ — **delivered in v0.1.2 (#59)**: the worker now probes on open, so a… |
| [A-007](#a-007) | ~~v0.1.1 (deep review)~~ | `duja-platform` `ipc/unix_socket.rs` + `single_instance.rs` | ~~**Live on macOS since `#102`; Linux from P7.** The unix stale-socket takeover is a non-atomic unlink+rebind (two… |
| [A-008](#a-008) | ~~v0.1.2 (`#58`)~~ | `duja-app` `backend.rs` | ~~The DDC↔WMI dedup only catches *serial-bearing* duplicates; a *serial-less* internal panel derives different ids… |
| [A-009](#a-009) | ~~post-v0.1.5 (tray split, #81)~~ | `duja-app` `tray.rs` | ~~Three doc-comment attachment slips spotted during the module split~~ — **drained in the same PR after review**: the… |
| [A-010](#a-010) | ~~post-v0.1.5 (tray split review, #81)~~ | `duja-dimmer` `tests/windows_live.rs` | ~~`drop_shuts_down_and_removes_overlays` failed once (`left: 2, right: 1`); these tests race the window server~~ —… |
| [A-011](#a-011) | ~~post-v0.1.5 (#86)~~ | `duja-app` `motion.rs` | ~~`os_animations_enabled` returns a flat `true` on non-Windows, so once the flyout ships on macOS it overrides a user… |
| [A-012](#a-012) | ~~post-v0.1.5 (#86)~~ | `duja-app` `tray.rs` `os_dark_theme` | ~~Returns `None` unconditionally, so the UI's "Auto" theme resolves to dark everywhere~~ — **drained by the macOS… |
| [A-013](#a-013) | ~~post-v0.1.5 (#86)~~ | `duja-app` `backend.rs` | ~~The three `#[cfg(not(windows))]` hardware stubs — `discover_ddc` → `Vec::new()`, `open_ddc` → `None`,… |
| [A-014](#a-014) | ~~post-v0.1.5 (#90)~~ | `duja-app` `backend.rs` / `duja-panel` (macOS) | ~~A macOS built-in panel enters the app as `(id, None, None)` — the same shape as a Windows WMI panel — so… |
| [A-015](#a-015) | ~~post-v0.1.5 (#90, found while wiring)~~ | `dujactl` `backend.rs` | ~~`discover()` is `discover_ddc()` + `discover_panel()` with **no merge or dedup at all** and `open()` is… |
| [A-016](#a-016) | ~~post-v0.1.5 (#92, found in passing)~~ | `dujactl` `cli.rs`/`run.rs`/`fmt.rs` + `CONTRIBUTING.md` / `.github/ISSUE_TEMPLATE/monitor-quirk-report.yml` | ~~Both places tell users to run `dujactl doctor --report`, and there is no such flag: `parse` routes `doctor` through… |
| [A-017](#a-017) | ~~P6 (`#103` CI)~~ | `duja-app` `tests/engine.rs` | ~~**`worker_panic_does_not_kill_engine` is timing-flaky on the Windows CI lane.**~~ — **diagnosed at the P6 gate: a… |
| [D-091](#d-091) | ~~P7 wave 4b-5 (`#132`)~~ | `duja-platform` `geometry.rs` `cursor_anchor` + `linux/geometry.rs` | ~~`cursor_anchor()` can block indefinitely on X11, and it will run on the Slint main thread once the tray lands~~ - **drained in `#139`**, with a deadline around the whole probe rather than the `Stream` wrapper this row proposed |
| [D-098](#d-098) | ~~P7 wave 4 (`#124`), narrowed to X11 `#131`~~ | `duja-dimmer` `linux/gamma.rs` + `duja-app` `bin_support/gamma.rs` | ~~An X11 gamma ramp outlives the process and Linux has no crash guard for it~~ — **drained in `#136`**, in the same PR as the sink, exactly as its deferral note demanded |
| [D-101](#d-101) | ~~P7 wave 4 (`#124` review)~~ | `duja-ui` `ui/settings.slint` + `duja-app` `bin_support/settings.rs` | ~~The gamma hazard caption names macOS, and `gamma_is_advisory()` is now true on Linux too~~ - **drained in `#138`**: the `bool` that made the two platforms indistinguishable is now a kind |
| [D-011](#d-011) | ~~P5 / v0.1.0~~ | `duja-app` binary size | ~~`duja.exe` is **~19 MB** vs the ≤16 MB ADR-0012 budget~~ - **drained in P8 wave 1**: 15,709,696 bytes, within, and the budget is now enforced by `cargo xtask size`. Two of the three levers this row named were wrong |
| [D-002](#d-002) | ~~P2~~ | `.github/workflows/` | ~~Add `coverage.yml` (llvm-cov ≥90% gate) and `fuzz.yml` (weekly nightly burn) CI jobs~~ - **drained in P8 wave 2**, plus a third lane the row did not ask for |
| [D-023](#d-023) | ~~P6 (audit 2026-07-13)~~ | `fuzz/` | ~~Add the `fuzz_config_toml` target~~ - **drained in P8 wave 2**, landed with the workflow that runs it, exactly as the row asked |
| [D-108](#d-108) | ~~P7 gate~~ | `duja-app` `tray/state.rs` `begin_quit` | ~~Every clean quit writes identity gamma to every display, including ones Duja never touched~~ - **drained in P8 wave 4** |
| [D-114](#d-114) | ~~`v0.1.6` checkpoint~~ | `xtask` `size.rs`/`dist.rs` + `main.rs` | ~~Both subcommands take `std::env::Args`, a type no test can construct, so neither one's argument parsing is reachable by a test~~ - **drained in `#156`**, and the measurement disproved half of the row: `dist`'s parsing was already reachable, though not fully covered |
| [D-040](#d-040) | ~~v0.1.1 (deep review) -> narrowed in `#82`~~ | `duja-app` `tray/state.rs` | ~~The throttle-final-value contract is pinned at the `duja-ui` end and the engine end, and the app layer between them is unpinned~~ - **drained in `#157`**, by the `AppState` fixture the row said it needed, and proven red at both of its two sites |

## Rows

### A-001

**Where:** `duja-ui` &nbsp;·&nbsp; **Added:** ~~P5~~

~~Theme "Auto" resolves to dark: no OS dark-mode query is exposed by the pinned winit/slint~~ — **drained by the macOS OS-hook hoist**. The deferral reason was the stale half of the row: nothing ever required going through winit or Slint. (For accuracy: winit 0.30 *does* expose `Window::theme()`, but it needs a live window and Slint does not hand out the winit handle, so it is unreachable from where Duja resolves its palette — before either window exists.) `duja_platform::desktop::os_dark_theme` reads the OS directly on both platforms (`HKCU\…\Themes\Personalize\AppsUseLightTheme` on Windows, `AppleInterfaceStyle` on macOS)

**Why deferred.** Waiting on a Slint bump was waiting on the wrong thing; the query belongs in the platform crate either way

### A-002

**Where:** `.github/workflows/` &nbsp;·&nbsp; **Added:** ~~P6 (audit 2026-07-13)~~

~~Add `release.yml` (SHA256SUMS + build-provenance + minisign)~~ — **done in v0.1.0**. Still open: add `soak.yml` (nightly fake-backed soak)

**Why deferred.** `release.yml` shipped for v0.1.0; `soak.yml` folds into the P8 soak work

### A-003

**Where:** `duja-ui` / `duja-app` &nbsp;·&nbsp; **Added:** ~~P6 (audit 2026-07-13)~~

~~De-duplicate: `physical` (`dpi.rs`) ≈ `anchor_dim` (`positioning.rs`, named `physical_dim` until `#91`) are byte-identical logical→scaled-unit math across two crates~~ — **drained at the P6 refactor checkpoint**: both now delegate to `duja_core::scale::scale_extent` (`duja-core` is the only crate both depend on), with five tests including the `.max(1)` floor that a naive hoist drops — `logical.max(1.0)` alone is not enough, because a small honoured factor rounds a one-unit extent back to zero. **The two wrappers stay**, and that is the point: an anchor unit is physical pixels on Windows and X11 and points on macOS (ADR-0021) while `dpi`'s output is always physical pixels, so only the *arithmetic* is shared. Calling the core helper from the call sites would erase the distinction the ADR exists to keep explicit. ~~`clamp_pct` is duplicated in `shell.rs` and `settings_shell.rs`~~ — **also drained**: `settings_shell` now imports `shell::clamp_pct`. The first draft of this drain left it open on a claim that the two "are not the same function", citing the settings copy's note about the floor slider being "capped further to the view-model's max" — **review showed that was false**. The two were byte-identical in body *and* contract; that note is a cross-reference to `SettingsVm::set_monitor_floor` applying `MAX_FLOOR_PCT` **after** `clamp_pct` returns, and `clamp_pct` has no floor-slider awareness at all. An invented contract difference is a worse outcome than the duplication, so the row was finished instead

**Why deferred.** Both halves done. **Residue worth recording rather than draining blind**: the degenerate-scale guard (`is_finite() && >= 0.1 else 1.0`) still appears in four places — `duja_core::scale`, `positioning::flyout_height_cap`, `dpi`'s `Resized` arm, and `duja_platform::geometry::sane_scale`, which ADR-0021 §4 names as *the* canonical low-end guard both anchor factors route through. `duja-core` cannot depend on `duja-platform`, so unifying them is a real design question (which crate owns it), not a hoist

### A-004

**Where:** `docs/adr/` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#87)~~

~~No ADR records the tray-geometry coordinate contract~~ — **drained in `#91`**: [ADR-0021](adr/0021-tray-anchor-coordinate-contract.md) records it, and the open half it was waiting on is closed by `AnchorUnit` plus the two derived factors (`logical_to_anchor` × `anchor_to_physical` == the sanitised `scale`, on both variants)

**Why deferred.** The macOS backend that was supposed to close the question landed in the same PR, so the decision is whole rather than half. The part that still needs a Mac is *verification*, not design — see the mixed-DPI row below

### A-005

**Where:** `duja-platform` `mac_geometry.rs` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#91 review)~~

~~The cursor hit-test bias trades a top-edge defect for a bottom-edge one at 3×~~ — **drained in `#91` before merge, not deferred**: the row's own justification was unsound. It claimed narrowing `ε` "needs a Mac", but the derivation gives the safety condition as `0 < ε <= δ` (`δ = 1/backingScaleFactor`), so hardware could only ever reveal a *smaller* `δ` — which argues for a smaller `ε`, never a larger one. `0.5` was the **largest** admissible value; `ε = 0.25` satisfies the condition for every backing scale up to and including 4×, twice the density Apple ships. Verified by a brute-force sweep (7 layouts × steps 1.0/0.5/⅓/0.25 × every reachable reported row and column: `0.5` clean at 1×/2× and wrong at 3×/4×, `0.25` clean throughout) and pinned by `the_lowest_row_of_a_screen_still_belongs_to_that_screen`

**Why deferred.** Nothing was left to decide, so nothing was carried: the value is at a setting with 2× margin over shipping hardware, the remedy for a hypothetical denser display is to shrink the same one-line constant, and the condition is recorded on it. The sweep also surfaced a second, more common manifestation the row had missed — the probe falling outside **every** frame and taking the `Some(0)` primary fallback, which silently mis-routes a non-primary screen's lowest row even in a plain side-by-side layout — now covered by its own test

### A-006

**Where:** `duja-app` `worker.rs`/`engine.rs` &nbsp;·&nbsp; **Added:** ~~v0.1.1 (`#54`)~~

~~Honor `ddc_broken` in the tray app~~ — **delivered in v0.1.2 (#59)**: the worker now probes on open, so a `ddc_broken` monitor (empty caps ⇒ `hardware_range: false`) is downgraded to `SoftwareOnly` full-range software dimming instead of getting dead VCP 0x10 writes

**Why deferred.** Was a documented-deferred static-caps design; the no-hardware detection wave now covers it (probe-on-open + verify-first-write)

### A-007

**Where:** `duja-platform` `ipc/unix_socket.rs` + `single_instance.rs` &nbsp;·&nbsp; **Added:** ~~v0.1.1 (deep review)~~

~~**Live on macOS since `#102`; Linux from P7.** The unix stale-socket takeover is a non-atomic unlink+rebind (two concurrent starts → two live servers), and the `/tmp/duja-<uid>` fallback dir is squattable by another local user (`ensure_dir_0700` trusts a pre-existing dir; fails OPEN). Fix with `create_new` + fstat owner/mode + `O_NOFOLLOW`, or require `XDG_RUNTIME_DIR`~~ — **fixed in `#114`**, both halves, in the order this row prescribed. **The whole bind** runs under a sibling `flock`, not just the takeover: `UnixListener::bind` is `socket`/`bind`/`listen`, and an instance parked between the last two is indistinguishable from a stale inode to a concurrent probe, so locking only the takeover left the race narrowed rather than closed. Shutdown's unlink is conditioned on the same lock plus a `(dev, ino)` check, because a departing server would otherwise delete its *successor's* socket — the same "two servers, one unreachable" symptom reached with no takeover involved at all. **An inode number alone was not enough for that check, and the place it fails is the one this row already identifies as the Linux exposure.** ext4 and XFS recycle inode numbers, so once the departing server's fd is closed and the successor unlinks the old inode, the final `iput` reaches `ext4_free_inode`, the number returns to the block group's inode bitmap, and `ext4_new_inode` allocates the lowest free bit in it — which **can** be the number just recorded, making the comparison match on a coincidence. (Not *likely*: that bitmap spans a whole block group, typically 8192 inodes, not one directory. The guard has to be exact regardless.) tmpfs allocates from a monotonic counter (`get_next_ino`) and APFS assigns object ids monotonically, so `$XDG_RUNTIME_DIR` and macOS are immune; the `/tmp/duja-<uid>` fallback, the "routine cron/ssh/container condition" below, is usually ext4. The server therefore holds an **`O_PATH` descriptor** on the socket's inode until after the unlink: `ext4_free_inode` runs only from `ext4_evict_inode`, so a referenced inode cannot be freed and the successor's rebind is guaranteed a different number. The first attempt at this pinned the inode by dup'ing the **listening socket** instead, which works but keeps the socket *connectable* - and review round 3 showed that cost more than it bought. If both handler threads die the listener exits while the socket stays in `LISTEN`, so `dujactl` connects, blocks for the read timeout and exits with a server error, where before it got `ECONNREFUSED` and **fell back to driving the hardware directly**; and during shutdown a concurrent start is told "already listening", which `duja-app`'s IPC bridge does not retry. `O_PATH` references the inode and nothing else. It is Linux-only, which is where the problem is. The lock file is deliberately never unlinked: `flock` locks an inode, so unlink-and-recreate would let two processes lock two inodes and each believe it was exclusive. The directory check went to a shared `unix_dir` module (`O_NOFOLLOW | O_DIRECTORY` + fstat on the descriptor) used by **both** subsystems, which is what the row meant by "together". **Two corrections to this row.** (1) Only `single_instance` failed *open*. The socket half failed **closed**: `prepare_socket_dir` chmod'd the directory, which is `EPERM` on another uid's, so a squat stopped the server rather than being adopted by it. Its real hole was different and this row never named it — `set_mode` follows **symlinks**, so a symlink planted at the path redirected both the chmod and the bind. (2) `create_new` + fstat is the right shape, but refuse-on-mode is the wrong cut. The line is **writability**, not looseness. Write permission on a directory is permission to create, rename and unlink entries in it, so a group- or world-writable one may *already* hold an attacker's `ctl.sock` (and `PipeClient` performs no server-identity check, so `dujactl` would act on forged replies) or a lock file they hold a `flock` on; a late `chmod` does not undo any of it, so it is refused. Merely readable is repaired, because it grants only traverse and list against `0600` files — and it is what `create_dir_all` leaves under an ordinary umask, i.e. the state a caller that makes the directory first actually produces. Both wrong cuts were made before the right one: refusing every loose mode broke `stale_socket_is_taken_over`, and repairing every loose mode was unsound. Residual, a limit rather than an omission: the **foreign-owner** refusal is the one arm CI cannot execute, since it runs as a single user, so it is covered the way `peer_allowed` is — the pure decision unit-tested over a chosen grid of uid/mode pairs, the `stat` feeding it unverified. The concurrency test is likewise one-sided (a green run cannot prove a race absent), which is why the deterministic half is an assertion that the bind lock file exists at all

**Why deferred.** ~~Both are `#[cfg(unix)]` and unreachable on the shipping Windows build; fix before the P7 Linux port, together~~ — **the deferral reason is now false, re-triaged at the P6 gate.** `ipc/mod.rs` selects the unix socket on `cfg(unix)`, and `ipc/unix_socket.rs`'s own header now reads "macOS now, Linux in P7": the code is unchanged but it is **live on every macOS build**, so neither issue is hypothetical any more. `SECURITY.md` advertises "0600 unix socket in a 0700 dir" with no caveat. This must be fixed **before a macOS artifact ships**, not before P7 — and the release is held, so there is time. **The two halves are not equally live, and the order matters for whoever fixes them.** The **non-atomic takeover** (`unix_socket.rs`'s probe → `remove_file` → `bind`) is path-independent and therefore fully live on macOS: fix it first. The **squattable `/tmp`** half is *less* reachable on macOS than on Linux, not more — `socket_path` reads `XDG_RUNTIME_DIR` only under `cfg(not(target_os = "macos"))`; the macOS arm resolves `~/Library/Application Support/duja` and falls back to `/tmp/duja-<uid>` only when `HOME` is unset, which launchd and login always set. `single_instance.rs` goes further and uses `ProjectDirs::data_dir()` on macOS, degrading to "first" rather than touching `/tmp` at all — so `ensure_dir_0700` is not reachable via `/tmp` there. On Linux an unset `XDG_RUNTIME_DIR` is a routine cron/ssh/container condition, which is where that half actually bites. **All of this is now history — `#114` closed both halves as P7 wave 0, before any Linux code was written.** The re-triage above is what made that the first PR of the phase rather than a footnote inside it

### A-008

**Where:** `duja-app` `backend.rs` &nbsp;·&nbsp; **Added:** ~~v0.1.2 (`#58`)~~

~~The DDC↔WMI dedup only catches *serial-bearing* duplicates; a *serial-less* internal panel derives different ids across backends (`from_edid` vs `from_parts`) and would not dedup~~ — **reworked in v0.1.3 (#64)**: the merge no longer id-matches at all; a DDC-fallback internal panel is deduped by the *WMI-has-any-panel* signal (which also covers the serial-less divergence), and an internal panel WMI cannot see is now *surfaced* via DDC rather than skipped

**Why deferred.** Superseded — the id-hash divergence no longer blocks dedup. The remaining residual is the dual-internal-panel case (v0.1.3 row below)

### A-009

**Where:** `duja-app` `tray.rs` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (tray split, #81)~~

~~Three doc-comment attachment slips spotted during the module split~~ — **drained in the same PR after review**: the `ReentrantCell` explainer (the one describing the `0xe06d7363`→`0xc0000409` crash cure) now attaches to the struct instead of the intervening `type Deferred<T>`; `build_flyout`/`build_settings_window` got their opening lines back; and `with_app_ref` — which the `APP` doc already claimed existed — is now a real function, which also let `APP` drop out of `wiring.rs`'s imports so the thread-local is reachable from `tray.rs` alone

**Why deferred.** Fixed rather than deferred: the review pointed out that adding `with_app_ref` *hardens* the re-entrancy invariant (a submodule can no longer name `APP` and take a raw borrow), so it was worth doing immediately rather than carrying

### A-010

**Where:** `duja-dimmer` `tests/windows_live.rs` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (tray split review, #81)~~

~~`drop_shuts_down_and_removes_overlays` failed once (`left: 2, right: 1`); these tests race the window server~~ — **drained 2026-07-30, and the recorded cause was wrong.** Not a window-server race: two independent defects in the 20-line enumeration helper, both now fixed. (1) The walk used `FindWindowExW`, whose `hwndChildAfter` Microsoft documents as resuming "with the next child window in the **Z order**" — a Z-order-relative cursor over a class matched across *every* process. A concurrent overlay elsewhere (a real `duja.exe` while it is software-dimming) could therefore make the walk return one window **twice** (⇒ exactly the recorded `left: 2, right: 1`) *or* **truncate early** when the handle held as the cursor was destroyed mid-walk (⇒ an undercount, `left: 2, right: 3`). Both reproduced on demand by running 2-3 copies of the test binary concurrently. Replaced with an `EnumWindows` snapshot, which is what Microsoft recommends over a `GetWindow`/`FindWindowEx` loop for these two reasons, and which has no cursor to invalidate — so the dedup, a cycle guard and the truncation all cease to be needed. (2) Attribution is only ever per *process*, never per *dimmer*, so sibling tests miscounted each other under a shared-process harness: `cargo test -p duja-dimmer` failed most of its tests nondeterministically (counts varying with `--test-threads`; `alpha_change_round_trips` read a sibling's window, `left: 128, right: 64`). Fixed with a 9-line `static Mutex` gate taken first in each test (released after the dimmer drops, so overlays are gone before the lock frees). `cargo test` now passes 7/7 across repeated runs; nextest unchanged at 49

**Why deferred.** Both halves fixed rather than documented around, because `CONTRIBUTING.md` lists `cargo test --workspace` as the *first* developer workflow — so the red path was the documented one, and no doc change was needed once the tests were right. The gate costs nothing under nextest (one process per test ⇒ uncontended). A pre-spawn baseline diff was implemented first, measured as still-failing (a sibling's window can appear after the snapshot) and reverted rather than left in place implying a robustness it did not deliver. Honest limit: the *specific* historical run was never re-obtained, so (1) is the mechanism the code permitted plus a reproduction of that mechanism, not a post-mortem of that run

### A-011

**Where:** `duja-app` `motion.rs` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#86)~~

~~`os_animations_enabled` returns a flat `true` on non-Windows, so once the flyout ships on macOS it overrides a user who explicitly asked the system to reduce motion — an accessibility regression~~ — **drained by the macOS OS-hook hoist**. `duja_platform::desktop::animations_enabled` answers it with `!NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()`, and `motion.rs` keeps only the pure glide policy

**Why deferred.** The deferral reason — "reaching `NSWorkspace` means adding `AppKit` to `duja-app`, which belongs to the macOS app assembly rather than being smuggled in ahead of it" — was sound and was dissolved rather than overruled: the query went into `duja-platform`, which already had `AppKit`, so `duja-app` gained no dependency at all. The sibling `os_dark_theme` row above went the same way for the same reason

### A-012

**Where:** `duja-app` `tray.rs` `os_dark_theme` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#86)~~

~~Returns `None` unconditionally, so the UI's "Auto" theme resolves to dark everywhere~~ — **drained by the macOS OS-hook hoist**, and not the way this row predicted. It assumed the fix had to wait for the tray to be un-gated, because `os_dark_theme` lived *inside* the `cfg(windows)` tray module. Moving the query into `duja_platform::desktop` removed that coupling entirely, so both halves landed before the un-gating rather than after it. The macOS side also does **not** use `NSApp.effectiveAppearance` as this row proposed: that needs a live `NSApplication`, and the call happens during startup before Slint's winit backend builds one, so touching `sharedApplication` there would create the app object out from under winit. `NSUserDefaults`' `AppleInterfaceStyle` answers the same question with no such requirement

**Why deferred.** Nothing carried

### A-013

**Where:** `duja-app` `backend.rs` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#86)~~

~~The three `#[cfg(not(windows))]` hardware stubs — `discover_ddc` → `Vec::new()`, `open_ddc` → `None`, `open_panel_controller` → `None`~~ — **drained in `#90`**: `duja-app` and `dujactl` now open real macOS displays. `discover_ddc` got a macOS arm (kind always `ExternalDdc` — the mac backend filters built-ins with `CGDisplayIsBuiltin`, so there is no internal-DDC fallback carrier — and the display-surface token is the decimal `CGDirectDisplayID`); `open_ddc` and `open_panel_controller` became single `#[cfg(any(windows, target_os = "macos"))]` definitions (the Windows bodies compiled verbatim, as predicted), with the stubs narrowed to `not(any(windows, target_os = "macos"))`. So the `--once` inaccuracy is gone: a `hardware_range: true` panel row on macOS now has an opener behind it. `DisplayGeom` is re-documented (element 2's unit is physical pixels on Windows, **points** on macOS; element 3 is an opaque platform surface token with two invariants and two consumers), and `merge_displays`/`open_controller` say "the panel backend" instead of "WMI"

**Why deferred.** Was never dependency-blocked, only unwired; the residuals (never-executed-on-hardware paths, no macOS gamma sink, no shared-surface identity in the macOS token) are the row below

### A-014

**Where:** `duja-app` `backend.rs` / `duja-panel` (macOS) &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#90)~~

~~A macOS built-in panel enters the app as `(id, None, None)` — the same shape as a Windows WMI panel — so `dimming::plan` emits **no** `DimCommand` for it and it cannot be software-dimmed at all~~ — **fixed in `#105`**, by the route this row asked for. `duja-panel` gained a public `PanelGeometry` (bounds + the two dimming tokens) reported at `enumerate` time, `Some` on macOS and `None` on Windows, so the app folds it in without ever interpreting `instance_name` and without a line of display FFI. Three things had to move together, and the second is the one a narrower fix would have got wrong. **(1) Bounds** — `CGDisplayBounds`, in points, so the planner emits a `DimCommand` and the overlay covers the panel. **(2) The surface token** — bounds *alone* would have been a regression, not a fix: a `MacBook` mirroring its screen to a projector would then have had the panel (a `None`-token singleton) and the monitor at identical bounds in two groups, i.e. two overlays stacked on one framebuffer, which is `#66` reached from the other side. **(3) The gamma token** — the panel's own id, so a gamma-mode panel is addressed as itself rather than left with `overlay_alpha == 0` and nothing driving it. The surface rule and the `CGRect`→`DisplayBounds` conversion moved to a new pure `duja_core::macos` — not for tidiness: `duja-ddc` computes the external clone's token and `duja-panel` the master's, the app compares the two strings, and two copies of a rule that must agree exactly is the defect that arrangement invites. Both are FFI-free and tested on every lane

**Why deferred.** Residuals, all of them hardware-blind rather than unfinished. **(a)** The two new CoreGraphics reads have never run — the same gap as every other macOS row here, and CI's runners cannot close it. **(b)** The mirror case is where being wrong would show, and it is worth being precise about *how*. A macOS panel could previously never be merged away; now it joins a group, and — because `build_group` anchors on the lowest id string and an Apple panel decodes to `APP-…` (or the `AAP-…` sentinel), which sorts ahead of DEL/GSM/LEN/MSI/SAM/VSC and nearly every other PNP id — it will usually be the **anchor**. `plan_commands` takes the group's bounds from the anchor, so in a `MacBook`-to-projector mirror the merged set's single overlay is now placed from the *panel's* `CGDisplayBounds`, read from `CGGetOnlineDisplayList` (which `duja-ddc` never consults). If a hardware mirror reports a built-in rect that is not the shared mirror rect, the one overlay covers the wrong region on both screens. Non-anchor is the other half and is milder: `#66`'s open residual leaves that member's settings row inert. Both need the same Mac as the macOS-surface-token row below, whose premise this now shares and widens. Partly bounded already: a rect that is not finite or encloses no area — `CGRectNull`, what CoreGraphics answers for a display it considers invalid, and `CGRectInfinite`, whose `u32::MAX` extents would otherwise ask for a window covering the coordinate space — is refused at the source, `panel_geometry` returning `None` rather than a placeless overlay. So the failure mode left is a plausible-but-*wrong* rect, not a nonsense one, and a panel that reports none is simply un-dimmable below its floor, exactly as it was before this change. **(c)** Gamma on the built-in panel is reachable for the first time, so the macOS-gamma-sink row's advisory caveat now applies to the panel too (in practice an EDR panel refuses gamma before that matters) — and so does `MacSink::restore`, which writes a **linear identity** table rather than restoring the display's profile, on the one display whose factory calibration a user is most likely to notice. **(d)** A software-only member now pins the panel's backlight to 100 %: `fan_out_hardware` drives every hardware-capable member of a software-only group to MAX so the shared overlay is the sole dimmer, which on a laptop mirroring to a software-only projector means full backlight plus an overlay — correct by the group rule, and a battery and thermal cost that did not exist while the panel was always a singleton. **(e)** The Windows half is untouched and stays open in the v0.1.2 pure-WMI row above: WMI genuinely exposes no rectangle, which is what makes it a different problem rather than the same one

### A-015

**Where:** `dujactl` `backend.rs` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#90, found while wiring)~~

~~`discover()` is `discover_ddc()` + `discover_panel()` with **no merge or dedup at all** and `open()` is **DDC-first**, so on a laptop whose panel WMI can drive the built-in panel is listed **twice**, the DDC copy is labelled `ExternalDdc`, a **serial-bearing** panel gets stamped `-slot0`/`-slot1` (so `dujactl set …-slot1` resolves to nothing and fails outright while `-slot0` writes VCP 0x10 over eDP), and `doctor`'s `ddc_count()` reports it as an external monitor~~ — **fixed in `#92`**: the CLI now mirrors the app's policy rule for rule. `map_ddc_display` is the one place that classifies a DDC entry, from `is_internal` on Windows and from a hard-coded `false` on macOS (whose backend filters built-ins with `CGDisplayIsBuiltin` and carries no such field); `merge_displays` drops an internal DDC entry only when the panel backend listed **any** panel (presence, not an id match, so the serial-less id divergence is covered) and always keeps externals and the panel rows; `merge_and_resolve_slots` runs `assign_twin_slots` on the **merged** list, so a deduplicated panel keeps the bare id `open` can actually resolve; `open` is panel-first; and `doctor`'s two counts (`external_count`/`internal_count`, relabelled `external monitors` / `internal panels`) are derived from that one merged set, so the summary can no longer disagree with the listing below it. Nine tests pin the policy. **Six of the nine were proven red** with the two historical bodies restored in place — 3 by the un-merged `discover()` and 6 by the hardcoded kind, overlapping — plus the merge-survivor choice, which a mutation keeping the DDC row instead of the native panel row reds (it did **not** before review: the assertion compared `kind`, and both candidate rows are `InternalPanel`, so only `name` discriminates them). The remaining **three are non-regression guards that were already green** against the shipped code and are not evidence of anything: `genuine_identical_external_twins_are_still_slotted`, `external_ddc_display_always_survives_regardless_of_the_panel_backend`, `ddc_display_without_a_name_falls_back_to_a_dash`. See `#92`'s body for every `left:`/`right:`

**Why deferred.** Was pure divergence from the app rather than a design gap — recorded in `#90` instead of fixed there because each consequence deserved its own red-first test and the fix is a real Windows behaviour change, not a macOS wiring detail. That is now done. The residuals are the row below

### A-016

**Where:** `dujactl` `cli.rs`/`run.rs`/`fmt.rs` + `CONTRIBUTING.md` / `.github/ISSUE_TEMPLATE/monitor-quirk-report.yml` &nbsp;·&nbsp; **Added:** ~~post-v0.1.5 (#92, found in passing)~~

~~Both places tell users to run `dujactl doctor --report`, and there is no such flag: `parse` routes `doctor` through `end()`, which rejects any trailing argument, so the documented command exits `EXIT_USAGE` with ``unexpected argument `--report` ``. Either accept and ignore `--report`, or add the machine-readable report it implies, or fix both docs~~ — **drained in `#95`**, and the product question this row posed is answered: **option 2, add the report the flag implies**. Not option 1 (accept and ignore) — the template's own preamble promises the paste "contains the monitor identity and **probed capabilities**", and `run::doctor` called neither `probe()` nor `get()`, so an accepted-but-inert flag would have shipped a `doctor --report` that is *strictly less informative about the monitor than `dujactl list`*, and left four of the six symptoms its **required** dropdown offers (capability string wrong/missing, wrong or lying brightness range, input-source switching broken, brightness commands ignored) with no supporting evidence anywhere in its **required** textarea. Not option 3 (fix the docs) for that same reason plus one more: `CONTRIBUTING.md` calls this "the most valuable non-code contribution", so the answer to "the command does not exist" is to build it, not to stop asking for it. `--report` now opens each display and prints what the hardware answered — `Capabilities.raw_capabilities` (the raw MCCS string), the probed feature set, the live `current/max` with the `hardware_range` verdict, and `allowed_inputs` — while a probe **failure** is printed as a finding and still exits 0, because "enumerates but DDC is dead" was byte-identical to a healthy monitor in this output. Plain `doctor` stays probe-free (0 DDC reads, ~30 ms measured; `--report` is ~1.2 s for one monitor) and gains only two free lines: the `dujactl: <version> (<os> <arch>)` identity header — unconditional, since `README.md`, `docs/qa-checklist.md` and `docs/STATUS.md` all cite *plain* `doctor` as the diagnostic — and any quirk `notes`, which makes ADR-0007's "the `dujactl doctor` report can cite accumulated quirk `notes`" true for the first time (`ResolvedQuirks.notes` had accumulated them since P2 with nothing rendering them; the shipped `MSI-30B6` entry carries one, so it prints on the dev box today)

**Why deferred.** Fixed rather than carried: the docs were not wrong about what a reporter needs, only about what existed. The residual is a seam, and a narrower one than the `#92` row above: `doctor`'s assembly is now the pure `run::doctor_lines(report, reachable, displays, probe)` with the probe **injected**, so "`--report` probes, plain `doctor` does not" is asserted by *counting calls* rather than inferred from text — three mutations proven red, including the `let _ = report;` deletion that left 913/913 green before. What is still hardware-only is `probe_display`'s own `backend::open`→`probe`→`get` shell, exactly like the `discover_*` shells in the row above and for the same reason (the CLI has no fake-backend infrastructure). Exercised against the real MSI MP273QP with the tray app running: 6/6 clean runs, no contention with the engine's own writes

### A-017

**Where:** `duja-app` `tests/engine.rs` &nbsp;·&nbsp; **Added:** ~~P6 (`#103` CI)~~

~~**`worker_panic_does_not_kill_engine` is timing-flaky on the Windows CI lane.**~~ — **diagnosed at the P6 gate: a test artifact, not a dropped notification.** The row demanded the distinction be *established* rather than papered over with a bigger constant, and it now is. Evidence: **3,014 runs, 0 failures** across four harnesses (50 under nextest; 2,000 direct at 8-way; 960 with `RUST_BACKTRACE=1` at 24-way against 32 CPU burners on 16 cores; 4 full-workspace runs at `--test-threads 32` under load). One measurable cost sits inside the budget: Rust's default panic hook runs **at the panic site**, inside `catch_unwind` in `worker.rs` and *before* the `Panicked` ack is sent, so the budget absorbs a dbghelp symbolization of the test binary's PDB — ~30 ms idle, ~85 ms with `RUST_BACKTRACE=1` (which `ci.yml` sets), 184–201 ms under 32-way load. **It is nowhere near large enough to be the cause, and this row is the record of that**: across **four** red CI runs the failing waits took 2.804 s (run 30504679809, `04a074f`, 2026-07-30 — the one this row originally missed, and the only one never re-run), 10.266 s (30597183871), 6.589 s (30620730412) and 2.850 s (30700482072). Two orders of magnitude are missing, so anyone reaching for the hook as *the fix* is reading this row wrong — see the fourth-run entry below, where exactly that mistake was made and caught in review. The two 2026-07-31 jobs (30597183871 and 30620730412) also failed `install-action`'s bash startup, corroborating a sick runner, and both were green on a re-run of the identical commit. Nothing in production depends on 2 s: the real backstop is `watchdog_timeout` = 5 s, and `.config/nextest.toml` now carries a hang guard

**Why deferred.** **Both halves are now done (`#113`), and the fourth red run is why.** Run 30700482072 failed this test at 2.850 s on `f96e4bd` — a **docs-only** commit. Re-running the identical commit turned it green at **0.069 s**, the same outcome as the two earlier re-run reds. (Three of the four reds were re-run and all three went green; 30504679809 never was. That matters: it is the only one of the four whose outcome could have contradicted the environmental reading, and at 2.804 s it is the near-twin of this one — two failures at ~2.8 s is a tighter signature than the 6.6/10.3 s outliers, and this row missed it until review caught the miscount.) What makes this one decisive is a test with **no panic in it at all**: `loop_time_assembly`'s zero-duration single-shot went 0.363 s green → **4.308 s** red → 0.777 s on the re-run, while the median test in the red run was 1.04x its green time. A transient stall that hits an unrelated event-loop test twelvefold cannot be bounded by anything on the panic path. (One prior corroboration did *not* recur: this job's `install-action` succeeded, so "sick runner" rests on the re-run and the cross-test pattern rather than on a second failing step.) Done: `LIVENESS_BUDGET` (10 s) now names **every positive wait** in `tests/engine.rs` — the 53 that were on a 2 s budget, plus `drain_writes`' shared 3 s and the ~20 stragglers at 1 s, 3 s, 4 s, 5 s and 8 s. Those last ones matter and were nearly left behind: several sat *below* the 4.3 s stall the constant exists to tolerate, so "fixing" only the 2 s sites would have left the file's next-most-fragile waits more exposed than the one that failed. The **14** negative waits — which elapse in full every run — deliberately keep their own short literals (500-700 ms, 1 s, one at 2 s), each with a note. The no-op hook is in too, but as a **cleanup worth ~50 ms, not the fix**, and it keeps the panic's message and location so the next failure still shows that a worker panicked. Recorded because the first draft of `#113` got this backwards and review blocked it: it claimed `RUST_BACKTRACE` was "the single environmental difference", which **this row already refuted** — 960 of the 3,014 control runs had it set and passed, and five green Windows CI runs paid the same symbolization at 0.057–0.184 s. `std::panic::set_hook` is still **process-global**, which is why the installer is prefix-scoped, `Once`-guarded and documented as a one-way door

### D-098

**Where:** `duja-dimmer` `linux/gamma.rs` + `duja-app` `bin_support/gamma.rs` &nbsp;·&nbsp; **Added:** ~~P7 wave 4 (`#124`), **narrowed to X11** `#131`~~

~~**An X11 gamma ramp outlives the process and Linux has no crash guard for it.**~~ — **drained in `#136`**, in the same PR as the sink, which is what its own deferral note demanded. *(X11 only, and that is now established rather than assumed: a `zwlr_gamma_control_v1` dim lives exactly as long as the client's object, the compositor destroys every object a client holds when its socket closes, and destroying drops this client's colour transform - so the guarantee survives `SIGKILL` and a Wayland session cannot be left dark by a crash at all. (It restores the output's *default*, not some earlier client's curve: `gamma_control_destroy` emits `set_gamma` with no control attached and wlroots stores no previous table. This row said "restores the original table" through two revisions; the third caught it.) `#131` built that channel and checked the property against wlroots' `types/wlr_gamma_control_v1.c` rather than the protocol's prose alone.)* The X server holds each CRTC's table as server state and does not reset it when the writing client disconnects — which is why `xrandr --gamma` works as a one-shot command — so an **X11 session** sits with Windows, not with macOS, and needs the same machinery: a marker file written before the first engage, an RAII guard that restores identity on drop including a panic unwind, and `startup::recover_from_crash_marker` to undo a dirty exit on the next launch. All three now exist. `mark_if_needed` writes the marker **before** the OS write and only after correlation succeeded, which is the Windows guard's rule and has a second reason here: on X11 an `Err` from `set_gamma` does not prove the ramp is not live, because the write is confirmed with a round trip and a connection that dies in between reports a failure for a table that is on the screen and stays there. `impl Drop for LinuxSink` restores what the sink engaged, including on a panic unwind, and deliberately not the whole screen — a `Drop` is not a rescue and must not flatten a colour-temperature tool's ramp on the way out. `startup::recover_from_crash_marker` fires on Linux because `tray.rs` is no longer gated away from it. `duja --restore` is still there and is now the *second* line rather than the only one

**Why deferred, and how it drained.** The deferral was deliberate, and the reason was that **nothing engaged a ramp on Linux yet**: the engage path is the app's gamma sink, which only the tray constructs, and the tray is not built on Linux until the ksni wave. A guard added now would have no caller, and its tests would pin a lifecycle nothing drives — the dead-code shape this crate has already been burnt by (the P4 gate found `dim_mode = "gamma"` was a silent no-op for exactly that reason). Lands with the Linux sink, in the same PR, or the sink ships without a net.

**It did, and two things about the delivery are worth keeping.** The marker is written on **both** transports even though only X11 needs one, which this row's own narrowing argues against: the reason is the drift case [D-096](debt.md#d-096) describes, where a process engages X11 ramps and *then* acquires a `WAYLAND_DISPLAY` — a transport check at engage time writes no marker for exactly that run, and it is the run that leaves CRTCs dark permanently. It costs a Wayland-only session nothing measurable, because the next launch's `restore_all` opens no connection on either channel.

And the three functions this needed were not Linux-shaped at all: `mark_dirty`, `clear_marker` and `marker_present` were sitting in `win::gamma` behind a `cfg(windows)`, which framed the marker as a Windows idea. It is an idea about **ramps that outlive the process**, and this row is the record that X11 has those too. They moved to an unconditional `duja_dimmer::marker`, and their idempotence test moved off the Windows lane onto all three

### D-101

**Where:** `duja-ui` `ui/settings.slint` + `duja-app` `bin_support/settings.rs` &nbsp;·&nbsp; **Added:** ~~P7 wave 4 (`#124` review)~~

~~**The gamma hazard caption names macOS, and `gamma_is_advisory()` is now true on Linux too.**~~ - **drained in `#138`**, the wave after the one that made it reachable, and before wave 6 tags anything. `#103` added the caption for one platform and worded it for that platform: *"macOS can accept a ramp and not apply it — on some recent Macs regardless of settings, and on others while 'Automatically adjust brightness' is on"*. `#124` made the predicate true on X11 for a **different and sharper** mechanism: `ProcRRSetCrtcGamma` discards `RRCrtcGammaSet`'s return, which is the driver hook's own result, and answers `Success` regardless — so there is no setting to blame and no hardware subset, it is every X11 write. A Linux user would be shown macOS copy about a Mac feature they do not have

**Why deferred.** It was not reachable, which is why it was a row and not a fix: `platform_gamma_limits()` has one caller, `tray/state.rs`, and `mod tray` was `cfg(any(windows, target_os = "macos"))`. **`#136` un-gated it, so this is now a live defect** rather than a pending one - a Linux user opening the settings window is shown macOS copy about a Mac feature they do not have. Not shipped: Linux has no release until `v0.3.0` (**superseded**: [ADR-0024](adr/0024-preview-artifacts-on-the-patch-train.md) shipped the Linux tarball as a preview at `v0.1.6`, and re-mapped `v0.3.0` to mean hardware-confirmed), and wave 6 is what makes one. So this is owed **before wave 6 tags anything**, and it is deliberately not folded into the un-gate PR that revealed it, which changed `mod tray`'s gate and a hundred other things. The fix is for the caption to select its text on the platform (or to state the shared fact — an accepted gamma write is not proof of a dimmed screen — and drop the per-OS cause). Recorded because the failure mode is silent: the code is correct, the string is wrong, and nothing tests a translated caption's *content*

**How it drained, and the part that is not the fix.** The row offered two
remedies - select the text per platform, or state the shared fact and drop the
per-OS cause - and the first is what landed, for a reason the row did not have:
**the underlying fact is identical on both platforms**. A write reported as
accepted, not applied, not detectable by reading it back. So a shared sentence
would have been correct, and it would have cost the one clause a user can *act*
on: macOS names "Automatically adjust brightness", a setting they can turn off.
Linux has nothing equivalent, because `ProcRRSetCrtcGamma`'s behaviour is
unconditional. Two sentences, one shared opening, different second halves.

`GammaLimits::advisory` went from `bool` to a three-variant `GammaAdvisory`, and
that type change is the actual fix rather than the strings. Under the `bool`,
macOS and Linux carried the **same value**, so no fixture could tell them apart
and no assertion could fail - which is why this shipped through a whole wave with
a green suite. `duja-dimmer` gained `gamma_advisory()` beside `gamma_is_advisory()`
(joined by `the_two_advisory_answers_cannot_drift`, because two `cfg` ladders that
must agree is exactly the shape that stops agreeing), `duja-ui` carries its own
copy of the enum because it depends on neither dimmer crate, and `duja-app` maps
between them in one exhaustive `match` with no `_` arm - so a fourth variant
fails to compile rather than silently rendering as "nothing to disclose".

The new test reads the **rendered** caption and asserts on distinguishing
substrings rather than whole strings: "macOS" must never appear on the Linux
caption whatever language it is in, and a test pinned to exact wording would go
red on a copy edit, get relaxed, and stop guarding anything. Proven red by
reinstating the defect where it historically sat - one `@tr` string behind
`gamma-advisory-kind != 0`.

**Still true, and worth carrying forward**: nothing tests a translated caption's
content. This test would pass against a translation that said the wrong thing in
any language it does not know the words for

### D-091

**Where:** `duja-platform` `geometry.rs` `cursor_anchor` + `linux/geometry.rs` &nbsp;·&nbsp; **Added:** ~~P7 wave 4b-5 (`#132`)~~

~~**`cursor_anchor()` can block indefinitely on X11, and it will run on the Slint main thread once the tray lands.**~~ - **drained in `#139`**, one PR after the wave that made the second half of that sentence true. Its doc said "never fails and never blocks" - true while Windows and macOS were the only backends, both of which are local syscalls. The X11 one opens a connection (a TCP connect when `DISPLAY` names a remote server), makes several round trips, and reads X resource files through `resource_manager` - at most two of `.Xresources`, `.Xdefaults` and either `$XENVIRONMENT` or `.Xdefaults-<hostname>`, and then however many those pull in, since the parser follows `#include` a hundred levels deep. x11rb 0.13.2 sets no connect or read timeout anywhere, so a hung or wedged X server hangs the caller - and so does a `$HOME` on an unresponsive network mount, with no X server involved at all. (An earlier revision of this row said "up to four files under `$HOME`", which is wrong in both directions and was corrected in `geometry.rs` while this copy stood.) The sentence is corrected; the exposure is not

**Why deferred.** Closable, at a price, and the price is the argument rather than impossibility - an earlier revision of this row said x11rb "offers no timeout knob" and enumerated only a worker thread or an `alarm`/`SIGALRM` dance. It has one: `rust_connection::Stream` is a public trait, `PollMode` is public, and `RustConnection::connect_to_stream` is generic over it, so a wrapper around `DefaultStream` whose `poll` carries a deadline bounds every wait that goes through the stream - round trips, flushes, and the setup handshake - with no extra thread and no signals. **Not the socket connect**, which is the one this row's problem column names first: `DefaultStream::connect` calls `TcpStream::connect` or `UnixStream::connect` before any `Stream` exists to wrap. The remote case wants `TcpStream::connect_timeout`; the local one - every ordinary session - has no std timeout at all and needs a non-blocking socket and a poll loop. What that costs is display-string parsing and the xauth lookup `x11rb::connect` does for free, plus a generic parameter through this module's helpers. (`SO_RCVTIMEO` is the obvious first guess and does not work: the block is in `poll(2)`, not `read`, so a socket timeout there just spins.) The worker-thread alternative remains worse than it looks - it leaks the blocked thread rather than cancelling it, since nothing cancels a blocked `connect`. The honest framings are that this is the exposure every X client has, that Duja's other X paths (`duja-dimmer`'s probe, the overlay, `--restore`) already carry it unbounded, and that a wedged X server means the user has no working desktop to open a flyout on. What would change the calculus is the tray: a hang there freezes the UI thread rather than one CLI invocation. **That is now the case** - P7 wave 5 (`#136`) un-gated the tray on Linux, and `geometry::cursor_anchor` runs on the Slint main thread on every flyout open. This row was written to be re-read at exactly this point, so: the calculus has changed and the exposure is live, which promotes it from "the exposure every X client has" to "one unresponsive `$HOME` mount freezes the whole app". Consider caching the resource database first - the file reads are the part with no protocol excuse, and they are the half that hangs with no X server involved

**How it drained, and why not the way this row proposed.** The row's remedy was a
deadline-carrying `rust_connection::Stream` wrapper, and it was a real knob -
`Stream` is a public trait and `connect_to_stream` is generic over it. It is also
the wrong fix, by this row's *own* problem column: it bounds only what goes
through the stream, and **neither** hazard named at the top does. The socket
connect happens in `DefaultStream::connect`, before any `Stream` exists to wrap -
the row says so itself, in bold, and then proposes the wrapper anyway. And the
resource-database file reads never touch the server at all, so no protocol-level
timeout can see them.

What landed instead is a deadline around the **whole** probe:
`linux_deadline::probe_within` runs it on a worker thread and gives up after 250
ms, and `cursor_anchor` falls back exactly as it already did for every other
failure. That bounds all three failure sites - connect, round trips, file reads -
in a fraction of the code, and it names no x11rb type, so its timeout, latch and
panic-unwind behaviour are tested on **all three lanes** rather than on none.
Three mutations proven red: removing the `Drop` guard, removing the latch, and
removing the deadline itself.

The row's closing suggestion - cache the resource database first - was also not
taken, and for a reason worth keeping: a cache removes the file reads from the
*second* call onward and leaves the first one, still on the main thread, exactly
as exposed. Cheapness was never the problem.

**What it does not close is now [D-106](debt.md#d-106)**: every other X path in
the tree is still unbounded, and a timed-out probe parks a thread that nothing
can cancel

### D-011

**Where:** `duja-app` binary size &nbsp;·&nbsp; **Added:** P5 / v0.1.0 &nbsp;·&nbsp; **Drained:** P8 wave 1

~~`duja.exe` is **~19 MB** vs the ≤16 MB ADR-0012 budget: the ureq/rustls/ring/webpki-roots update stack (+1.2 MB) **plus** the WinRT toast bindings the v0.1.0 smart update loop added (`UI_Notifications`/`Data_Xml_Dom`/`Foundation*`). Levers: fat LTO (−1.0 MB measured), feature-gate the update stack (network + toast) behind a default-on feature so a "lite" build drops both, drop `tracing-subscriber`'s `env-filter` regex~~

~~**Why deferred.** P8 hardening owns binary trimming; RAM and wakeup budgets still pass with headroom~~

**How it drained, and what it got wrong.** `duja.exe` is **15,709,696 bytes**, within the 16 MiB budget with 1,067,520 to spare, and the budget is now checked by `cargo xtask size` in the release workflow instead of being remembered. The full measured ledger is [ADR-0012](adr/0012-binary-size-budget-variance.md)'s P8 section.

Two of the three levers this row named were wrong, which is the part worth keeping:

- **"Feature-gate the update stack (network + toast) behind a default-on feature so a 'lite' build drops both."** Gating something that is on by default saves the default build *nothing*. It creates the possibility of a smaller artifact nobody currently publishes, so as a lever against this budget it was worth zero bytes. Not taken.
- **"Drop `tracing-subscriber`'s `env-filter` regex."** Correct, and worth more than it looked: 345 KiB of `.text` by `cargo bloat`, but 664,064 bytes off the file, because a crate leaves with its read-only data. It also was not free the way the row implies - `filter::Targets` differs from `EnvFilter` in three ways, two of which had to be repaired in `bin_support::logging` and one of which (an empty `RUST_LOG=` silencing the log) would have been a silent regression.
- **"Fat LTO (-1.0 MB measured)."** Correct, and -1,098,240 on re-measurement.

What actually reached the budget was none of them: `opt-level = "s"` with the frame path exempted, -3,277,312 against `opt-level = 3`. This row never mentioned the profile's optimization level, and its framing - a list of dependencies to remove - is why: the binary is **data-bound**, not code-bound. 36 % of it is `.rdata`, dominated by ICU and font tables welded to a Slint feature a desktop GUI cannot turn off, and no amount of removing crates from the list above could have reached it

### D-002

**Where:** `.github/workflows/` &nbsp;·&nbsp; **Added:** P2 &nbsp;·&nbsp; **Drained:** P8 wave 2

~~Add `coverage.yml` (llvm-cov ≥90% gate) and `fuzz.yml` (weekly nightly burn) CI jobs~~

~~**Why deferred.** Ran locally at the P2 gate; wire into CI in a P8 hardening pass~~

**How it drained.** Both workflows, plus a third lane this row did not think to ask for.

`fuzz.yml` runs all six targets on a nightly toolchain every Sunday and uploads the crashing input on failure. `coverage.yml` runs `cargo llvm-cov` and enforces the three floors [review-rubric.md](review-rubric.md) has been asking for since P2 - core >= 90 %, ipc and view-models >= 85 % - which measured 97.49 %, 94.66 % and 98.8 % on the dev box, so the floors were set from the rubric rather than ratcheted to today's number.

**The third lane is the one worth recording, because the row's framing would have missed it.** A weekly burn only runs code that still compiles, and `fuzz/` is a *separate Cargo workspace*: nothing in the PR matrix touches it, so a rename in `duja-core` breaks a target silently and the only thing that notices is a scheduled run the following Sunday, attributed to whatever merged since. So `cargo check --manifest-path fuzz/Cargo.toml --locked` is now a step in the **existing** `clippy (ubuntu-latest)` job. `--locked` because `fuzz/Cargo.lock` had itself drifted - it still named `duja-core 0.0.1` - which is the same class of rot one directory further down. Inside a required check it is enforced from the first PR; as a job of its own it would have been a new status context, and a new context is advisory until somebody edits branch protection.

**What is honestly not enforced.** `coverage.yml` is exactly that new-context case and is therefore advisory today. It is a workflow rather than a step because `cargo llvm-cov` rebuilds and re-runs the whole workspace instrumented, which would roughly triple the wall-clock of a check every PR waits on. Making it required is a repository setting, and the workflow's own header says so instead of implying enforcement it does not have

### D-023

**Where:** `fuzz/` &nbsp;·&nbsp; **Added:** P6 (audit 2026-07-13) &nbsp;·&nbsp; **Drained:** P8 wave 2

~~Add the `fuzz_config_toml` target (plan §4 names it) — `config.toml` is user-editable and parsed through chained `toml_edit` migrations, an untrusted-parse surface currently without fuzz coverage (caps/edid/quirks/ipc/ddc are covered)~~

~~**Why deferred.** Low marginal value until `fuzz.yml` runs targets in CI (also deferred, see the coverage/fuzz row above); add both together in the P8 hardening pass~~

**How it drained, and the one thing it under-specified.** Landed with `fuzz.yml`, which is what its deferral note asked for.

The row describes the surface as "parsed through chained `toml_edit` migrations", and a target that drove only `ConfigDocument::parse` would have covered none of that - `parse` is TOML syntax and nothing else, and the migration chain lives behind `load`, which takes a path. So the target drives three stages by hand: `parse`, then `config()` for the serde deserialize, then `migrate` **from every version a file could claim** rather than from the version the document declares. Reading the declared version would have let the fuzzer trivially avoid multi-step chains by always claiming to be current, which is exactly the path a corrupted file does not take.

Its seed is a v0-shaped config rather than a current one, for the same reason: a file already at `CURRENT_VERSION` walks no migration at all.

**And what the first version of that target's documentation over-claimed**, caught in review before merge. It said `migrate` was driven "from every version a file could claim". `CURRENT_VERSION` is **1**, so `0..=CURRENT_VERSION` was two iterations of which one is a no-op, and `migrate`'s own `from > CURRENT_VERSION -> UnsupportedVersion` arm was unreachable. Nor is there a "chain" yet: `migrate.rs` says in its own header that the single registered step is a *fake* `v0 -> v1` that exists to exercise the framework. The range now runs past `CURRENT_VERSION` so the rejection arm is covered, and the doc describes the one real step rather than a sequence

### D-108

**Where:** `duja-app` `tray/state.rs` `begin_quit` &nbsp;·&nbsp; **Added:** P7 gate &nbsp;·&nbsp; **Drained:** P8 wave 4

~~**Every clean quit writes identity gamma to every display, including ones Duja never touched.** `begin_quit` calls `self.gamma.restore_all()` - which restores exactly what this session engaged, correctly - and then calls `duja_dimmer::restore_all()` **unconditionally**, as a "global identity pass [to clear] any ramp left over from a prior dirty run". What that second call does is not the same on all three platforms, and the comment beside it does not say so:~~

- ~~**Windows**: enumerates every gamma display and writes the identity ramp to each. A running f.lux loses its tint on every Duja quit.~~
- ~~**macOS**: `CGDisplayRestoreColorSyncSettings`, which reloads the user's **profile**. This is a restore rather than a flatten, and is the only benign arm.~~
- ~~**Linux/X11**: walks every CRTC on the screen - including ones driving nothing - and writes identity. `redshift`, `gammastep`, GNOME Night Light and a `colord` calibration curve are all clobbered.~~
- ~~**Linux/Wayland**: releases only this process's gamma controls. Benign, because there is nothing else to find.~~

~~**Why deferred.** Two reasons, and the second is the reason it is a row rather than a fix in the gate that found it. First, it is **not P7's defect**: the call has been there since the Windows train, so a fix changes shipped Windows quit behaviour and belongs in a PR whose subject that is, with its own review - `#82` is this project's standing example of what happens otherwise. Second, the analysis is worth landing before the change, because the *right* fix is not obvious from the symptom. The pass is nearly redundant everywhere: a leftover from a dirty run is what `startup::recover_from_crash_marker` handles, at launch, from the marker - and P7 is what gave **Linux** that marker, so the belt-and-braces argument is now weakest on the platform where the cost is highest. [D-099](debt.md#d-099) already carries the victim classification (`redshift` and friends repair themselves on their next timer; a `colord`/`xcalib` curve is loaded once at login and stays flattened), which is what makes this worth doing rather than filing as cosmetic. The likely shape is to drop the unconditional pass entirely and let the marker path own leftovers, keeping the wide walk for the two places a user asks for it: `duja --restore` and the tray's "Restore screen"~~

**How it drained.** The rule the row guessed at turned out to be the right one, and it is now `bin_support::gamma::tear_down_gamma`: **a rescue runs when there is something to rescue.** A quit that restored every ramp it engaged leaves nothing of ours behind, so the global identity pass has no work to do and skipping it is what stops a bystander's curve being flattened. A quit that could *not* restore something may have left a stuck ramp, which is a possibly-unusable screen, and that outranks another tool's tint - so the wide pass still runs there. The wide walk is kept unconditionally where the user asks for it by name (`duja --restore` and the tray's "Restore screen"), because someone pressing those is asking for exactly that trade.

**What this row got right and what it under-specified.** Right: that the pass is nearly redundant, and that the marker path owns leftovers. Under-specified: it proposed to "drop the unconditional pass entirely and let the marker path own leftovers", which would have removed the rescue from the one case that still needs it - a restore that failed. The failure mode the row was closest to missing is the one the second test pins.

**And the honest limit of the proof, which a review had to establish because the first version of this section got it wrong.** The two effects are parameters, so the *sequencing* is observable and the test goes red when the unconditional call is re-inserted **into `tear_down_gamma`**. That is not the criterion [plan.md](plan.md) sets. Re-insert the defect where it *historically occupied* - inline in `begin_quit`, bypassing the new function - and the whole suite stays green, because nothing reaches `begin_quit` at all. The first draft of this row claimed closures avoided the `#82` shape; they made the sequencing visible and moved no test one line closer to the caller.

What would close it is [D-102](debt.md#d-102)'s experiment, and that is the sharp part: D-102 already records that the "`AppState` cannot be constructed" reason **went stale** when `#134` removed the `tray_icon::TrayIcon` field. The excuse was re-asserted in new code without re-checking it.

**One case the fix's own argument does not cover.** "The marker path owns leftovers" is true for a leftover whose *address* is unchanged. It is not true for one whose address changed while the process ran: a monitor that renumbers (`\\.\DISPLAY2` to `DISPLAY3`) after engage and before the next apply batch leaves the guard restoring a stale device name, which can succeed - so `own_clean` is `true`, the wide pass is skipped, and the marker is removed, with a live ramp on a panel nothing now tracks. The old unconditional pass enumerated *current* displays and covered it. Whether the ramp survives a topology change at all is uncertain (`GammaBackend::invalidate` hedges that "the OS **may** have reset every ramp"), so this is an unexamined hole rather than a proven brick - which is why it is written here rather than left for someone to find

### D-114

**Where:** `xtask` `size.rs`/`dist.rs` + `main.rs` &nbsp;·&nbsp; **Added:** ~~`v0.1.6` checkpoint~~

~~**Both xtask subcommands take `std::env::Args`, and a test cannot choose what is in one.**~~ - **drained in `#156`**, the first change after the tag, exactly where the row said it belonged. `size::run` and `dist::run` now take `impl Iterator<Item = String>`, which `std::env::args()` already satisfies, so `main()` itself needed no change (`main.rs` gained the `mod args;` line and a corrected module doc, nothing else); `size`'s argument loop moved into an `Invocation::parse` on `dist`'s model and gained seven tests, the shared rule moved to a new `xtask/src/args.rs` with four of its own, and `size.rs` went from **60.10 %** of regions to **77.36 %** while the crate's suite went from 48 tests to 59.

**Half of the row was wrong, and the measurement is what settled it.** The headline said "**both**", and "every line of argument parsing in both is unreachable from a unit test by construction". That was true of `size` and false of `dist`: `dist::run` takes an `Args` but does no parsing at all - it delegates immediately to `Invocation::parse<I: Iterator<Item = String>>`, which has been generic since it arrived (`#104`, alongside the struct) and which **four** tests in that file already drive through its `args()` helper. The row was written by reading two function *signatures* and inferring what was behind them.

**Getting that paragraph right took three review rounds, and what they found is the record worth keeping.** It first said *twelve* tests drove the parser - which is `grep -c '#\[test\]'` on `dist.rs`, a count of something else entirely, used as though it answered this question. That is precisely the error this entry convicts the original row of, committed inside the correction that makes the accusation. It then said `dist`'s parsing was "already tested", which overstates in the other direction; the paragraph below has what is actually true. No draft-by-draft accounting is given, because this project's own rule is to remove a count rather than correct it a fourth time, and the substance is not improved by knowing which round produced which sentence.

The consequence is the part worth keeping. The row explained `dist.rs`'s **41.78 %** by that unreachability - "the uncovered part is disproportionately the arg loop and the error strings it produces". Draining it moved `dist.rs` to **41.01 %**, three quarters of a point *down*, because `value()` left the file for the new `xtask/src/args.rs` - taking 12 regions of which 1 was uncovered. So the row's number was real and its explanation of the number was not: `dist.rs`'s uncovered fraction is what its own module header calls "filesystem plumbing plus five external tools" - `windows`, `macos`, `fresh_dir`, `copy_into`, `archive` and `slices` alongside the header's own five, `powershell`, `lipo`, `codesign`, `hdiutil` and `tar` - and the header already says those are exercised by the release workflow's `workflow_dispatch` dry run rather than by unit tests. Nothing new is owed there; what was owed was not confusing a correct diagnosis of `size.rs` with an assumed one about its neighbour.

**Reachable is not the same as covered, and only the first was ever `dist`'s problem.** Reachability is what a signature decides; coverage is what tests decide. Two of `dist`'s parsing arms were cold on `main`, and exactly one of them is warm now:

- `value()`'s missing-value arm is executed by `args::tests::a_missing_value_names_the_flag_that_wanted_one`, the first thing in this repository ever to reach it. It closed by *leaving* `dist.rs`, so nothing about `dist.rs`'s own coverage improved by it.
- `Invocation::parse`'s `None => Target::host()?` arm is **still cold**. No test omits `--target` while supplying a valid `--version`, and none was added here. A real remaining gap in `dist`, small, and stated rather than left for the coverage number to imply otherwise.

**And making the parsing reachable immediately found a defect in it**, which is the argument for the row rather than against it. `size`'s loop took the token after `--target` unconditionally, so `cargo xtask size --target --release` set the triple to `--release`, looked under `target/--release/release`, and reported a *missing binary* at a path the user never typed - a message about the wrong problem entirely, on the tool a maintainer runs while cutting a release. `dist` had already decided that was wrong and guarded it with a flag-shaped-value check; the two subcommands had simply drifted. The check is now `xtask::args::value`, one rule in one place, and the red-first proof is at the site: `a_flag_shaped_target_value_is_refused_rather_than_used_as_a_directory` fails against `size`'s own loop with `Invocation { triple: Some("--release") }` and passes once the shared rule is routed in.

**What did not change, and deliberately.** A repeated `--target` still takes the last value rather than erroring, which is the GNU convention and what `dist` does with its own flags; it is now pinned by a test, because it is the one argument rule here that is a *choice* rather than a rejection and nothing else recorded it. The integration-test route the row mentioned in passing - spawning the built binary - was not taken and is not owed: it costs a release-profile build per case to reach rules a pure function now answers in microseconds
### D-040

**Where:** `duja-app` `tray/state.rs` &nbsp;·&nbsp; **Added:** ~~v0.1.1 (deep review) -> narrowed in `#82`~~

~~**The app layer between the `duja-ui` pin and the engine pin is unpinned.**~~ - **drained in `#157`**, with exactly the refactor this row named: `AppState` is now constructible in a test, so `set_user_level` and `on_ui_command`'s `SetLevel` arm are executed rather than reasoned about. `a_slider_drag_forwards_every_sample_and_the_released_value_last` drives six samples through `set_user_level` and asserts both that all six reach the engine and that the released one is last; `the_ui_command_arm_forwards_every_sample_too` does the same through the other entry point.

**Two tests rather than one, because the row named two sites.** `#82`'s rule is that a defect is re-inserted where it *historically occurred* rather than where a test can reach it, and this contract has two such places. A throttle added to `on_ui_command`'s arm leaves `set_user_level` untouched, so a single test that reached the deeper site through the shallower one would have proved nothing about the shallower one. Both were re-inserted and both went red - a leading-edge guard on `self.levels.forward(&writes)` leaves **one of six** samples standing:

```text
every sample must be forwarded [...]: [("GSM-0001-A", 87)]
  left: 1
 right: 6
```

**Both assertions are load-bearing, and for different failures.** A throttle that merely coalesced would fail the count; one that dropped the trailing edge would fail the last value. P4 gate Finding 1 did the *second* while looking like the first, which is why the released sample in the fixture is deliberately not the extreme of the drag: a throttle that happened to keep the minimum would still red.

**What the row got right, and the one thing it did not.** Right, and unusually so: it stated the gap, named both sites, said a refactor rather than a test was what closed it, and recorded that the defect had been re-inserted *empirically* rather than assumed. Its deferral reason was the false part - "`AppState` cannot be constructed in a test: it owns two live Slint shells and a concrete `tray_icon::TrayIcon`" - and it was false in one half from the day it was written (`duja-ui` was already building both shells headless) and in the other from `#134`. See [D-102](debt.md#d-102) for the re-triage, and for the part that is *still* open: the fixture answers this row and does not answer the two that need the gamma channel observable.

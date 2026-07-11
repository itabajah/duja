# Refactor debt

Running list, drained by a dedicated `refactor:` PR at each phase checkpoint
(time-boxed to ~15% of the phase). Add entries during feature work instead of
detouring; delete entries when drained.

| Added | Where | What | Why deferred |
|---|---|---|---|
| P1 | P4 QA | Manually verify tray menu-click delivery (not scriptable; identical dispatch path as verified hotkey) | Needs human hand on shell tray |
| P2 | `.github/workflows/` | Add `coverage.yml` (llvm-cov ≥90% gate) and `fuzz.yml` (weekly nightly burn) CI jobs | Ran locally at the P2 gate; wire into CI in a P8 hardening pass |
| P2 | `duja-core` `sync`↔`config` | Wire `MonitorConfig.sync_offset` (now persisted) into the app's `SyncGroups` load/save path | Consumer lands in P4/P5 UI wiring; core plumbing complete |
| P3 | `duja-ddc` `win/sys.rs` | Validate `classify_failure`'s `GetLastError`-after-VCP-call assumption on real hardware; a gone monitor may classify `Timeout` instead of `Disconnected` | dxva2 VCP calls are not documented to `SetLastError`; needs a live unplug experiment (console session) |
| P6 | `duja-app` `--stress` harness | The stress report gates on **absolute zero** hardware errors ⇒ PASS. Live QA (2026-07-11) showed the real MSI MP273QP surfaces 1–2 transient DDC errors per ~300 inputs in ~1 run of 5–8 under 20–25 Hz flood (NAK/timeout past the retry budget) — handled gracefully (no cascade, no false-unresponsive, no panic). Change the gate to an **error-rate threshold** so a normal-flakiness run isn't reported as FAIL | Cosmetic harness policy, not a product defect; the runtime degradation is already correct. Revisit when the P8 soak numbers set a real threshold |
| P3 | `duja-app` `engine.rs` | First `SetUserLevel` before the initial-Get ack scales against the default max=100; wrong absolute level on non-100-max monitors until calibration lands (transient, self-correcting) | Needs a queue-behind-initial-Get design decision; harmless for P3 bring-up |
| P3 | `duja-app` `engine.rs` | Watchdog deadline re-stamps on every dispatch, so continuous slider input against a stuck worker defers detection until the user pauses | Semantics decision (per-op vs per-display deadline); revisit with P4 UI feel |
| P3 | `duja-app` `run.rs` | `PlatformEvent::Suspending` is dropped; no pre-suspend write quiescing | Consumer is the P4 dimmer/state re-apply work |
| P3 | `duja-ddc`/`duja-app` | Full DDC capability probing (contrast, input source) at enumeration; P3 sets brightness-only capabilities statically | Consumers (contrast UI, VCP 0x60 switching) land in P4/P5 |
| P3 | `dujactl` | `duja-ipc` dependency is unused until the P5 transport lands | Placeholder for the P5 IPC client |
| P3 | `duja-panel` `wmi.rs` | `WmiMonitorID` array decoding, `WmiSetBrightness` invocation, and ProductCodeID assumptions never executed on real hardware (dev box has no internal panel) | Needs a 30-min borrowed-laptop run before P5 (plan §P3) |
| P4 | `duja-app` `engine.rs`/`run.rs` | Suspend/resume DDC re-push: on resume the display set is usually unchanged, so the manager emits no `Added`/`Reattached` and the engine never re-applies levels — a monitor that forgot its brightness across sleep (or a laptop panel reset by the firmware) stays wrong until the user nudges the slider | Needs hardware evidence (which monitors drop DDC state across S3/modern-standby) before choosing a policy: re-push all levels on `PlatformEvent::Resume`, or only after a resume-triggered enumeration diff |
| P5 | `duja-app` binary size | `duja.exe` is **17.21 MB** vs the ≤16 MB ADR-0012 budget (+1.2 MB, all ureq/rustls/ring/webpki-roots). Levers: fat LTO (−1.0 MB measured), feature-gate the update stack, drop `tracing-subscriber`'s `env-filter` regex | P8 hardening owns binary trimming; RAM and wakeup budgets still pass with headroom |
| P5 | `duja-core` `quirks` | User-directory quirk override (`quirks.override.toml`) is documented in the module + plan §7 but not wired — embedded DB only | Reduces attack surface today; wire with the P8 quirk-DB refresh from beta reports |
| P5 | `duja-ui` | Theme "Auto" resolves to dark: no OS dark-mode query is exposed by the pinned winit/slint | Revisit at the next Slint bump; explicit light/dark config is honoured |
| P5 | `duja-ui` settings | Sync-group management (create/assign/offset) has no UI, so `MonitorConfig.sync_offset` (persisted since P2) still has no consumer | Needs a group-management design, not a toggle; post-beta |
| P6 | `duja-ddc` `mac/` | macOS DDC/CI (enumeration + both I2C transports) has **never run on real hardware** — Duja has no Mac and CI mac runners are virtualized. Experimental until ≥3 independent community confirmations per architecture (Apple Silicon + Intel), per plan §P6 (ADR-0013) | Hardware-blind by design; the pure packet codec is fully CI-verified, only the FFI I/O is community-gated |
| P6 | `duja-ddc` `mac/sys.rs` | Display↔I2C-service matching is **positional** (CGGetOnlineDisplayList order); two identical external monitors can mis-pair ("brightness on the wrong monitor"). Port MonitorControl's EDID-attribute scoring (`ioregMatchScore`, `Location`-weighted) | Needs a multi-external-display Mac to validate; positional is correct for the common single-display case |
| P6 | `duja-ddc` `mac/sys.rs` | `io_return_to_result` maps every non-success `IOReturn` to `Timeout`; a truly gone display is only removed by re-enumeration, never surfaced as `Disconnected` from the transport. Distinguish `kIOReturnNoDevice`/`NotResponding` | Same live-unplug experiment gap as the Windows `classify_failure` debt; needs hardware |
| P6 | `duja-platform` `autostart/mac.rs` | Migrate macOS autostart from the `launchd` LaunchAgent plist to `SMAppService.loginItem` (the modern login-item API) | `SMAppService` requires a signed `.app` bundle and macOS 13+; land once the DMG bundle + code-signing exist (packaging phase) |

Drained at P2 gate (2026-07-08): MSI MP273QP quirk rows encoded in `quirks/quirks.toml`; contract suite hardened against `max`-lying backends (ADR-0002); unstamped-config migration semantics fixed (ADR-0007).

Drained at P3 gate (2026-07-10): `ResolvedQuirks` fields are now consumed by the
dxva2 backend (pacing, caps retry, verify-writes, `max_brightness`,
`input_source_allowed`, `no_input_switch`, `caps_unreliable`, `ddc_broken`).

Drained at P4 (2026-07-10): the ubuntu Slint system-deps step (fontconfig +
xkbcommon + xcb + wayland) is wired into the `clippy`, `test`, and `docs` CI
jobs now that `duja-ui` brings Slint into the workspace.

Drained at P5 gate (2026-07-10): `dujactl`'s `duja-ipc` dependency is now a real
consumer (IPC client); full DDC capability probing landed for input sources
(`Capabilities.allowed_inputs` = caps ∩ quirks). Gate findings fixed: the IPC
read deadline is armed once per exchange (a dribbling peer can no longer renew
it per syscall and pin a handler thread) and the first pipe instance is closed
on thread-spawn failure.

Drained by the P0 live-QA fix (2026-07-11): the `tray.rs` `APP.borrow_mut()`
-across-`update_from_vm` latent double-borrow is gone — all `AppState` access now
goes through the re-entrancy-safe `ReentrantCell` dispatcher (`with_app`), and
the *actual* live-QA crash (the settings shell callbacks held a `SettingsVm`
`borrow_mut` across the app's re-render `borrow`) is fixed by releasing the
borrow before the handler runs.

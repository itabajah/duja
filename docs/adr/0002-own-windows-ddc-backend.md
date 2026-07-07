# 0002 — DDC backend: own thin dxva2 implementation on Windows

- Status: accepted
- Date: 2026-07-07
- Evidence: spike branch `spike/ddc` (hardware run on an MSI MP273QP / NVIDIA
  machine) + GitHub-API research on the `ddc-hi` crate family.

## Context

The architecture research nominated `ddc-hi` as the cross-platform DDC/CI
abstraction. The P1 spike evaluated it against real hardware and inspected the
crate family's health. Findings:

- **Duplicate, unidentifiable displays on Windows.** `ddc-hi`'s default
  backends enumerated one physical monitor **twice** (WinAPI + NVAPI), and the
  WinAPI entry carries no EDID/serial (`ddc-winapi` source: *"TODO: good luck
  getting EDID"*) — its id is the non-unique `"Generic PnP Monitor"`. Fatal for
  per-monitor persistence (ADR-0004). A 153-LOC direct dxva2 proof enumerated
  the monitor once and recovered a stable PnP DeviceID.
- **The hard work is ours regardless of crate.** 60–70% of unpaced back-to-back
  VCP reads failed identically through `ddc-hi` and raw dxva2 — flakiness is
  monitor/OS-level. Retry, ≥40–50 ms pacing, and verify-by-readback must be
  built by us either way; no retry layer exists upstream (open since 2018).
- **Threading mismatch.** `ddc-hi::Display` is `!Send` (raw HANDLE + `Rc`),
  proven by compile error — it cannot move onto our per-monitor worker threads
  without our own unsafe Send wrapper anyway.
- **Dormant upstream.** ddc-hi 0.4.1 (2021, "passively-maintained"), pins
  ddc 0.2.2 (2020), drags `nom 3` (future-incompat) + `serde_yaml 0.7`;
  errors are `anyhow` (unclassifiable for retry/quirk logic); enumeration
  swallows backend errors; release-mode UB in enumeration was only fixed in
  ddc-winapi 0.2.2 (mid-2024).
- **Own-backend cost is small.** The full Windows DDC surface is 6 dxva2
  functions (`windows` crate features: `Win32_Foundation`,
  `Win32_Devices_Display`, `Win32_Graphics_Gdi`). Estimate ~300–450 LOC
  including enumeration, identity, caps, retry/pacing, drop-safe handles.

## Decision

`duja-ddc`'s Windows backend is written in-house against the `windows` crate
(dxva2 + `EnumDisplayDevicesW`/registry EDID for identity), implementing
`duja-core`'s `BrightnessController`. Our trait remains the only abstraction
the rest of Duja sees. `ddc-i2c` (Linux, EDID-capable, freshest upstream) and
`ddc-macos` (Intel Macs) remain candidate implementations *behind our trait*
for P6/P7 — decided then. Apple Silicon needs our own IOAVService backend
regardless (no upstream support exists).

## Consequences

- We own retry/pacing/verify: ≥40–50 ms inter-command gap, ≥3 retries with
  backoff for caps strings (measured 0.8–1.1 s, intermittent failures),
  read-back verification after writes.
- Known FFI sharp edges to encode: `PHYSICAL_MONITOR` is `repr(C, packed(1))`
  — copy fields out, never reference (windows-rs #2135); VCP functions return
  raw `i32` + `GetLastError` while enum/destroy return `Result` — wrap
  uniformly; physical-monitor `HANDLE` gets a documented unsafe `Send` newtype
  owned exclusively by its worker thread.
- Never trust `max` for non-continuous VCP features (spike: 0x60 reported
  max=3 with current=15); allowed values come from the caps string ∩ quirks.
- First quirk-DB rows captured (MSI MP273QP): needs pacing, flaky caps,
  write-verify required, NVAPI mislabels connectors.

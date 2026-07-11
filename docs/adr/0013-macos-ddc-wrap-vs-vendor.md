# 0013 — macOS DDC/CI: own thin backend behind our transport (don't wrap ddc-macos)

- Status: accepted
- Date: 2026-07-11
- Evidence: source review of `MonitorControl` (`Arm64DDC.swift`, `IntelDDC.swift`,
  `DisplayManager.swift`) and the `ddc-macos` crate (`haimgel/ddc-macos-rs`,
  0.2.2, 2024-11-05); crates.io/GitHub-API health checks of the candidate
  binding crates. Hardware-blind: Duja has no macOS hardware (plan §P6).

## Context

ADR-0002 already committed the *policy* layer (pacing, retry, verify-by-readback,
quirks, capability parsing) to `duja-core` behind our own
[`VcpTransport`](../../crates/duja-ddc/src/transport.rs) seam, and flagged that
Apple Silicon needs an IOAVService backend regardless because no upstream Rust
crate covered it. P6 is the point that decision is made concretely. The question
is narrow: for the *transport* (the raw wire), do we **wrap** the `ddc-macos`
crate or **vendor a thin backend of our own** against the maintained OS bindings?

Findings:

- **`ddc-macos` now covers Apple Silicon, but shallowly.** 0.2.2 (Nov 2024) added
  M1/M2/M3 support via the private `IOAVServiceCreateWithService` /
  `IOAVServiceWriteI2C` / `IOAVServiceReadI2C` symbols (linked against
  CoreDisplay) and reads EDID via `CoreDisplay_DisplayCreateInfoDictionary`
  (`"IODisplayEDIDOriginal"`). It is MIT, functional, and implements the `ddc`
  crate traits. But it is **low-activity** (no commits since the 0.2.2 release,
  2 open issues) and, critically, its multi-display **service-to-display
  matching is the known-fragile part** we would still have to own or fork.
- **The `ddc` trait is the wrong seam for us.** `ddc-macos` implements
  `DdcCommandRaw`/`Ddc` with its own `anyhow`-ish error type — unclassified for
  our retry/quirk logic — exactly the mismatch ADR-0002 rejected for `ddc-hi` on
  Windows. Wrapping it means adapting *its* trait onto *our* `VcpTransport`
  anyway, then re-deriving `Disconnected`/`Timeout`/`Backend` from opaque errors.
- **The wire is tiny and standard.** DDC/CI is three request shapes (get/set VCP,
  capabilities) plus reply parsing and an XOR checksum. The only real subtlety is
  that Apple Silicon's `IOAVServiceWriteI2C` path wants a slightly different frame
  than the Intel/standard MCCS frame (an extra length byte, checksum seed
  `0x6E ^ 0x51`) — a per-arm branch of a few lines, encoded from
  `Arm64DDC.swift`.
- **Own-backend cost is small and reuses everything.** By expressing the OS layer
  as an [`I2cBus`](../../crates/duja-ddc/src/ddcci.rs) (write bytes / read bytes)
  and building a generic `DdcCiTransport<B: I2cBus>` on it, the entire codec is
  **pure, cross-platform, and host-testable on every OS**, and the whole existing
  controller policy is reused with **zero duplication**. The concrete buses
  (`IOAVService` on Apple Silicon, `IOI2CInterface` on Intel) are the only
  platform code, confined to `mac::sys`.
- **Maintained bindings exist for the public surface.** `core-graphics` 0.25,
  `core-foundation` 0.10, and `io-kit-sys` 0.5 (the same MIT/Apache-2.0 slice
  `ddc-macos` itself uses) cover CoreGraphics enumeration, CoreFoundation
  containers, and IOKit service iteration. The `objc2-*` family is *healthier*
  still (Zlib/Apache/MIT, monthly releases), but the servo crates are the proven,
  lower-churn path for the thin slice we need and match `ddc-macos`'s own choice;
  either passes `cargo deny`. The genuinely private symbols
  (`IOAVService*`, `CoreDisplay_DisplayCreateInfoDictionary`) are in **no** crate
  and are resolved at runtime with `dlopen`/`dlsym` — which also gives us the
  graceful-absence behaviour the contract demands.

## Decision

`duja-ddc`'s macOS backend is **written in-house behind our `VcpTransport`**, not
a wrap of `ddc-macos`. Concretely:

- A cross-platform `ddcci` module owns the DDC/CI packet framing, checksum and
  reply parsing as pure functions, plus the `I2cBus` seam and a generic
  `DdcCiTransport<B: I2cBus>`. It is unit-tested (exact-byte vectors from
  MonitorControl + property tests) and bound to the `duja-core` controller
  contract with a scriptable fake I2C bus, so the mac controller logic runs green
  on **every** OS in CI.
- The macOS-only `mac` module enumerates external displays via CoreGraphics
  (skipping the builtin panel), recovers EDID via CoreDisplay, and provides the
  two concrete buses. Public OS APIs come from `core-graphics` / `core-foundation`
  / `io-kit-sys`; private symbols are `dlsym`'d with graceful absence.

`ddc-macos` remains a **reference implementation** we cross-check against, not a
dependency. Wrapping was permitted by the P6 brief if it "genuinely covers Apple
Silicon IOAVService cleanly" — it covers it, but not *cleanly enough* for our
seam (fragile matching we'd fork anyway, wrong error model), so the ADR-0002
precedent holds.

## Consequences

- **We own the display↔service matching** — the hard part. Apple exposes no
  direct `CGDirectDisplayID` → `IOAVService` link. We pair external displays to
  external `DCPAVServiceProxy` services (and Intel framebuffers) **positionally**
  in `CGGetOnlineDisplayList` order. The single-external-display case is
  unambiguous; two-or-more identical externals can mis-pair (documented failure
  mode: "brightness lands on the wrong monitor"). MonitorControl's EDID-attribute
  scoring (`ioregMatchScore`, `Location` weighted highest) is the hardening path,
  tracked in `docs/debt.md`.
- **Bounds are in points, not pixels.** `CGDisplayBounds` (and the misnamed
  `CGDisplayPixelsWide/High`) return points; reconciling to physical pixels for a
  macOS overlay dimmer is deferred to that later work. The Windows backend's
  bounds are pixels — the field's meaning is documented per backend.
- **Timings encoded as cited constants** from `Arm64DDC.swift`: 10 ms write
  settle, 50 ms write→read gap (above the DDC/CI ~40 ms floor).
- **New deps** (`core-graphics`, `core-foundation`, `core-foundation-sys`,
  `io-kit-sys`), all MIT/Apache-2.0 and target-gated to `cfg(target_os = "macos")`
  so no other platform's build graph sees them.
- **Experimental until community-verified.** Hardware-blind by construction: CI's
  mac runners are virtualized, so `enumerate` returns empty there and the buses
  never execute. DDC-on-mac is experimental until ≥3 independent community
  confirmations per architecture (Apple Silicon and Intel); see the crate docs and
  `docs/debt.md`.

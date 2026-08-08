# 0012 — Binary size budget raised to 16 MB for 1.0 (variance ADR)

- Status: accepted
- Date: 2026-07-10 (P4 gate)
- Evidence: release builds of `duja.exe` at the P4 gate on Windows/MSVC.

## Context

`docs/perf-budgets.md` set a ≤ 12 MB stripped-binary budget, derived from the
ADR-0009 renderer bake-off where a bare Slint software-renderer binary measured
9.76 MB. The assembled P4 tray application measures **14.9 MB** with thin LTO
(13.9 MB with fat LTO). The overage is not code we wrote: it is the Slint winit
backend's image/SVG decode stack (`resvg`, `image`, `tiny-skia`, `zune-*`,
`png`), `tray-icon`/`muda`, and `tracing-subscriber`'s `regex`-based
`EnvFilter`. Every other perf budget passes with headroom (idle RSS 23.3 MB vs
≤ 35; **zero** idle CPU wakeups measured over 20 s; `dujactl` is 0.6 MB).

## Decision

Raise the 1.0 stripped-binary budget for `duja.exe` to **≤ 16 MB** and treat
size reduction as a P8 hardening work item rather than blocking the MVP. The
levers, in expected-payoff order, are recorded for P8:

1. Fat LTO in the release profile (measured −1.0 MB; deferred only because the
   profile change is workspace-wide and P8 re-verifies budgets anyway).
2. Slint image-format features: the flyout uses no SVG/EXR/animated images —
   investigate disabling the decoder stack Slint pulls by default.
3. `tracing-subscriber` without the `env-filter`/`regex` feature (a static
   level filter is enough for a tray app; `--verbose` can stay).
4. `panic = "unwind"` is required by supervision (ADR-0005) — not a lever.

## Consequences

- `docs/perf-budgets.md` now reads ≤ 16 MB (aspiration 12) for `duja.exe`.
  *(P8 note: it reads `≤ 16,777,216 bytes (16 MiB)` today, and the aspiration
  was dropped when the unit was pinned. See the P8 section.)*
- If P8's levers get the binary back under 12 MB, this ADR is superseded and
  the original budget is restored.
- The RAM budget — the budget users actually feel, and the reason Electron was
  rejected — is unchanged and passing at 23.3 MB idle.

## Ledger

| Gate | `duja.exe` (stripped) | Verdict |
|---|---|---|
| P4 (tray + flyout + dimmer) | 14.9 MB (thin LTO) | within the raised 16 MB budget |
| P5 (+ settings, autostart, ureq/rustls update check) | **17.21 MB** | **over by 1.2 MB** |
| P7 (+ the Linux tray and gamma sink) | 19,446,784 bytes (thin LTO) | over by 2,669,568 |
| **P8 (hardening)** | **15,709,696 bytes** (fat LTO, `opt-level = "s"`) | **within, by 1,067,520** |

P5's overage is entirely the opt-in update check's TLS stack
(`ureq` + `rustls` + `ring` + `webpki-roots`). It is **not** accepted as a new
budget: P8 must recover it. Levers, in expected-payoff order:

1. Feature-gate the update check (default on for release, off for a "lite"
   artifact) — removes the whole TLS stack.
2. Fat LTO (measured −1.0 MB at P4).
3. `tracing-subscriber` without `env-filter` (drops `regex`).
4. Slint image-format features the flyout never uses.

If P8 cannot get under 16 MB, this ADR is superseded by an explicit
raise-with-rationale rather than silent drift. `dujactl` remains 0.6 MB.

> **Read the P8 section below before following either lever list above.** One of
> the four levers does not exist, the unit was never pinned, and the largest
> single component of the binary is in a section neither list mentions. P8 did
> get under the budget, so no raise was needed - `dujactl` measures
> 643,584 bytes and now has a budget of its own.

## P8 outcome (2026-08-08)

**Recovered, to 15,709,696 bytes (14.98 MiB).** Under the budget
for the first time since P4, with 19.2 % taken off the P7 binary. This section
records what worked, what the levers above got wrong, and the one trade that was
made rather than avoided.

### Two things this ADR said that were not true

**"16 MB" never named a unit, and four gates argued past it.** 16 MB
(16,000,000) and 16 MiB (16,777,216) differ by 5 %, which is wider than three of
the four levers listed above. The budget is now an integer number of **bytes**
in `xtask/src/size.rs`, and `docs/perf-budgets.md` states the same integer; a
test reads the doc and fails if they drift. The value chosen is
16,777,216 (16 MiB), the *looser* of the two readings, deliberately: the
measured binary lands under both, so taking the loose one costs nothing today,
and quietly tightening a budget under cover of disambiguating it would be a
different change wearing this one's clothes.

**The Slint image-format lever does not exist.** Named rather than numbered,
because this ADR lists levers twice and the two lists disagree: it is item 2 of
the *Decision* list above and item 4 of the *Ledger* list, and "strike lever 2"
read against the wrong one strikes fat LTO. The claim is "Slint image-format
features: the flyout uses no SVG/EXR/animated images - investigate disabling the
decoder stack Slint pulls by default." Investigated, and there is nothing to
disable: `slint/std` implies
`i-slint-core/std`, which implies `image-decoders` **and** `svg`, with no seam
between them. The formats that *were* optional are already off - which is why
`exr`, `tiff`, `qoi`, AVIF, WebP and GIF are all absent from the binary. Only
**PNG and JPEG** remain, which is what `i-slint-core` asks for by name
(`features = ["png", "jpeg"], default-features = false`). An earlier version of
this paragraph listed WebP and GIF as present; they reach only the build-dep
universe, which resolver 2 keeps out of the link. Removing the rest means patching Slint, which is not a hardening
change. Struck rather than left for the next person to spend a day on.

### And one thing the Context missed entirely

This ADR reasons about `.text`, because `cargo bloat` reasons about `.text`.
Read the P7 baseline's PE section table instead and it is
**11,885,568 bytes of `.text` and 7,048,192 of `.rdata`** - 11.33 MiB against
6.72, with **36 % of the file** in a section nothing here had ever looked at and
no tool in this project's reach attributes.

(Both figures come from one binary, rebuilt for this ADR in a clean worktree off
`main` so that the byte count the ledger below diffs against and the section
split are the same file. An earlier draft mixed a targeted build's byte count
with an untargeted build's sections - 25,600 bytes apart, which changed no
conclusion and would still have been two measurements presented as one.)

That section is dominated by static data tables: ICU segmentation,
normalization, properties and locale data, plus font and shaping tables. **The
attribution is by dependency graph rather than by symbol** - nothing here can
read a stripped PE's `.rdata` per crate, and that limitation is the finding as
much as the number is. It is also why "the largest single component" below should
be read as "the largest contributor we can name", not as a measured ranking:
`.text` is the larger *section*.

They do not all arrive with `std`, which an earlier draft claimed. `svg` and
`image-decoders` do; `unicode` (segmentation) and `shared-parley` (normalization)
come from `i-slint-core/default` and from the software renderer's `systemfonts`.
The conclusion survives intact - every one of those features is on a path a
desktop GUI cannot turn off - but the single feature name was wrong.

It reframes the problem either way. **The binary is not code-bound, it is
data-bound**, and the data is welded to a feature a desktop GUI cannot turn off.
That is why the dependency levers could not reach the budget and a profile change
had to.

### The update-check lever is a choice, not a saving

`ureq` + `rustls` + `ring` + `rustls-webpki` + `webpki-roots` measure 724 KiB of
`.text`, and the Ledger's lever 1 proposes feature-gating them "default on for
release, off for a 'lite' artifact". D-011 adds the WinRT toast bindings to the
same gate.

**Gating something that is on by default saves the default build nothing.** It
creates the *possibility* of a smaller artifact, and Duja ships no lite artifact,
so as a lever against this budget it is worth zero bytes until "we publish a
second artifact" is also a decision. It was not taken here, and it is not counted
below. Whether a lite build should exist is a packaging question, not a size one,
and answering it inside a size wave would have been the wrong PR for it.

### The measured ledger

Every row is `duja.exe`, stripped, `x86_64-pc-windows-msvc`, measured on the
dev box. Each lever was applied **alone** against the baseline before any were
combined, because a single combined diff that lands 5 MB teaches nothing about
which lever to reach for at 1.1.

| configuration | bytes | delta |
|---|---|---|
| P7 baseline: `lto = "thin"`, `opt-level = 3`, `env-filter` | 19,446,784 | - |
| `lto = "fat"` alone | 18,348,544 | -1,098,240 |
| `filter::Targets` instead of `EnvFilter`, alone | 18,782,720 | -664,064 |
| both | 17,557,504 | -1,889,280 |
| both, `opt-level = 2` | 17,161,216 | -2,285,568 |
| both, `opt-level = "s"` everywhere | 14,280,192 | -5,166,592 |
| **both, `opt-level = "s"` with the render path at 3** | **15,709,696** | **-3,737,088** |

Two of those numbers are worth more than their row. **Fat LTO and `Targets`
together beat the sum of their parts** by 126,976 bytes: LTO has less code to
work with once the regex engine leaves, and inlines across what is left.
And **`Targets` gave back nearly twice its `.text`** - `cargo bloat` attributes
345 KiB to `regex-syntax` and `regex-automata`, and removing them took 664,064
bytes off the file, because a crate leaves with its read-only data.

### The trade that was made, and the budget it was made against

`opt-level = "s"` is the lever that reached the budget, and it is the only one
here that is a **trade** rather than a free win. It is `-Os`: `-O2`'s pipeline
with size-aware inlining and vectorization thresholds. On a tight per-pixel loop
that costs real time, and Duja's per-frame path is a *software* renderer
(ADR-0009).

So it is not applied to the per-frame path. `i-slint-core`,
`i-slint-renderer-software`, `swash`, `zeno` and `duja-ui` keep `opt-level = 3`
through per-package profile overrides, and the 1,429,504 bytes that costs
against `"s"` everywhere is the price of not guessing about the renderer.

`swash` is on that list because a review caught the first version of it
crediting `zeno` with "glyph rasterization" and stopping there.
`i-slint-renderer-software`'s `fonts/vectorfont.rs` calls
`swash::scale::Render` to load, scale and hint the outline and hands `zeno` the
mask to fill, so exempting only `zeno` would have left half the glyph pipeline
at `-Os` while claiming to have exempted the frame path. It costs 163,328 bytes
and it is the difference between a list and a correct one.

**What is honestly not proven:** that a per-package `opt-level` override under
`lto = "fat"` produces codegen identical to a whole-program `-O3` build. The
mechanism is that rustc writes per-function `optsize` attributes into the
bitcode and the LTO pipeline honours them, so functions from the O3 crates carry
no size constraint - and the 1,429,504-byte difference proves the
overrides do reach the linker. "Reaches the linker" is not "identical to O3",
and this ADR does not claim it.

**And the budget that has not been re-measured**: "Overlay alpha update < 16 ms"
and "Cold start to tray icon < 300 ms". There is no automated render benchmark
in this repository and there never has been - both were measured by hand at P4.
A change to the optimization level plausibly affects them, and the rubric says a
plausibly-affected budget gets re-measured, so the re-measurement is booked into
[`docs/qa-checklist.md`](../qa-checklist.md) where the other hand-measured
numbers live, and the missing benchmark is a debt row. Naming the gap is the
point; the alternative was to take the bytes and say nothing.

### What now protects it

`cargo xtask size`, called by the release workflow's **Windows** job after it
builds. A *Windows* release cannot ship over budget; the macOS and Linux jobs
build their own binaries and measure nothing, and because `release` declares
`needs: [macos, linux]` both are already built when the gate runs. macOS cannot
use this number at all - its artifact is a universal binary carrying two
architectures - and neither platform has a measured budget. `docs/debt.md` D-110
carries that gap. It deliberately does not run per PR - the check needs a
fat-LTO release build, roughly twenty minutes on a hosted runner - so a
dependency bump that adds a megabyte is caught at the next release rather than
at the PR that lands it. That gap is a debt row too.

# 0023 — x11rb stays on 0.13 until the Slint graph moves

- Status: accepted
- Date: 2026-08-04

## Context

Dependabot opened `#128`, bumping `x11rb` 0.13.2 → 0.14.0, with all 11 checks
green. Declining it is a standing policy rather than a one-off, which is why it is
recorded here and not in `docs/debt.md`: that file's rows are things to *do* and
are deleted when drained, and "draining" a row that says `x11rb` is pinned would
read as an instruction to unpin it.

### Taking the bump would not move Duja off 0.13

Three crates in the Linux graph require `x11rb` on a caret range over 0.13, each
declaring it **independently**, and Duja declares none of the three
(`cargo tree --target x86_64-unknown-linux-gnu -i x11rb -e normal`):

| crate | requires | reached through |
|---|---|---|
| `softbuffer` 0.4.8 | `0.13.0` | `i-slint-backend-winit` |
| `winit` 0.30.13 | `0.13.0` | `i-slint-backend-winit` |
| `x11-clipboard` 0.9.3 | `0.13.0` | `copypasta` → `i-slint-backend-winit` |

`winit` is a **sibling** of `softbuffer` in that tree, not nested under it: it
declares `x11rb` itself (`winit-0.30.13/Cargo.toml`, `version = "0.13.0"`, with
`dl-libxcb`). That matters, because it means no single one of the three moving is
enough — they have to arrive on 0.14 together.

A fourth crate, `global-hotkey` 0.8.0, requires `0.13.1` for Linux and BSD
targets, and Duja *does* declare it. But `crates/duja-app/Cargo.toml` declares it
under `cfg(windows)` and `cfg(target_os = "macos")` only, so it never appears in
the Linux graph above and is never compiled for a Linux build. It shapes the
target-independent `Cargo.lock`, and nothing else.

A caret range on `0.13.x` means `>=0.13.x, <0.14.0`. This is a **semver
impossibility, not a lockfile snapshot that might drift**: no feature set, target
or resolver configuration can unify 0.13 and 0.14. So `#128` does not upgrade
anything. It adds 0.14 *beside* 0.13.

Duja's lever is a **Slint** bump — `slint` and `i-slint-backend-winit`, both
1.17.1 — not a `winit` bump. Duja has no direct `winit` dependency at all.

### The decisive cost: one process, three connect semantics

`x11rb` 0.14 removes the abstract-unix-socket attempt from `RustConnection`
(`src/rust_connection/stream.rs`: 0.13 tries the abstract socket first **and falls
back** to the filesystem path; 0.14 deletes both the attempt and the helper).
Taking `#128` therefore leaves a single Duja process holding three different
connect behaviours:

| consumer | transport | abstract socket |
|---|---|---|
| `duja-dimmer` | `x11rb` **0.14** `RustConnection` | **no** |
| `x11-clipboard` 0.9.3 | `x11rb` **0.13.2** `RustConnection` | yes, with fallback |
| `winit` 0.30.13 | `XCBConnection` via `dl-libxcb` | whatever the *system* libxcb does |

In a session reachable only over the abstract socket, the clipboard would connect
and the dimmer would not — a partial failure that neither version produces on its
own, and that is strictly worse than being uniformly on either one. Today every
`RustConnection` in the process comes from one build and they all behave the same.

That is the reason to hold. The removal itself is defensible upstream: x11rb's
changelog attributes it to a change in libxcb (not independently verified here,
and worth checking before citing it as settled). The objection is not to the
removal, it is to applying it to one consumer out of three.

### The upside is zero, established by diff rather than by changelog

A changelog is a summary, so the crates were diffed directly:

- `x11rb-protocol` 0.13.2 → 0.14.0 changes exactly **two source** files:
  `src/protocol/xproto.rs` (the `CUT_BUFFE_R0`–`CUT_BUFFE_R7` → `CUT_BUFFER0`–
  `CUT_BUFFER7` atom-name fix) and `src/parse_display/connect_instruction.rs`,
  which is a doc-comment-only change — and specifically the *documentation half*
  of the abstract-socket removal discussed above, not an unrelated edit. (The
  manifest and README also move, for the MSRV; nothing a consumer compiles.)
- **`randr.rs`, `xfixes.rs` and `shape.rs` are byte-identical** between the two
  versions (`cmp -s` clean on all three). Every area Duja uses — CRTC gamma
  tables, output enumeration, geometry, input shape — is unchanged. There is no
  un-changelogged fix waiting in them.
- `x11rb` itself changes only two library files, `rust_connection/stream.rs` and
  `xcb_ffi/mod.rs` (plus two examples and its manifest).
- `grep -rn 'CUT_BUFFE\|raw_window_handle' crates/` is empty: Duja references
  neither API the release touches.

For completeness, 0.14's breaking changes are three, not one: the abstract-socket
removal above, an MSRV rise to 1.68 (moot — this workspace is on 1.94 with a
1.96.1 toolchain), and a `raw-window-handle` 0.6 migration confined to `xcb_ffi`,
behind a feature Duja does not enable.

### The compile cost, stated accurately

The second copy compiles only Duja's feature set (`randr`, `xfixes`, `shape`, and
what they pull in), which is roughly **54k** of `x11rb-protocol`'s ~137k generated
lines, on top of the ~91k the shared 0.13.2 copy already builds for the union of
winit's, softbuffer's and x11-clipboard's features. So the ubuntu lane grows by
**55–60%** on that crate, not by 2×.

The addition is pure: Duja's three features are a strict subset of what the other
consumers already request, so the 0.13.2 copy does not shrink by a line. Nothing
ships today either way — `release.yml` builds macOS and Windows only, so there is
no Linux artifact for this to weigh down, and the cost is CI compile time.

## Decision

Hold `x11rb` at `"0.13"`. Record the reason at the declaration in the workspace
manifest, and add a `dependabot.yml` ignore for `0.14.x` so the decision is not
re-argued on every weekly run.

## Consequences

- **Lift it when the Slint graph moves**, not on a dependabot ping. The bump
  becomes free and consistent the moment `softbuffer`, `x11-clipboard` and `winit`
  arrive on 0.14 together through a Slint release.
- **Two things will not warn us.** cargo-deny's `multiple-versions = "warn"` will
  not fail a re-introduction, and the ubuntu CI lane is headless — no test
  exercises the connect path this bump changes, so a regression there would reach
  a user before it reached CI.
- **A RUSTSEC advisory against 0.13.2 would force action, but not this bump.**
  0.13.2 would remain in the graph through the three crates above, so cargo-deny
  would still fail after taking `#128`. The remedies would be a Slint bump, a
  `[patch]`, or a scoped `ignore` — bumping Duja's own edge only narrows Duja's
  own traffic.
- **cargo-deny, not dependabot, is what will surface such an advisory here.** A
  `dependabot.yml` `ignore` suppresses that dependency's *security* PRs as well as
  its version PRs, so if an advisory against 0.13.x were ever fixed only in a
  0.14.x, the entry above would hide dependabot's alert. The backstop is
  independent and unaffected: CI runs `cargo deny check` on every PR, and
  `[advisories]` in `deny.toml` is `version = 2` with `yanked = "deny"`.
- **Accepted downside:** 0.13 is likely the abandoned line (0.14.0 shipped
  2026-07-16, 0.13.2 on 2025-08-29; the series runs 5–16 months between
  releases), and migrating is
  free *today* precisely because `randr`/`xfixes`/`shape` are untouched. That stays
  true only as long as it stays true, so re-check the diff — not the changelog —
  each time the offer returns.

# 0021 — Tray-anchor coordinate contract (y-down normalized, unit named)

- Status: accepted
- Date: 2026-07-30

## Context

Placing the tray flyout needs three facts from the OS: where the cursor is, the
work area of the monitor under it, and that monitor's scale. `#87` hoisted those
queries out of the `duja-app` binary into `duja-platform::geometry` (`WorkRect`,
`TrayAnchor`, `cursor_anchor()`), because the geometry — unlike the remaining
app-local FFI — has a genuine cross-platform consumer.

Hoisting them forced a normalization question the Windows-only code had never had
to answer, and the two halves of it turned out to have different answers:

- **Orientation.** Win32 reports a y-**down** virtual desktop; Cocoa's global
  space is y-**up** with its origin at the bottom-left of the menu-bar screen. An
  un-flipped anchor does not mis-place the flyout by a few pixels, it mirrors it
  to the opposite screen edge.
- **Unit.** Win32 reports physical device pixels to a Per-Monitor-V2 process.
  macOS reports points, and each `NSScreen` carries its own
  `backingScaleFactor` — so there is **no single global physical-pixel space** to
  normalize into. Multiplying global point coordinates by any one display's
  factor makes a Retina built-in and a non-Retina external stop tiling: the same
  desktop would have two different pixel sizes for one shared edge.

`#87` shipped the orientation half and recorded the unit half as an open
question in `docs/debt.md`, deliberately not writing an ADR for half a decision.
The open half was concrete: the consumer converted a *logical* `.slint` window
size into anchor units by multiplying by `TrayAnchor::scale`. That is right when
anchor units are physical pixels and **double-scales on a Retina Mac**, where
anchor units are already logical. The macOS backend (this PR) is what closes it.

## Decision

`duja-platform::geometry` defines one anchor space, and names its unit rather
than pretending there is only one:

1. **Orientation is normalized: top-left origin, y increasing downward.** Every
   backend converts into it. The macOS backend does the Cocoa y-flip against the
   *first* `NSScreen`'s height (`NSScreen::screens[0]` is the menu-bar screen,
   whose bottom-left corner is the Cocoa origin), in a pure helper
   (`mac_geometry`) that is unit-tested on every CI host.

2. **The unit is *not* normalized; it is declared.** `TrayAnchor` carries an
   `AnchorUnit` — `PhysicalPixels` (Windows) or `Points` (macOS) — and the
   contract is "the unit the platform's own window-positioning API expects".
   This costs the placement kernel nothing: it only compares the cursor against
   the work area and clamps inside it, and both are in the same unit by
   construction.

3. **Two derived factors, not a raw scale.** `TrayAnchor` exposes
   `logical_to_anchor()` (multiply a logical window size by this to get anchor
   units) and `anchor_to_physical()` (multiply an anchor-space coordinate by this
   to get the physical pixels `slint::PhysicalPosition`/winit want).

   | Unit | `logical_to_anchor()` | `anchor_to_physical()` |
   |---|---|---|
   | `PhysicalPixels` (Windows) | `scale` | `1.0` |
   | `Points` (macOS) | `1.0` | `scale` |

4. **The invariant:** `logical_to_anchor() * anchor_to_physical() ==
   sane_scale(scale)` on **every** variant. Logical → winit-physical is `×scale`
   on every platform; the unit only decides *where* that single multiplication
   happens. Both factors route through the same low-end guard (`sane_scale`), so
   neither can hand a layout a `NaN` or a zero.

5. **`TrayAnchor::scale` stays, as the monitor's DPI/backing scale, and stops
   being what a consumer multiplies by.** Which conversion it belongs to depends
   on the unit, so reading it directly for placement is exactly the double-scale
   bug. Its doc comment says so, and the app's adapter (`tray::geometry`) no
   longer forwards it.

The consumer side renames follow from the contract: `positioning`'s
`physical_window_size`/`physical_dim` became `anchor_window_size`/`anchor_dim`,
and the `scale` parameter of `anchor_window_size`/`flyout_height_cap` became
`logical_to_anchor`. That module's docs used to assert "physical pixels"
throughout, which was true while Windows was the only backend and would have
become a false statement the moment macOS landed.

## Consequences

- **Windows is bit-for-bit unchanged.** Its `logical_to_anchor` is the monitor's
  `scale` and its `anchor_to_physical` is `1.0`, and the anchor→physical
  conversion is written so that a `1.0` factor is provably the identity (`i32` →
  `f64` is lossless, `×1.0` is exact, `round` of an integer is that integer)
  rather than approximately so. A test pins that at the extremes of the coordinate
  space. (Stated per factor rather than as an ordered pair on purpose: an
  "`x` and `y` respectively" phrasing of this table is exactly how the same
  sentence got written backwards once already.)
- **A new backend has exactly two questions to answer** — which unit its
  window-positioning API takes, and whether its y axis needs flipping — and the
  factor table answers everything downstream. This binds the P7 Linux backend,
  whose placeholder is `PhysicalPixels` today only because its scale is a flat
  `1.0`, which makes both factors `1.0` and the choice inert.
- **`AnchorUnit` is public API.** A third unit (if some platform positions
  windows in something that is neither) is an additive variant plus two table
  rows, not a re-litigation.
- **The y-flip helper is duplicated, on purpose.** `duja-dimmer`'s
  `mac_geom::cocoa_overlay_frame` does the forward flip (y-down → Cocoa) and
  `duja-platform`'s `mac_geometry` does the inverse. Sharing it would mean either
  `duja-platform` depending on `duja-dimmer` (a sibling backend, not a
  foundation) or putting screen-server geometry into the pure `duja-core`
  brightness kernel. The two are instead tied together by a test that round-trips
  through the dimmer's own formula, with a comment in each saying they must agree.
- **The contract fixes the *space*, not the OS's reporting conventions within
  it.** Each backend still owns the quirks of the API it reads, and macOS has one
  that bites immediately: `NSEvent::mouseLocation` reports a screen's y over
  `(y0, y0 + h]` — closed at the top, so the topmost cursor row reads exactly the
  screen's top edge — while a rectangle hit test is naturally half-open the other
  way. Unbiased, that gives every menu-bar click to a screen mounted *above* the
  primary, or (with nothing above) to the primary via the no-match fallback.
  `mac_geometry` handles it once, at the boundary where cursor reports enter — a
  quarter-point downward bias, whose safety condition is `0 < ε <= δ` for a cursor
  step `δ = 1/backingScaleFactor` — rather than distorting the containment
  predicate, which also has to serve the x axis where Quartz's convention is the
  opposite. A future backend should expect its own version of this and resolve it
  the same way: at the read, not in shared geometry.
- **What this does not settle:** winit's `set_outer_position` divides the
  `PhysicalPosition` by the scale factor of the screen the window is *currently*
  on, not the one being targeted. On a mixed-DPI Mac a flyout moving between a
  Retina and a non-Retina screen can therefore land off by the scale ratio. The
  contract is correct per-anchor; the residual is a winit-side property that
  needs real hardware to observe. It is recorded in `docs/debt.md` rather than
  papered over — and it is the **only** such row this decision leaves open: the
  cursor bias above was first filed as debt too, then drained once its safety
  condition showed there was nothing for hardware to decide.

## Alternatives considered

- **Normalize everything to physical pixels.** The obvious contract, and not
  implementable: macOS has no coherent global physical-pixel space (see Context).
  A "pick the cursor screen's factor" version works for a single display and
  breaks tiling the moment a second one has a different backing scale — the
  failure would appear only on mixed-DPI multi-monitor Macs, i.e. after release.
- **Normalize everything to logical units.** Symmetrically broken in the other
  direction: Windows work areas *are* physical, so converting them to logical
  loses sub-point precision at 125 %/150 % and re-introduces the exact
  off-screen-overflow bug (P0 live-QA bug 4) the physical anchor fixed. It also
  moves rounding error from one conversion into two.
- **Drop the multiply on macOS (`#[cfg]` in the consumer).** The minimal fix, and
  the reason this ADR exists instead: it puts platform knowledge back in the app
  layer that `#87` just took it out of, and it silently answers only the
  `logical_to_anchor` half — the `anchor_to_physical` half (winit dividing by the
  scale factor) would have stayed a latent Retina bug with nothing naming it.
- **Return both a logical and a physical anchor.** Two rectangles that must stay
  consistent, one of which is always a lossy derivation of the other. Two scalars
  with a stated product invariant is the same information with a testable
  relationship.
- **Keep deferring the ADR until Mac hardware confirms it.** The `#87` debt row's
  position, and it has to end somewhere: the arithmetic is now pinned by tests
  that run on every CI host, and the part that genuinely needs hardware (does the
  flyout land under the menu-bar icon on a real Retina Mac?) is a *verification*
  gap, not an undecided design. Deferring further would mean the macOS backend
  ships with its contract recorded nowhere.

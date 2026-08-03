# Manual QA checklist

Run per release (and the relevant OS section at phase gates). Sections grow
with the phases; keep entries as observable behaviors, not implementation.

## All platforms
- [ ] Tray icon appears < 300 ms after launch; correct in light and dark theme.
- [ ] Flyout opens on tray interaction, dismisses on Esc/focus-loss, never steals focus on open.
- [ ] Slider 100 → 0 is one visually continuous dim; no jump at the hardware/overlay handoff.
- [ ] Overlay never intercepts input: click/type/drag through a dimmed region (security property).
- [ ] Unplug → replug a monitor: levels restored ≤ 2 s, no crash, no ghost entries.
- [ ] Sleep → resume and lock → unlock: levels re-applied.
- [ ] Second app launch forwards to the running instance (flyout shows) and exits.
- [ ] Kill the process while dimmed → relaunch: screen state restored (no stuck gamma/overlay).
- [ ] Keyboard-only walkthrough: Tab between sliders, arrows adjust, Esc closes.

## Windows
- [ ] Mixed DPI (100/150/200%): overlays cover each monitor exactly; flyout anchors correctly for all taskbar positions.
- [ ] HDR toggle mid-session: gamma path disabled, overlay still works, tooltip explains.
- [ ] Display rotation and clone/extend mode switches survive without stale overlays.
- [ ] Laptop brightness keys reflect into Duja's slider (WMI events).
- [ ] **Gamma re-applies after a display event.** The macOS item below is not macOS-only:
      the coordinator diffs against its own record on every platform, and ADR-0003 says the
      Windows ramp "is reset by display events". Set a display to `dim_mode = "gamma"`, put
      the slider in the band where the **ramp** is doing the dimming — with shipped defaults
      that is roughly **13–24**, because `plan_for_platform` substitutes an overlay below
      `MIN_ACCEPTED_GAMMA` (0.5), which lands near slider 12 — then sleep/wake, or toggle a
      second monitor. It must stay dimmed with no slider input. Dragging "low" is the wrong
      test: below ~12 you are on the overlay path and the check passes without ever
      exercising a ramp.

## macOS (community-assisted until hardware access)
- [ ] Flyout on a Space with a fullscreen app; overlay joins all Spaces.
- [ ] **Gamma (if enabled) re-applies after wake.** Set a display to `dim_mode = "gamma"`,
      put the slider where the *ramp* is doing the dimming, then sleep and wake. It must
      come back dimmed without touching the slider. Before `#109` a pass here was
      **incidental** — it happened only when the wake also changed the display set, which
      made `restore_phase` forget the display so it re-engaged on return; a wake that left
      the set unchanged did not re-apply at all. So a failure is worth escalating, but an
      old passing run is not evidence the path was covered.
- [ ] DDC on Apple Silicon over USB-C (not built-in HDMI on M1/entry-M2 — expected unsupported).
- [ ] **Built-in panel below the backlight floor**: dragging its slider into the sub-floor zone
      keeps dimming, and the overlay covers the built-in screen **exactly** — no offset, no
      partial cover, nothing spilling onto a second display. This is the first behaviour to
      exercise `CGDisplayBounds` for the panel; a wrong rect shows up here and nowhere else.
- [ ] **Mirror the built-in to an external** (Displays → Mirror): the two collapse into **one**
      row with **one** overlay, correctly placed — two rows, or visible double-darkening where
      they overlap, is the `#66` shape. Then break the mirror and confirm they split back into
      two independent rows.
- [ ] **Mirrored + software-only member**: with a mirror set whose external cannot be driven over
      DDC, the laptop backlight is pinned to full and the shared overlay does the dimming (the
      documented group rule) — confirm it is not left stuck dark or double-dimmed.

## Linux
- [ ] X11 session **with** a compositing manager running: tray menu, overlay, backlight.
- [ ] X11 session with **no** compositing manager (kill `picom`/`xcompmgr`, or use a
      bare window manager): dragging a slider below the hardware floor must **not**
      black the screen out. Software overlay dimming reports itself unavailable
      naming the compositing manager as the reason, gamma still works if RandR is
      there, and hardware control is untouched.
      <!-- X ignores a window's alpha channel and draws its colour bytes at full
           coverage; only a compositing manager blends. Duja's overlay is black, so
           with no compositor every alpha renders as opaque black over the whole
           monitor. This is the one Linux check whose failure mode is
           unrecoverable-looking rather than merely broken, so run it before the
           happy path, not after. -->
- [ ] **Kill the compositing manager while Duja is dimming in software** on X11.
      The overlay must come down, not turn into a black screen. ADR-0011's
      amendment makes this the overlay's own responsibility (watch
      `_NET_WM_CM_S<n>` and tear down on an owner change); until that lands the
      capability is a startup answer only, and `docs/debt.md` carries it. Failing
      this before the overlay wave ships is expected; failing it after is a release
      blocker.
- [ ] **Fullscreen app while dimming** on X11: a compositing manager may unredirect
      a fullscreen window, which produces the same black screen with a compositor
      running. The overlay is meant to carry `_NET_WM_BYPASS_COMPOSITOR = 2` to
      forbid that; check a fullscreen video and a fullscreen game.
- [ ] Start a compositing manager **while Duja is running** on an X11 session that
      had none: the overlay becomes available without a restart. Same mechanism as
      the kill case above and the same debt row; this is the direction a capability
      table could never represent.
- [ ] Wayland session that **advertises** `zwlr_layer_shell_v1`: the overlay appears and dims.
- [ ] Wayland session that advertises **neither** wlr protocol: software dimming reports itself
      unavailable *with the reason*, and hardware paths still work.
- [ ] Wayland session advertising `zwlr_gamma_control_manager_v1` while another client
      (`wlsunset`, `gammastep`) already holds it: the bind is refused and the report flips to
      unavailable rather than claiming a gamma path Duja does not have.
      <!-- Written by capability, not by compositor name, per ADR-0011: a name table would fail
           a correct implementation the day Mutter shipped either protocol. `dujactl doctor`
           prints which protocols the session offered, so these are checkable without guessing
           which desktop is which. -->
- [ ] Missing i2c permissions: the app degrades gracefully (hardware control unavailable, no
      crash, software dimming still works) **and `dujactl doctor` names the fix**. Its Linux
      section lists every DRM connector with why its DDC/CI channel is or is not reachable, and
      the remedy where there is one: `modprobe i2c-dev` for a missing module, and *install
      `i2c-tools` then join the group* for permissions — not "join the `i2c` group", because on
      a stock system that group does not exist until a package creates it.
- [ ] `i2c-dev` not loaded at all: `doctor` says so per connector rather than reporting zero
      monitors with no explanation.

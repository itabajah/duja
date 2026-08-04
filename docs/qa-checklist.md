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
      Every overlay must come down within a moment, leaving the screen at its
      hardware brightness. It must **not** turn into a black rectangle. This is a
      release blocker: it is the one Linux failure a user cannot see well enough to
      recover from.
- [ ] **Fullscreen app while dimming** on X11: a compositing manager may unredirect
      a fullscreen window, which produces the same black screen with a compositor
      running. The overlay carries `_NET_WM_BYPASS_COMPOSITOR = 2` to forbid that;
      check a fullscreen video and a fullscreen game, in at least one compositor
      with `unredir-if-possible` (or its equivalent) explicitly enabled.
- [ ] **Click, type and drag through a dimmed X11 region.** The overlay's `XFixes`
      input region is empty, so every event must reach what is underneath. This is
      the ADR-0003 security property, and on X11 it is also the failure mode with
      no in-app workaround: an overlay that swallows input cannot be dismissed
      from the flyout it is covering.
- [ ] Start a compositing manager **while Duja is running** on an X11 session that
      had none: the overlay becomes available without a restart. **Expected to fail
      today** - the app starts its dimmer once, at launch, and `docs/debt.md`
      carries the missing re-spawn seam. Kept on the list because it is the
      direction a capability table could never represent, and because a passing run
      is what closes that row.
- [ ] **Open a window while dimming** on X11, then click around and switch
      workspaces. The dimming must stay on top of every window that appears. Watch
      for flicker against another always-on-top client (a second OSD, an on-screen
      keyboard, a presentation tool): X has no always-on-top, so Duja re-raises on
      every root restack, and two clients doing that can fight.
- [ ] **`duja --restore` on X11 clears a ramp Duja did not set.** Run
      `xrandr --output <name> --gamma 1:1:0.5` (or leave `redshift`/`gammastep`
      running), then `duja --restore`: the screen must
      return to normal and the command must report `restored identity gamma on N
      CRTC(s)` and exit 0. **N counts CRTCs, not monitors**, and on a multi-head GPU it
      is legitimately larger than the number of screens: the walk deliberately includes
      CRTCs driving nothing, because a gamma table survives its CRTC being disabled.
      Individual CRTCs are named (`DP-1 (CRTC 63)`, or `CRTC-3` for an idle one) only
      on the failure lines. This is the whole of Linux's gamma crash recovery today —
      there is no marker and no automatic recovery until the tray lands — so if this
      does not work, a user who ever hits a stuck ramp has nothing.
- [ ] **`sudo duja --restore` does not lie.** sudo drops `XAUTHORITY`, so the X
      connection fails. The command must print the reason on a `failed:` line and exit
      **non-zero**, never "nothing to restore" with exit 0. Same check for `DISPLAY`
      pointed at a server that is not running. This is the one failure mode a user with
      a dark screen will actually hit, because sudo is what people try first.
- [ ] **`duja --restore` on a Wayland session refuses rather than lying.** It must
      print "nothing to restore" and exit 0, **not** a count. `DISPLAY` is set to
      Xwayland on almost every Wayland session, and Duja must refuse on two independent
      grounds: the environment (`WAYLAND_DISPLAY` is set) and the server itself (the
      `XWAYLAND` extension is present). Check the second in isolation by clearing
      `WAYLAND_DISPLAY` from the environment and leaving `DISPLAY` set — a `systemd
      --user` unit or an `ssh` login is the real-world shape — and confirm it still
      refuses. If it reports restoring CRTCs there, check `Xwayland -version` before
      calling it a bug: only **Xwayland 23.1 and later** register the `XWAYLAND`
      extension, so the 22.1 branch in Ubuntu 22.04 LTS and Debian bookworm cannot
      be seen by this gate however new the point release is. On those the
      environment gate is the only one, and the uncovered case (22.1 *and* a
      stripped environment) is a documented limit rather than a defect. On 23.1 or
      later, a count there means the protocol check is broken and every gamma write
      in that session is going somewhere the user cannot see.
- [ ] **`--restore` flattens a running colour-temperature tool's tint**, and that is
      the documented behaviour rather than a bug: one LUT per CRTC, last writer wins,
      and Duja keeps no baseline yet (`docs/debt.md`). Check that Duja did not leave the
      screen darker than it found it. `redshift`/`gammastep`/Night Light rewrite on a
      timer and recover on their own; a **calibration** curve (`colord`, `xcalib`,
      `dispwin`) is loaded once at login and does **not** come back until the next
      login, so verify the tint loss and re-run the loader rather than waiting.
      <!-- Use `xrandr --gamma` rather than `xgamma` to set the test ramp: `xgamma`
           drives XFree86-VidModeExtension, and whether a server routes that into the
           same per-CRTC RandR LUT Duja writes is a driver-level behaviour rather than
           a protocol guarantee. If it does not, that row fails for a reason that is
           not a Duja bug. -->
- [ ] **An X server with no `RandR` at all** — `X -extension RANDR`, or `Xnest`.
      **Not `Xvnc`**: TigerVNC's server is built on xorg-server's own `randr/` and
      calls `RRScreenInit`, so it advertises the stock RandR version (1.6 on any
      xorg-server since 1.19) and exercises the *ordinary* path — neither this row
      nor the one below. `Xnest` works because it never calls `RRScreenInit`, so
      `RRExtensionInit` early-returns and the extension is genuinely absent. `duja --restore` must
      print "nothing to restore" and exit **0**, not a failure: such a server has no
      per-CRTC gamma table, so Duja can never have dimmed through it. This
      classification has flipped twice in review and no CI lane can reach it — if it
      exits non-zero, `UnsupportedExtension` is not what x11rb surfaces on that stack.
- [ ] **An X server whose `RandR` is present but older than 1.3.** Opposite
      expectation, and the contrast is the point. No easy modern server sits here —
      RandR has been at 1.6 since xorg-server 1.19 (2016) — so this may only be
      reachable on a genuinely old distribution or not at all; record it as
      untested rather than inventing a stand-in. If you can reach one: `duja --restore` must print a
      `failed:` line saying the CRTCs cannot be listed and exit **non-zero**. The gamma
      *writes* are RandR 1.2 and work, so a ramp may well be live and only the walk
      that would find it is missing — that is a rescue which could not run, not a
      session with nothing to rescue.
- [ ] **A multi-head GPU reports more CRTCs than monitors.** `duja --restore` counts
      CRTCs, and the rescue walk deliberately includes idle ones (a disabled CRTC keeps
      its gamma table). If N equals the monitor count exactly on a machine with spare
      CRTCs, the rescue is not reaching idle CRTCs and a ramp on an unplugged monitor
      would be missed.
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

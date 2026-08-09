# Manual QA checklist

Run per release (and the relevant OS section at phase gates). Sections grow
with the phases; keep entries as observable behaviors, not implementation.

## All platforms

**Two rows here are the only instrument two perf budgets have.** P8 wave 1 moved
the release profile to `opt-level = "s"` for everything except the crates on the
frame path, which plausibly moves both "cold start" and "overlay alpha", and
nothing in this repository can measure either. They were last measured by hand at
the P4 gate. If the numbers have moved, that is a finding and
[`docs/perf-budgets.md`](perf-budgets.md) needs it; if they have not, say so, so
the next person is not re-litigating a settled question. [D-109](debt.md#d-109)
built a frame instrument and it does **not** cover these two - it renders the
flyout, and neither row is about the flyout. Read its own row before assuming
otherwise.

The one thing that *is* automated now runs off this box and needs no session:

- [ ] **`cargo test -p duja-ui --release --test frame_probe -- --ignored --nocapture`**
      on each platform you have. It prints min/mean/max for a real three-monitor
      flyout rendered through the software renderer, and fails if any frame ran
      over 16 ms or if the run drew less than the full window. `--release`
      matters: the profile is the thing being questioned, so a debug number
      answers nothing.

- [ ] Tray icon appears < 300 ms after launch; correct in light and dark theme.
      **Time it** rather than eyeballing it - this is one of the two rows above.
- [ ] **Dragging a slider is smooth**, with no stutter as the overlay alpha
      follows it. The budget is one frame (< 16 ms) per alpha update, and the
      renderer is a *software* one, so this is the row that would notice a
      size-optimized build if the per-package exemptions did not do their job.
      Drag fast, across the full range, on the largest display available.
- [ ] Flyout opens on tray interaction, dismisses on Esc/focus-loss, never steals focus on open.
- [ ] Slider 100 → 0 is one visually continuous dim; no jump at the hardware/overlay handoff.
- [ ] Overlay never intercepts input: click/type/drag through a dimmed region (security property).
- [ ] Unplug → replug a monitor: levels restored ≤ 2 s, no crash, no ghost entries.
- [ ] Sleep → resume and lock → unlock: levels re-applied.
- [ ] Second app launch forwards to the running instance (flyout shows) and exits.
- [ ] Kill the process while dimmed → relaunch: screen state restored (no stuck gamma/overlay).
- [ ] Keyboard-only walkthrough: Tab between sliders, arrows adjust, Esc closes.

### The soak, once per release train (P8 wave 3)

- [ ] **`duja --soak 86400 --every 60`**, on an idle desktop, from a real tray
      build's box. **On Windows the invocation matters**, because a release
      `duja.exe` is a GUI-subsystem binary with no console: it prints to nowhere
      and the shell does not wait for it. Use

      ```
      start /wait duja.exe --soak 86400 --every 60
      echo %ERRORLEVEL%
      ```

      and read the report from `soak-report.txt` beside the rotating log, which
      the run writes regardless. The exit code is the verdict: non-zero on a
      budget miss **or** on a run that could not measure.

      **Quit any running Duja first.** The soak takes the IPC endpoint, and an
      already-running instance holds it - the report says `ipc server NOT
      started` when that happened, which means the run measured a smaller
      assembly than it is supposed to.

      Two things to record even on a pass: the **peak RSS**, which is the
      headless figure and not the tray one the idle budget asks for, and the
      **handle drift**, which is what should replace `HANDLE_GROWTH_TOLERANCE` -
      that constant is a guess today and its own docs ask for this run. Note the
      handle half is weak evidence here ([D-112](debt.md#d-112)): nothing the
      soak assembles creates a GUI object.
- [ ] **The tray build's idle RSS, by hand.** Task Manager, flyout closed, after
      a few minutes. This is the number `perf-budgets.md`'s idle row actually
      names, and `--soak` cannot produce it: it builds no window.

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
      on the failure lines. This is no longer the whole of **X11's** gamma crash
      recovery: P7 wave 5 gave Linux the crash marker, so a dirty exit is undone at
      the next launch (checked by its own row below). This stays a release blocker
      anyway, because it is the recovery a user reaches for when the automatic one
      did not happen -- a marker that could not be written, or a screen that has to
      be fixed without launching the app. A Wayland
      session needs none of it; see the `kill -9` row below for why, and for the
      check that the compositor really does behave that way.
### The tarball, the tray and the sink (P7 waves 5-6, nothing below has ever run)

Run these **first**: everything under the dimming rows above assumes a Duja that
started, and none of the code in this block has executed anywhere. A CI runner
has no display server and no `StatusNotifierWatcher`, so every accept path here is
unexercised ([`docs/debt.md`](debt.md) D-105).

- [ ] **The tarball installs the way its README says.** Extract
      `duja-<version>-linux-x64.tar.gz`; it must produce exactly one directory,
      named for the release. Run the `install -D` block in
      [`packaging/linux/README.md`](../packaging/linux/README.md) verbatim - if any
      line needs editing, the README is wrong and that is the finding.
- [ ] **`dujactl doctor` before anything else.** It must name the transport
      (X11/Wayland), the overlay and gamma verdicts, and the displays found. A
      missing shared library shows up here as a launch failure rather than as a
      Duja bug, which is the whole reason this is the first command.
- [ ] **The tray icon appears**, and is the accent-coloured monitor glyph rather
      than a placeholder or a black square. The pixmap is ARGB32 in network byte
      order and no lane has ever rendered it: a wrong byte order shows as colour
      channels swapped, a wrong alpha as a black or invisible icon.
      <!-- KDE Plasma implements StatusNotifierItem natively. On GNOME the
           AppIndicator extension is required, and its absence is the expected
           "no icon" case rather than a defect - check `dujactl doctor` still
           works, which proves the process is alive. -->
- [ ] **Every menu item does what it says**: Open, Settings, Restore screen,
      Restart, Quit. Each one dispatches across a thread boundary onto the Slint
      loop, and none of that path has run - a wrong dispatch is a menu item that
      silently does nothing.
- [ ] **Left-click toggles the flyout**, and on **X11** it opens near the cursor
      on the monitor the cursor is on. On **Wayland** it lands wherever the
      compositor puts it; that is expected, not a defect (there is no global
      cursor query and no client-side toplevel positioning).
- [ ] **The flyout opens promptly on X11 with a slow or absent `$HOME`.** The
      anchor probe is bounded at 250 ms and falls back; a freeze here means the
      deadline is not covering what it should.
- [ ] **The hotkey rows in Settings are greyed out**, with the reason naming the
      platform rather than "the combination is already in use". Linux registers no
      global hotkeys at all, and telling a user their combo is taken is a loop with
      no exit.
- [ ] **The gamma hazard caption names the display server, not macOS.** Set a
      display to `dim_mode = "gamma"` in Settings and read the caption underneath:
      it must talk about the display server reporting an accepted write, and must
      **not** mention "Automatically adjust brightness", which is a Mac setting.
- [ ] **`dim_mode = "gamma"` actually dims below the floor** on X11, and the
      screen returns to normal when the slider comes back up. No lane has ever
      reached a successful `set_gamma`, on any platform - this is the accept path.
- [ ] **Crash recovery, X11.** With a display dimmed by gamma, `kill -9` the
      process. The screen stays dark (an `XRandR` ramp is server state). Relaunch:
      it must come back to normal by itself, from the crash marker at
      `$XDG_DATA_HOME/duja/gamma.dirty`. Check the marker is gone afterwards.
- [ ] **Crash recovery, Wayland: there must be nothing to recover.** Same `kill -9`
      while dimmed by gamma. The screen must return to normal **immediately**,
      before any relaunch, because the compositor drops every gamma control a
      client held when its socket closes. If it does not, that is a compositor
      behaving differently from what ADR-0010's reasoning assumes, and it is a
      finding worth writing up rather than a Duja bug to fix.
- [ ] **Quitting does not flatten a colour-temperature tool.** Start
      `redshift`/`gammastep`, let it tint the screen, launch and quit Duja without
      touching gamma. **Expected to fail today on X11** - `docs/debt.md` D-108 has
      the analysis. Kept on the list because a passing run is what closes that row,
      and because the tint returning within a minute (the tool's next timer) is
      what distinguishes it from the `colord` case, which does not.

- [ ] **`sudo duja --restore` does not lie.** sudo drops `XAUTHORITY`, so the X
      connection fails. The command must print the reason on a `failed:` line and exit
      **non-zero**, never "nothing to restore" with exit 0. Same check for `DISPLAY`
      pointed at a server that is not running. This is the one failure mode a user with
      a dark screen will actually hit, because sudo is what people try first.
- [ ] **`duja --restore` on a Wayland session has nothing to restore, and says so.**
      It must print "nothing to restore" and exit 0, **not** a count. (The title used
      to say "refuses"; since the `wlr-gamma-control` channel landed this is an
      emptiness rather than a refusal — that channel cannot leave a ramp for a
      separate process to find. The `XRandR` half below is still a refusal.) `DISPLAY` is set to
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
      (`wlsunset`, `gammastep`) already holds it: an attempt to take gamma is **refused**,
      and Duja reports that refusal to whatever asked rather than claiming a dim it does
      not have. Check the refusal, not the doctor line: this row used to say "the report
      flips to unavailable", and it does not. `SurfaceCaps::refuse_gamma` exists for that
      downgrade and has no caller — `dujactl doctor` reads the registry only, and a probe
      that *bound* a control to find out would take the output away from `wlsunset` to
      answer a read-only question. `docs/debt.md` carries the gap.
      <!-- On wlroots 0.17+ the refusal arrives as `failed` on Duja's new object. On 0.16
           and earlier (which includes the 0.15 in Debian bookworm and Ubuntu 22.04 LTS)
           `get_gamma_control` instead
           evicted the *incumbent* and answered the newcomer with nothing at all, so
           Duja's object receives neither `gamma_size` nor `failed`. Both end in a
           refusal; on the older ones, expect `wlsunset` to lose its tint as a side
           effect, which is the compositor's behaviour and not Duja's. -->
- [ ] **`duja --restore` on a Wayland session must not connect at all.** The row above
      already pins the *answer* ("nothing to restore", exit 0); this one pins the
      reason. A `zwlr_gamma_control_v1` ramp dies with the client that set it, so a
      fresh process has nothing to find and opening a socket to discover that would
      also mean binding a gamma manager for nothing. Watch with
      `WAYLAND_DEBUG=1 duja --restore`: there must be **no** `zwlr_gamma_control`
      traffic. (A `wl_display` connect from some other part of startup is fine; a
      `get_gamma_control` is not.)
- [ ] **A Wayland gamma dim survives being killed.** This is the property the whole
      Wayland gamma design rests on and the one no CI lane can check. Once the tray
      lands and a ramp can be engaged: dim a display through the gamma path on a
      wlroots session, confirm the screen changed, then `kill -9` the process. The
      screen must return to normal **immediately**, with no `duja --restore` and no
      relaunch. If it stays dark, the compositor is not restoring on client
      disconnect and the `#124` crash-guard debt row was narrowed to X11 wrongly.
- [ ] **A Wayland gamma restore releases the output rather than parking on it.**
      Not "the tint comes back": an earlier version of this row said that and it was
      unrunnable, because stopping `gammastep` destroys `gammastep`'s own control and
      the compositor drops its tint at that instant — there is nothing left for Duja
      to restore, so the tester would have seen a neutral screen before Duja touched
      anything and been told it was a bug. What the destroy actually buys is the
      **release**, so check that instead. With `gammastep` stopped, dim through the
      gamma path, undim, then start `gammastep` again: **it must acquire the output
      and its tint must appear.**
      <!-- One caveat on wlroots 0.16 and earlier, where a newcomer facing a held
           output evicts the incumbent and is itself never registered: if the tool
           under test retries after a failed acquire, its second attempt finds the
           output free *because its first attempt evicted Duja*, and a tint would
           appear over an output Duja never released. If it does appear, confirm
           Duja is not still holding the control (`WAYLAND_DEBUG=1`, look for a
           `zwlr_gamma_control_v1` this process has not destroyed) before passing
           the row. Not an issue on 0.17+, which refuses the newcomer instead. --> Do not use "`gammastep` logs a gamma-control failure" as the signal:
      that is what a still-holding Duja looks like on 0.17+, but on 0.16 and earlier
      (which includes the 0.15 in Debian bookworm and Ubuntu 22.04 LTS)
      `get_gamma_control` answers a newcomer
      facing a held output with *nothing at all*, so `gammastep` would show no tint
      and log nothing. Absent tint is the failure on both.
- [ ] **(Not yet checkable) Nothing read-only steals gamma from a running
      `gammastep`.** Marked so rather than dressed up as a gate, because **no shipped
      command can fail it today**: the rule belongs to `enumerate_gamma_displays`,
      which deliberately reports every named output *without* taking a control, and
      nothing calls it. `dujactl doctor` goes through `probe_session`, which reads the
      registry and binds no gamma manager; `dujactl list`, `duja --once` and
      `duja --restore` never reach the enumeration either. An earlier version of this
      row listed those four commands as the test, which reproduced the defect it was
      written to fix. Becomes live the first time something on Linux enumerates gamma
      displays: run `gammastep`, run that thing, and `gammastep` must keep its tint.
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
- [ ] **Flyout placement on X11.** *Blocked until the tray lands on Linux: nothing
      currently calls `cursor_anchor()` there, and no CLI surface reaches it.* When
      it does, four checks in one session:
      - a single monitor with a panel on any edge - the flyout must sit clear of the
        panel, not under it;
      - two monitors - the flyout must open on the one the pointer is on, clamped to
        *that* monitor's work area;
      - two monitors of **different heights** with the panel on the shorter one -
        the taller monitor's flyout must use its full height. This is the case
        `_NET_WORKAREA` gets wrong and the reason Duja reads struts instead: a strut
        is measured from the edge of the whole screen, so the panel's raw `bottom`
        value is much larger than the panel is tall;
      - a panel on the **right** edge with a second monitor to its left - the left
        monitor must be untouched. Measuring that strut from the monitor's own right
        edge instead of the screen's is the mistake this catches, and it is invisible
        on the monitor the panel is actually on.
- [ ] **Flyout size on a HiDPI X11 session.** *Blocked on the ksni wave, like the
      row above and the row below - there is no flyout on Linux until the tray
      lands.* Set `Xft.dpi: 144` (or let the desktop set it), restart, and compare
      the flyout against any GTK or Qt dialog: same apparent size. Then check
      that `WINIT_X11_SCALE_FACTOR=2` scales it further, and that
      `WINIT_X11_SCALE_FACTOR=randr` falls back to the display measurement.
      <!-- Duja re-implements winit's scale chain rather than asking for it, because the
           anchor has to be computed before the window exists. The failure mode is not a
           wrongly *sized* window - Slint still uses winit's number for that - but a
           wrongly sized *clamp box*, so it shows up as a flyout overhanging the panel
           or the screen edge rather than as an obviously wrong window. `docs/debt.md`
           carries the pin. -->
- [ ] **Flyout placement on Wayland is the compositor's, and that is expected.**
      *Blocked on the same thing as the two rows above: there is no flyout on
      Linux at all yet - `run_tray` is a stub that prints
      "the tray application is not available on this platform in this build" and
      exits 1.* When the tray lands: the flyout will not open under the tray icon,
      and that is not a bug to file. A Wayland client cannot ask where the pointer
      is and cannot position its own toplevel, so the anchor has to arrive through
      `Activate(x, y)` and a compositor-side positioner. What must hold even then:
      the flyout opens, is fully on screen, and is the right size.

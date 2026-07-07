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

## macOS (community-assisted until hardware access)
- [ ] Flyout on a Space with a fullscreen app; overlay joins all Spaces.
- [ ] Gamma (if enabled) re-applies after wake.
- [ ] DDC on Apple Silicon over USB-C (not built-in HDMI on M1/entry-M2 — expected unsupported).

## Linux
- [ ] X11 (KDE/GNOME) and KDE Wayland: tray menu, overlay, backlight.
- [ ] GNOME Wayland: software dimming correctly reports unavailable; hardware paths work.
- [ ] Missing i2c permissions: `dujactl doctor` names the fix; app degrades gracefully.

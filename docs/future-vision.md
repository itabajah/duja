# Duja Future Vision: A Menu of Everything It Could Do

Today Duja is an ultra-lightweight Rust tray app that adjusts monitor brightness. Under the hood it already speaks DDC/CI over its own backend (the same `dxva2` `SetVCPFeature`/`GetVCPFeature` path the Windows low-level monitor API exposes), reads laptop panels over WMI, and can dim any display below its hardware floor with a software overlay. It already carries a `Feature` enum with Brightness, Contrast, and InputSource, so several of the ideas below are less "new capability" and more "wire up a code we can already talk to."

This document is a deliberate over-collection. It is a menu, not a plan. Everything Duja could plausibly do is written down here so we can triage later, from one-line wiring jobs to research-grade experiments. Nothing was dropped for being ambitious. Feasibility ratings (Proven, Likely, Hard, Speculative) fold in the adversarial verdicts, and wherever a verdict flagged a real-world gotcha there is a reliability caveat attached. Where an idea is a direct extension of code Duja already has, it is marked as low-hanging fruit.

A note on how the hardware layer works, since it underpins Section 1 and 2: DDC/CI is an I2C-based protocol (the display sits at fixed address 0x6E/0x6F, the host at a virtual 0x50/0x51). It carries single-byte VCP ("Virtual Control Panel") feature codes defined by the VESA MCCS standard. Codes 0x00 through 0xDF are standardized; 0xE0 through 0xFF are reserved for each manufacturer to do as they please. Continuous (C) codes take any value up to a reported max; Non-Continuous (NC) codes take a defined set of discrete values; Table (T) codes carry structured blobs like a LUT. The capabilities string (fetched with DDC/CI opcode 0xF3) tells you exactly which codes and which discrete values a given panel actually implements, which is the key to not showing controls a monitor cannot honor. Sources for this section throughout: [ddcutil VCP info](https://www.ddcutil.com/vcpinfo_output/), [ddcutil vcp_feature_codes.c](https://github.com/rockowitz/ddcutil/blob/master/src/vcp/vcp_feature_codes.c), [VESA MCCS 2.2a spec](https://milek7.pl/ddcbacklight/mccs.pdf), [Microsoft low-level monitor config](https://learn.microsoft.com/en-us/windows/win32/monitor/using-the-low-level-monitor-configuration-functions), [ControlMyMonitor](https://www.nirsoft.net/utils/control_my_monitor.html).

---

## 1. Hardware: standard DDC/CI (MCCS VCP) features

Everything in this section rides the exact transport Duja already uses. On Windows that is `SetVCPFeature` / `GetVCPFeatureAndVCPFeatureReply` plus `CapabilitiesRequestAndCapabilitiesReply` on a physical-monitor handle; on Linux the same commands go over `/dev/i2c-*`; on macOS via IOKit/DDC. The single biggest structural win here is not any one code, it is **capabilities-string parsing** so the UI is built per-monitor from what the panel reports rather than hard-coded. That one piece makes every other hardware idea reliable.

### VCP code reference table

Reliability notes reflect field behavior, not just what the spec allows.

| VCP (hex) | Feature | R/W | Type | Real-world reliability |
|---|---|---|---|---|
| 0x10 | Brightness / Luminance | RW | C | Near-universal. Duja's core today. |
| 0x12 | Contrast | RW | C | Near-universal. Already in Duja's `Feature` enum. |
| 0x14 | Select color preset (5000K/6500K/sRGB/User...) | RW | NC | Common, but the value list is vendor-specific; enumerate from capabilities. |
| 0x0B | Color temperature increment | RO | NC | Rare; pairs with 0x0C. |
| 0x0C | Color temperature request (Kelvin) | RW | C | Rare; Kelvin = 3000 + (0x0C x 0x0B). Many panels only do 0x14 presets. |
| 0x16 / 0x18 / 0x1A | Video gain Red / Green / Blue | RW | C | Widely present on desktop monitors ("User" RGB sliders). |
| 0x6C / 0x6E / 0x70 | Video black level Red / Green / Blue | RW | C | Less commonly exposed than gain; pairs with it for 2-point calibration. |
| 0x6B / 0x6D / 0x6F / 0x71 | Backlight level White / R / G / B | RW | C | Uncommon; on some panels this is the *real* backlight vs 0x10's signal scaling. |
| 0x13 | Backlight control (legacy) | RW | C | Deprecated in MCCS 2.2; do not target new designs, but old panels have it. |
| 0x87 | Sharpness | RW | C (spec: NC) | Common; treat as continuous in practice (often 0-7 or 0-max). |
| 0x72 | Gamma | RW | NC (complex) | Uncommon; vendor-specific encoding, enumerate from capabilities. |
| 0x90 / 0x8A | Hue / Saturation | RW | C | Mostly TV/multimedia panels. |
| 0x59-0x5E | 6-axis saturation (R,Y,G,C,B,M) | RW | C | Higher-end "pro" color monitors only. 0x7F = nominal. |
| 0x9B-0xA0 | 6-axis hue (R,Y,G,C,B,M) | RW | C | Same population as above. 0x7F = nominal. |
| 0x60 | Input source | RW | NC | Very widely supported. Values frequently vendor-remapped, so read capabilities. Already in Duja's enum. |
| 0xD0 | Output select | RW | NC | Same value table as 0x60; multi-output/PBP panels. |
| 0x86 | Display scaling / aspect | RW | NC | Present on many; the only way to reach the panel's own scaler. |
| 0xDC | Display application / picture mode | RW | NC | Common on gaming/multimedia; names vendor-defined. |
| 0xDB | Image mode (full/zoom/squeeze) | RW | NC | Less common. |
| 0xDA | Scan mode (under/overscan) | RW | NC | TV-ish panels. |
| 0x62 | Audio speaker volume | RW | C (2.2+: NC w/ special vals) | Only panels with speakers. |
| 0x8D | Audio mute / screen blank | RW | NC | Only panels with speakers; SH governs mute, SL can blank screen. |
| 0x8F / 0x91 / 0x93 / 0x94 | Treble / Bass / Balance / Processor mode | RW | C/NC | Rare; multimedia monitors. |
| 0x63 / 0x64 / 0x65 | Speaker select / Mic volume / Jack status | RW/RO | NC | Very rare. |
| 0xD6 | Power mode (DPMS) | RW | NC | 0x01 On, 0x02 Standby, 0x03 Suspend, 0x04 Off, 0x05 hard off. Sleep reliable; wake-over-DDC often impossible (bus unpowered). |
| 0xD7 | Auxiliary output power | RW | NC | Very rare; power the display feeds a host device. |
| 0x04/0x05/0x06/0x08/0x0A | Restore factory: all / bri-con / geometry / color / TV | WO | NC | 0x04 nearly universal; others panel-dependent. Any non-zero triggers. |
| 0xB0 | Save/restore settings | WO | NC | 0x01 store to NVRAM, 0x02 restore factory. Inconsistent vendor support. |
| 0x02 | New control value (OSD-change flag) | RW | NC | For syncing app UI with hardware OSD changes. |
| 0x52 | Active control (changed-VCP FIFO) | RO | NC | Same purpose; read which code the user just changed on the OSD. |
| 0x0C save via opcode 0x0C | Save current settings (protocol op, not VCP) | - | - | Persists adjustments; wait 200ms after. |
| 0xCA | OSD / button lock + OSD enable | RW | NC (complex) | Kiosk/shared-desk; encoding version-dependent. |
| 0xCC | OSD language | RW | NC | ~36 enumerated languages incl. Arabic (0x0F). Trivial via NC path. |
| 0xAA | Screen orientation (0/90/180/270) | RO | NC | Reports physical rotation; can drive OS rotation. |
| 0x82 / 0x84 | Horizontal / Vertical mirror (flip) | RW | NC | Rare in the field. |
| 0x86 covered above; 0x54 | Performance preservation (anti-burn-in: orbit, dim static) | RW | NC | Rare; bit-flagged. |
| 0x1E / 0x1F / 0xA2 | Auto setup / Auto color / Auto setup on-off | RW/WO | NC | Analog/VGA "fix my picture" button. |
| 0x0E / 0x3E | Clock / Clock phase | RW | C | Analog/VGA only. |
| 0x20 / 0x30 | Horizontal / Vertical position | RW | C | Analog/VGA (and CRT). |
| 0x22 / 0x32 | Horizontal / Vertical size | RW | C | Analog/VGA (and CRT). |
| 0x24-0x4C | CRT geometry (pincushion, keystone, rotation, convergence, linearity, moire) | RW | C | CRT-only; gate on 0xB6 = CRT. |
| 0x01 | Degauss | WO | NC | CRT-only one-shot. |
| 0x8B | TV channel up/down | WO | NC | TV-tuner panels only. |
| 0x2E | Gray scale expansion | RW | NC | Occasional (0-3 on some HP). |
| 0xC0 | Display usage time (hours) | RO | C | Read-only telemetry; not all panels populate it. |
| 0xB6 | Display technology type (CRT/LCD/OLED/Plasma) | RO | NC | Reliable identity read. |
| 0xB2 | Flat-panel sub-pixel layout (RGB/BGR/Delta...) | RO | NC | Reliable identity read. |
| 0xAC / 0xAE | Horizontal / Vertical frequency (live) | RO | C | Live refresh telemetry. |
| 0xC8 | Display controller type (chip vendor) | RO | NC | Per-model quirk branching. |
| 0xC9 | Display firmware level | RO | NC | Firmware-update awareness. |
| 0xDF | VCP (MCCS) version | RO | NC | Branch quirks per protocol version. |
| 0x78 | Display Identification Data (EDID/DisplayID read) | RO | T | Read EDID blocks over DDC. |
| 0xD2 | Asset tag (16-byte, keyed write) | RW | T | Fleet/inventory tagging. |
| 0x73/0x74/0x75/0x76 | LUT size / point / block / RPC macro | RO/WO | T | Hardware LUT load on capable panels; niche. |
| 0x95-0x98 / 0x99 / 0x9A / 0xA5 | PBP window TL/BR coords / on-off / background / change window | RW | C/NC | Ultrawide/PBP panels only. |
| 0x66 | Ambient light sensor enable/read | RW | NC | Rare in consumer panels. |
| 0xFA-0xFD | Theft deterrence / timeout / low+high PIN | RW/WO | NC | HP shipped PINs as readable (CVE-2023-5449); audit, don't exploit. |
| 0xE0-0xFF | Manufacturer-specific | varies | varies | Undocumented, per-model, reverse-engineered. Bad writes can wedge the OSD. |

Sources: [ddcutil VCP info](https://www.ddcutil.com/vcpinfo_output/), [MCCS 2.2a spec](https://milek7.pl/ddcbacklight/mccs.pdf), [Twinkle Tray HP OMEN dump](https://github.com/xanderfrangos/twinkle-tray/issues/958), [Dell U3011 capabilities](https://www.ddcutil.com/cap_u3011_verbose_output/).

### 1a. Brightness-adjacent (contrast, color, sharpness, gamma)

- **Contrast (0x12)** â€” full image-quality control next to brightness. Mechanism: 0x12 continuous over the same path as 0x10. Feasibility: **Proven.** Win/mac/Linux. *Low-hanging fruit: already in Duja's `Feature` enum, just needs UI.*
- **Color temperature presets (0x14)** â€” one-click warm/cool white point (sRGB, 5000K-9300K, User). Mechanism: NC, enumerate values from capabilities since they differ per vendor. Feasibility: **Likely.** Caveat: the value list is not fixed; hard-coding it breaks on remapped panels. Win/mac/Linux.
- **Continuous color temperature (0x0C + 0x0B)** â€” exact Kelvin rather than fixed steps. Mechanism: value = (Kelvin - 3000) / increment read from 0x0B. Feasibility: **Hard.** Caveat: much rarer than 0x14; many panels ignore it. Win/mac/Linux.
- **RGB video gain / white-point trim (0x16/0x18/0x1A)** â€” per-channel hardware white balance. Mechanism: three continuous codes, 0-max (often 0-255). Feasibility: **Likely.** Widely present on desktop monitors. Win/mac/Linux.
- **RGB black level / offset (0x6C/0x6E/0x70)** â€” completes a 2-point grayscale calibration in hardware alongside gain. Mechanism: three continuous codes. Feasibility: **Hard.** Caveat: less commonly exposed than gain. Win/mac/Linux.
- **Backlight level (0x6B / legacy 0x13, plus per-color 0x6D/0x6F/0x71)** â€” drive the physical backlight for deeper dimming where 0x10 only scales the signal. Mechanism: continuous. Feasibility: **Hard.** Caveat: 0x13 is deprecated; 0x6B uncommon; behavior monitor-dependent. Win/mac/Linux.
- **Sharpness (0x87)** â€” edge-sharpness filter. Mechanism: treated as continuous on modern panels. Feasibility: **Likely.** Commonly supported. Win/mac/Linux.
- **Gamma select (0x72)** â€” switch hardware gamma curve (1.8/2.0/2.2/2.4). Mechanism: complex NC, vendor-specific encoding, enumerate from capabilities. Feasibility: **Hard.** Distinct from Duja's software gamma dimmer. Win/mac/Linux.
- **Hue & saturation (0x90 / 0x8A)** â€” overall image hue/saturation. Mechanism: continuous. Feasibility: **Hard.** Caveat: mostly TV-style panels with "movie/game" modes. Win/mac/Linux.
- **Six-axis color (0x59-0x5E saturation, 0x9B-0xA0 hue)** â€” per-primary CMS tuning. Mechanism: continuous, 0x7F nominal. Feasibility: **Hard.** Caveat: pro color monitors only. Win/mac/Linux.
- **Gray scale expansion (0x2E)** â€” occasional NC control (0-3). Feasibility: **Hard.** Niche. Win/mac/Linux.

### 1b. Input & layout (input source, PIP/PBP, KVM)

- **Input source switching (0x60)** â€” HDMI/DP/USB-C/DVI/VGA from the tray, the marquee "beyond brightness" feature. Mechanism: NC; standard values (0x0F DP-1, 0x11 HDMI-1, etc.) but *frequently vendor-remapped* (Samsung uses 0x05/0x06, USB-C often 0x1B or 0x1F). Feasibility: **Proven.** Caveat: read `60(...)` from capabilities and keep a per-model override table. Win/mac/Linux. *Low-hanging fruit: already in Duja's `Feature` enum.*
- **Output select (0xD0)** â€” pick the output on multi-output panels; same value table as 0x60. Feasibility: **Likely.** Win/mac/Linux.
- **Display scaling / aspect (0x86)** â€” fill / aspect-correct / 1:1 pixel-perfect / max image for console and retro inputs. Mechanism: NC. Feasibility: **Proven** where present (standard MCCS). Win/mac/Linux.
- **PIP / PBP layout (vendor codes, no MCCS standard)** â€” toggle split-screen/inset and assign inputs per pane. Mechanism: vendor VCP, e.g. Dell 0xE9 (0x00 single, 0x24 PBP), 0xE5=0xF001 swaps sources, 0xE8 sets sub-input. Feasibility: **Likely.** Caveat: encodings vary wildly per model and are sometimes hidden from the capabilities string; ship per-model maps and let users capture their own. Win/mac/Linux.
- **PBP window geometry (0x95-0x9A, 0xA5)** â€” position on-screen sub-windows on ultrawide/PBP panels. Mechanism: continuous coords + NC on/off. Feasibility: **Speculative** (only a subset of panels expose these standard codes). Win/mac/Linux.
- **Hardware KVM input switching (standard 0x60)** â€” flip the physical input as a desk KVM. Feasibility: **Proven.** Same caveat as 0x60 remapping. Win/mac/Linux.
- **Integrated USB-hub / KVM upstream switching (vendor codes)** â€” reassign which host owns the monitor's USB hub independent of video, i.e. a real KVM. Mechanism: Dell E7/E8/E9 range; E7=0xFF00 toggles USB hub owner in PIP/PBP. Feasibility: **Likely.** Caveat: codes differ per model, some firmware makes USB-routing read-only; discover by hooking the OEM Display Manager. Win/mac/Linux.
- **Audio source routing on multi-input panels** â€” play PC-B audio while showing PC-A video. Mechanism: vendor VCP near the PIP/USB codes, plus 0x62/0x8D. Feasibility: **Hard.** Caveat: not standardized, discover via OEM-app hooking. Win/mac/Linux.

Sources: [display-switch](https://github.com/haimgel/display-switch), [Dell U2723QE VCP gist](https://gist.github.com/lainosantos/06d233f6c586305cde67489c2e4a764d), [ScriptGod1337/kvm](https://github.com/ScriptGod1337/kvm), [USB soft-KVM writeup](https://www.tqdev.com/2025-usb-soft-kvm-monitor-switching-ddc-ci/).

### 1c. Power (DPMS / power mode 0xD6)

- **Power mode / DPMS (0xD6)** â€” standby/suspend/off from the tray, decoupled from Windows sleep, without cutting USB/charging. Mechanism: NC, 0x01 On through 0x05 hard off. Feasibility: **Proven** for turn-off. Caveat: waking a fully-off panel over DDC is often impossible (I2C controller unpowered), so pair wake with OS `SetThreadExecutionState` / `SC_MONITORPOWER` broadcast. Win/mac/Linux.
- **Auxiliary output power (0xD7)** â€” toggle power the display feeds a host device. Feasibility: **Speculative.** Very rare. Win/mac/Linux.

### 1d. OSD & miscellaneous (language, lock, reset, orientation, save, identity)

- **OSD language (0xCC)** â€” change menu language, includes Arabic (0x0F). Mechanism: NC, ~36 values. Feasibility: **Likely.** Trivially reuses the NC-enumeration path. Win/mac/Linux.
- **OSD / button lock (0xCA)** â€” disable the menu and lock physical buttons for kiosk/shared desks. Mechanism: complex NC, SH=OSD, SL=button lock. Feasibility: **Hard.** Caveat: interpretation is version-dependent. Win/mac/Linux.
- **Factory / preset restore (0x04/0x05/0x06/0x08/0x0A)** â€” one-click "reset this monitor" buttons. Mechanism: WO, any non-zero triggers. Feasibility: **Likely** (0x04 nearly universal). Win/mac/Linux.
- **Save current settings (0xB0, or protocol opcode 0x0C)** â€” persist tray changes into the monitor's NVRAM so they survive a power cycle. Feasibility: **Hard.** Caveat: inconsistent vendor support. Win/mac/Linux.
- **Screen orientation read + image flip (0xAA / 0x82 / 0x84)** â€” read physical rotation (can auto-trigger OS rotation) and flip in hardware. Feasibility: **Speculative.** Caveat: flips are rare; 0xAA read is more common. Win/mac/Linux.
- **Auto setup / auto color (0x1E / 0x1F / 0xA2)** â€” "fix my picture" for analog/VGA. Feasibility: **Hard.** Analog-only. Win/mac/Linux.
- **Analog & CRT geometry (0x20/0x30/0x22/0x32/0x0E/0x3E, plus CRT 0x24-0x4C)** â€” the full geometry menu for VGA/CRT setups. Feasibility: **Hard** for VGA, **Speculative** for CRT-only convergence/pincushion. Gate CRT codes on 0xB6=CRT. Win/mac/Linux.
- **Degauss (0x01)** and **TV channel up/down (0x8B)** â€” legacy CRT/tuner nostalgia controls. Feasibility: **Speculative.** Gate on display type. Win/mac/Linux.
- **New-control-value / active-control sync (0x02 / 0x52)** â€” keep Duja's UI in step when the user changes something on the physical OSD. Feasibility: **Likely** where implemented. Win/mac/Linux.
- **Performance preservation / anti-burn-in (0x54)** â€” enable pixel-orbit and static-image dimming in hardware. Feasibility: **Hard.** Rare. Win/mac/Linux.
- **"About this monitor" telemetry (0xC0 hours, 0xB6 tech, 0xB2 sub-pixel, 0xAC/0xAE live refresh, 0xC8 controller, 0xC9 firmware, 0xDF MCCS version)** â€” a read-only info panel with zero risk of misconfiguration. Feasibility: **Likely** (individual codes vary in support, but reads are safe). Win/mac/Linux.
- **Asset tag (0xD2)** â€” keyed 16-byte inventory field for fleets. Feasibility: **Hard.** Niche. Win/mac/Linux.
- **Hardware LUT load (0x73/0x74/0x75/0x76 table ops)** â€” push a calibration curve into the panel. Feasibility: **Hard.** Table-class, few panels. Win/mac/Linux.

### 1e. Audio (volume / mute)

- **Speaker volume + mute (0x62 / 0x8D)** â€” drive the monitor's own amp like OS media keys; 0x8D can also blank the screen. Feasibility: **Proven** on panels with speakers, otherwise absent. Caveat: monitor-dependent; 0x62 becomes NC with reserved special values in MCCS 2.2/3.0. Win/mac/Linux.
- **Treble / bass / balance / processor mode (0x8F / 0x91 / 0x93 / 0x94)** â€” full audio menu on multimedia panels. Feasibility: **Hard.** Rare. Win/mac/Linux.

### 1f. Cross-cutting foundations (make everything above reliable)

- **Capabilities-string parsing / dynamic UI** â€” query 0xF3, parse the returned VCP list and NC value sets, show only supported controls with real value lists. Feasibility: **Proven.** This is the single most important structural investment in the whole hardware layer. Win/mac/Linux. Sources: [Microsoft cap string](https://learn.microsoft.com/en-us/windows/win32/monitor/using-the-low-level-monitor-configuration-functions), [mccs-rs](https://github.com/arcnmx/mccs-rs).
- **Full VCP scan + raw get/set editor** â€” enumerate 0x00-0xFF and expose an advanced raw editor for power users. Feasibility: **Proven** (ddcutil, ControlMyMonitor do it). Caveat: writing arbitrary codes can wedge an OSD until power-cycle; gate writes behind confirmation. Win/mac/Linux.
- **Save/load full monitor config profiles** â€” snapshot all VCP values to JSON and restore per scenario (gaming/work/movies). Feasibility: **Proven.** Win/mac/Linux.
- **Per-display DDC calibration (min/max clamp, invert, code remap, curve skew)** â€” tame quirky panels the way MonitorControl does. Feasibility: **Proven.** Win/mac/Linux.
- **Relative +/- steps** â€” nudge a value without a read-modify-write round trip (nice for hotkeys/dials). Feasibility: **Proven.** Win/mac/Linux.
- **Vendor sub-VCP discovery engine** â€” many vendors pack dozens of features into one catch-all 0xE0-0xFF code with a nested sub-parameter (HP's design, per CVE-2023-5449). A capture/replay hook of the OEM app's `SetVCPFeature` calls lets Duja learn and re-expose them. Feasibility: **Likely.** This is the general key that turns "OEM-app-only" features scriptable. Win/mac/Linux. Source: [spaceraccoon](https://spaceraccoon.dev/hacking-display-monitors-monitor-command-control-set/).
- **Vendor code community database** â€” ship a signed, EDID-keyed model->code map (like ddccontrol-db / ddcutil user-defined-feature files) filled with non-standard input values, PIP/KVM/RGB codes, crowdsourced from users' capability dumps. Feasibility: **Likely.** This is what makes the messy real-world monitor population "just work." Win/mac/Linux. Source: [ddccontrol-db](https://github.com/ddccontrol/ddccontrol-db/pull/71).

---

## 2. Hardware: beyond MCCS

These reach past the standard command set: USB-C PD, refresh/VRR, HDR at the panel, USB hubs, ambient sensors, RGB lighting, vendor extensions, and the low-level DisplayPort transport.

- **EDID / DisplayID identity parsing** â€” real model, serial, manufacture week/year, native timings, panel size and technology, for robust per-monitor profiles. Mechanism: read the 128/256-byte EDID over I2C addr 0x50 (tunneled over AUX on DisplayPort), or on Windows just read the cached blob under `HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY` with no I2C round-trip. Feasibility: **Proven.** Win/mac/Linux. *This is the foundation for keying every per-monitor setting.* Source: [EDID reference](https://en.wikipedia.org/wiki/Extended_Display_Identification_Data).
- **Vendor-specific VCP range (0xE0-0xFF)** â€” bind model-specific features (LG Black Stabilizer, backlight strobe, overdrive, presets). Mechanism: undocumented per-model codes, discovered via the sub-VCP hook. Feasibility: **Speculative** to blind-expose; **Likely** as an opt-in read-only prober plus community map. Caveat: bad writes can brick an OSD; gate writes. Win/mac/Linux.
- **Gaming-overlay features (crosshair, black equalizer, FPS counter, aim stabilizer)** â€” the stuff Gigabyte OSD Sidekick / ASUS DisplayWidget / MSI Gaming OSD expose. Mechanism: vendor 0xE0-0xFF over the USB-upstream link, obtained via sub-VCP discovery. Feasibility: **Hard.** Caveat: no standard codes, USB-upstream required. High novelty because no lightweight tool does it today. Win/mac/Linux.
- **Refresh-rate switching (OS-level, listed here for adjacency)** â€” 60/120/144/240 Hz toggles, per-app refresh. Mechanism: `ChangeDisplaySettingsEx` / `QueryDisplayConfig`+`SetDisplayConfig` (Win), `CGDisplaySetDisplayMode` (mac), RandR/DRM (Linux). Feasibility: **Proven.** Win/mac/Linux.
- **VRR / FreeSync / G-Sync toggle** â€” enable adaptive sync globally or per-game. Mechanism: no portable API; NVAPI (NVIDIA), ADL/AGS (AMD), plus a flaky Windows system flag; FreeSync range needs an EDID/CTA-861 override, not a live call. Feasibility: **Hard.** Caveat: driver-specific and historically unstable across Windows builds. Win/Linux. Source: [NVAPI dispcontrol](https://docs.nvidia.com/nvapi/group__dispcontrol.html).
- **USB-C Power Delivery awareness** â€” show wattage the monitor feeds the laptop, warn on insufficient PD or a throttling cable. Mechanism: PD is negotiated by USB-C PD controllers and **not exposed over DDC on essentially any consumer monitor**, so it is read-only host-side (Windows `UsbPmApi`, WMI `Win32_Battery` charge rate, some docks via HID). Feasibility: **Speculative.** Frame strictly as diagnostics, not control. Win/mac/Linux.
- **Ambient light sensor (panel-side, 0x66 or vendor)** â€” read a built-in ALS to drive auto-brightness. Mechanism: 0x66 enable/read, or a vendor code on pro/medical panels. Feasibility: **Speculative.** Caveat: very few consumer monitors surface it; treat as probe-and-fallback to the PC's own sensor or webcam. Win/mac/Linux.
- **Cross-vendor RGB / ambient lighting** â€” control the light bars on gaming monitors and sync to screen/audio, replacing Aura/Mystic Light bloat. Mechanism: monitor RGB is almost never on DDC; it rides USB HID or SMBus with proprietary protocols. Integrate the [OpenRGB](https://openrgb.org/) network SDK rather than reimplementing each. Feasibility: **Likely** via OpenRGB. Win/Linux.
- **Monitor firmware update awareness** â€” read installed firmware (0xC9), cross-check a version manifest, and guide the vendor updater. Mechanism: reading is safe; actual flashing is proprietary (Dell DDPM over USB-upstream, LG/NVIDIA updaters) and out of scope. Feasibility: **Hard** (awareness tier only; re-implementing the flash is risky/unsupported). Win/mac. Source: [Dell firmware KB](https://www.dell.com/support/kbdoc/en-us/000131809/steps-for-updating-the-firmware-for-your-dell-monitor).
- **DisplayPort AUX / I2C-over-AUX / DPCD access** â€” reach panel data plain DDC cannot: raw EDID re-reads, DPCD link/sink registers, MST config. Mechanism: on DP there are no SCL/SDA pins; I2C is tunneled over AUX; on Windows this generally needs a GPU-vendor I2C/AUX driver path. Feasibility: **Hard.** Caveat: not exposed by the standard Monitor Configuration API. Win/Linux. Source: [ddcutil DisplayPort](https://www.ddcutil.com/displayport/).
- **MST hub topology mapping** â€” map which physical panel sits where behind a daisy-chained/hub-split DisplayPort. Mechanism: DPCD registers over AUX, correlated with GDI/CCD display paths. Feasibility: **Hard.** Ensures per-monitor controls target the right panel over one cable. Win/Linux.
- **Anti-theft PIN audit (0xFA-0xFD)** â€” surface (don't exploit) whether a panel exposes theft-deterrence PINs as readable, a security check (HP's CVE-2023-5449). Feasibility: **Speculative.** Niche, but demonstrates deep MCCS coverage. Win/mac/Linux.
- **XDR / over-drive brightness unlock** â€” push mini-LED/XDR panels past the OS nit cap (500 -> 1600 nits) via EDR-range manipulation. Mechanism: OS/panel-specific, Lunar Pro does it on macOS. Feasibility: **Speculative.** Caveat: highly hardware-dependent, minimal Windows analog. macOS. Source: [Lunar Pro](https://lunar.fyi/pro).

---

## 3. Software / OS-level (no DDC needed)

These work on internal laptop panels and DDC-less displays, and underpin sub-zero dimming (which Duja already does). They are gamma/LUT, overlays, OS display config, ICC, and accessibility filters. Reliability is OS-version and GPU-driver sensitive.

### Windows

- **Gamma-ramp brightness/dimming** â€” dim/brighten any display including below the hardware floor by rewriting the GPU LUT. Mechanism: GDI `SetDeviceGammaRamp` on a per-monitor HDC. Feasibility: **Likely.** Caveat: Microsoft discourages it, ramps failing readability heuristics silently no-op (returns TRUE), and it fails on some drivers. Windows.
- **Software color-temperature shift (f.lux-style)** â€” warm the screen by biasing the blue/green ramps. Mechanism: same `SetDeviceGammaRamp` path. Feasibility: **Likely.** Windows/Linux.
- **WinRT BrightnessOverride / DisplayEnhancementOverride** â€” officially-supported per-app/system brightness (and color) override on integrated panels. Mechanism: `Windows.Graphics.Display.BrightnessOverride`. Feasibility: **Likely.** Caveat: integrated displays that expose controllable brightness only. Windows.
- **Magnification full-screen color effects** â€” desktop-wide invert / grayscale / sepia-warm / colorblind simulation via a 5x5 color matrix; can double as a dimmer. Mechanism: `MagSetFullscreenColorEffect`. Feasibility: **Proven.** Same engine as Windows Color Filters. Windows.
- **Layered black overlay dimmer (per-monitor, sub-zero)** â€” click-through translucent black window per monitor. Mechanism: `WS_EX_LAYERED | WS_EX_TRANSPARENT` topmost window + `SetLayeredWindowAttributes`. Feasibility: **Proven.** *Duja already does this.* Windows (analogous on mac/Linux).
- **Exclude overlay from capture** â€” make the dim/UI invisible to screenshots and recorders. Mechanism: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` (Win10 2004+). Feasibility: **Proven.** Pairs with the overlay dimmer. Windows.
- **Resolution / refresh / orientation (legacy)** â€” mode switch, rotate, refresh per display. Mechanism: `ChangeDisplaySettingsEx` + `EnumDisplaySettingsEx`. Feasibility: **Proven.** Windows.
- **Atomic multi-display config, arrangement, primary switch (modern)** â€” save/restore whole topologies (resolution, layout, primary, scaling, orientation, bit depth, refresh) in one call. Mechanism: `QueryDisplayConfig` / `SetDisplayConfig`. Feasibility: **Proven.** Windows.
- **Per-monitor HDR / Advanced Color toggle** â€” turn HDR on/off for a single display (unlike global Win+Alt+B). Mechanism: `DisplayConfigSetDeviceInfo` SET_HDR_STATE (24H2+) or SET_ADVANCED_COLOR_STATE. Feasibility: **Proven.** Windows.
- **HDR-aware SDR brightness slider** â€” when HDR is on, control SDR-content white level (DDC writes are blocked under HDR). Mechanism: Windows SDR white-level/HDR balance. Feasibility: **Likely.** Windows. *Directly complements Duja's brightness core for HDR users.*
- **ICC color-profile management + calibration loader** â€” assign per-display ICC profiles and load their VCGT curves into the GPU LUT. Mechanism: WCS/ICM `WcsAssociateColorProfileWithDevice` + `WcsSetCalibrationManagementState`. Feasibility: **Likely.** Windows.
- **Virtual monitors via Indirect Display Driver (IDD)** â€” software-only displays for headless/streaming/extra desktop. Mechanism: a UMDF2 driver on IddCx. Feasibility: **Likely** (proven pattern; shipping a driver raises Duja's footprint well past "ultra-lightweight," so weigh carefully). Windows.
- **Per-monitor DPI / scaling** â€” change a specific monitor's scale factor. Mechanism: undocumented `DisplayConfigGetDeviceInfo/SetDeviceInfo` types -3/-4, or `PerMonitorSettings` registry. Feasibility: **Hard.** Caveat: undocumented API. Windows.
- **Accessibility color filters / grayscale toggle** â€” flip the OS Color Filters from the tray (persists across sessions). Mechanism: `ColorFiltering` registry + WNF signal, or the Magnification matrix. Feasibility: **Likely.** Windows.
- **Cursor size/color + text scale** â€” Ease-of-Access controls from the tray. Mechanism: `CursorBaseSize` / `TextScaleFactor` registry + broadcast. Feasibility: **Likely.** Windows.
- **Per-window color/dim via DirectComposition** â€” tint/dim individual windows rather than the whole desktop. Mechanism: `IDCompositionDevice` color-matrix or clipped solid visual tracked via `SetWinEventHook`. Feasibility: **Hard.** Windows.
- **Per-monitor usage/uptime stats** â€” since no reliable runtime-hours VCP exists, accumulate app-side by polling power state (0xD6) and brightness per EDID serial. Feasibility: **Likely.** Windows/mac/Linux.
- **Dead-pixel / uniformity / burn-in test patterns** â€” fullscreen R/G/B/W/K flats and rapid flicker per monitor, plus a stuck-pixel exerciser. Mechanism: pure software, target the panel via EDID identity. Feasibility: **Proven.** Low-risk. Win/mac/Linux.

### macOS

- **Gamma / LUT control (CoreGraphics)** â€” software brightness/dim/color-temp. Mechanism: `CGSetDisplayTransferByFormula` / `ByTable`. Feasibility: **Hard.** Caveat: reportedly **silently ignored on macOS Tahoe and M5-class Apple Silicon**, breaking f.lux/Lunar/MonitorControl; treat as unreliable on newest hardware. macOS.
- **Built-in-panel brightness via private CoreDisplay/DisplayServices** â€” true backlight on internal (and some external) Apple displays without DDC. Mechanism: `CoreDisplay_Display_SetUserBrightness` + `DisplayServices*`. Feasibility: **Hard.** Caveat: undocumented, reliability gaps on Apple Silicon. macOS.
- **Night Shift control (private CoreBrightness)** â€” toggle/set strength and CCT. Mechanism: `CBBlueLightClient`. Feasibility: **Likely** (reliable across versions despite being unofficial). macOS.
- **Resolution / HiDPI scaling / refresh** â€” per-display mode change. Mechanism: `CGDisplayCopyAllDisplayModes` / `CGDisplaySetDisplayMode`. Feasibility: **Likely.** Caveat: requested mode can map to a different actual resolution. macOS.
- **Desktop-wide grayscale / invert** â€” Mechanism: `CGDisplayForceToGray`, plus MADisplayFilter private APIs for invert. Feasibility: **Likely.** macOS.
- **Virtual display (CGVirtualDisplay)** â€” headless/extra screen. Mechanism: private `CGVirtualDisplay*`. Feasibility: **Hard.** macOS.

### Linux

- **X11 gamma / brightness / color-temp (XRandR)** â€” per-CRTC gamma ramps. Mechanism: `XRRSetCrtcGamma`. Feasibility: **Proven** (the Redshift mechanism). Linux.
- **Wayland gamma / night-light protocol** â€” Mechanism: `wlr-gamma-control-unstable-v1` on wlroots compositors (sway/wayfire/niri/cosmic); GNOME/KDE keep night-light in their own settings. Feasibility: **Likely.** Caveat: compositor-dependent. Linux.
- **DRM/KMS color pipeline (GAMMA_LUT / DEGAMMA_LUT / CTM)** â€” kernel-level gamma and 3x3 color matrix, works where X gamma doesn't (ARM). Mechanism: libdrm atomic commits. Feasibility: **Hard.** Lowest-level, compositor-independent. Linux.
- **Resolution / refresh / rotation / primary / custom modes (XRandR)** â€” full mode management including custom modelines. Mechanism: `XRRSetCrtcConfig` / `XRRSetOutputPrimary` / `XRRCreateMode`. Feasibility: **Proven.** Linux.
- **Virtual monitors (EVDI / DRM writeback / VIRTUAL output)** â€” software displays for streaming/headless. Feasibility: **Hard.** Linux.

---

## 4. Automation & UX

This is where a brightness tool becomes a brightness *system*. All of these wrap the mechanisms above with timers, sensors, and rules. Most are **Proven** because Twinkle Tray, Monitorian, Lunar, and f.lux already ship them; the differentiation for Duja is doing them natively, lightly, and cross-platform.

- **Time-of-day schedule (named blocks)** â€” morning/evening/night blocks, each with per-monitor brightness applied as the clock crosses each threshold. Mechanism: in-process tick loop over sorted trigger times, fan out over the existing DDC/WMI backend. Feasibility: **Proven.** Win/mac/Linux. *Low-hanging fruit: pure logic on top of today's brightness path.*
- **Sunrise/sunset schedule (geolocation)** â€” transitions anchored to local solar events, recomputed daily. Mechanism: SunCalc/NOAA solar math from lat/long (Win Geolocation, CoreLocation, geoclue, or IP fallback), with offsets like "30 min before sunrise." Feasibility: **Proven.** Win/mac/Linux.
- **Solar-elevation adaptive curve** â€” continuously interpolate brightness along the sun's altitude rather than snapping at events (Lunar Location Mode). Feasibility: **Proven.** Win/mac/Linux.
- **Hardware ALS adaptive brightness (external/wireless sensor)** â€” read lux from a USB/ESP lux board and map to brightness/contrast with a learned curve; one sensor can serve several machines. Mechanism: Windows Sensor API `LightSensor`, macOS ambient, or a cheap VCNL4040/APDS-9930. Feasibility: **Likely.** Win/mac/Linux.
- **Sync internal-panel adaptive brightness to externals** â€” when the laptop's own auto-brightness moves the built-in panel, mirror it to external displays (Lunar Sync Mode). Mechanism: watch WMI/ACPI backlight (Win) or built-in ALS (mac), propagate deltas. Feasibility: **Likely.** Caveat: needs a per-monitor mapping curve. Win/mac.
- **Webcam-as-ALS** â€” sample frames, average luminance, drive brightness. Feasibility: **Hard.** Caveat: most webcams have non-disableable auto-gain/exposure that corrupt absolute lux, so it only works as a relative/hysteresis signal, and it steals the camera plus trips the privacy LED. Win/mac/Linux.
- **Screen-content adaptive dimming (Gammy-style)** â€” sample the desktop and adjust to keep perceived luminance steady. Mechanism: DXGI Desktop Duplication (Win) + gamma ramps, so it works DDC-free and doubles as sub-zero dimming. Feasibility: **Proven.** Win/Linux.
- **App/window-aware profiles** â€” auto-apply a brightness/temperature profile per focused app (boost for design, warm for reading, pause entirely for color-critical work). Mechanism: `GetForegroundWindow` + process name (Win), `NSWorkspace` frontmost (mac), matched against a rule table. Feasibility: **Proven.** Win/mac.
- **Fullscreen / game / video-aware behavior** â€” don't dim mid-movie or mid-match; disable filters on fullscreen. Mechanism: `SHQueryUserNotificationState` (QUNS_RUNNING_D3D_FULL_SCREEN) or compare foreground bounds to the monitor rect. Feasibility: **Proven.** Win/mac.
- **Presets / scenes** â€” named one-click multi-monitor states (per-display brightness/contrast/input/temperature). Mechanism: serialize per-display settings, iterate on apply. Feasibility: **Proven.** Win/mac/Linux. *Low-hanging fruit given the existing backend.*
- **Sync groups (one slider, all displays)** â€” link monitors so one control fans out. Feasibility: **Proven.** Trivial over the per-display backend. Win/mac/Linux.
- **Per-monitor offsets / normalization** â€” a signed offset per display so a single group level lands visually matched despite panel differences (optionally normalized by measured nits). Feasibility: **Proven.** Win/mac/Linux.
- **Global hotkeys** â€” raise/lower/set brightness (or contrast), target one or all monitors, toggle displays, switch presets. Mechanism: `RegisterHotKey` / low-level hook (Win), Carbon/CGEvent (mac). Feasibility: **Proven.** Win/mac/Linux. *Natural extension of the brightness path.*
- **Brightness media-key passthrough** â€” capture the keyboard's brightness keys and route them to external monitors with a native-style OSD (MonitorControl's signature). Feasibility: **Proven.** Win/mac.
- **Gradual / eased transitions** â€” ramp between levels with easing instead of a jump. Mechanism: interpolate on a timer. Feasibility: **Proven.** Caveat: DDC write latency limits step rate, so gamma-based fades are smoother for fine steps. Win/mac/Linux.
- **Idle dimming** â€” dim after N minutes of no input, restore on activity. Mechanism: `GetLastInputInfo` (Win), `CGEventSourceSecondsSinceLastEventType` (mac). Feasibility: **Proven.** Win/mac/Linux.
- **Presence / away detection** â€” dim or lock when you step away. Mechanism: cheap via session-lock/idle; richer via webcam face presence or Windows Human Presence sensors (some ASUS ROG OLEDs even have a Neo Proximity Sensor). Feasibility: **Hard** for the camera/sensor path, **Proven** for the session-lock path. Win/mac.
- **Sub-zero dimming (below hardware minimum)** â€” overlay or gamma to go past the DDC floor. Feasibility: **Proven.** *Duja already does this;* the extension is auto-learning sub-zero values inside adaptive modes. Win/mac/Linux.
- **Auto-learning adaptive curve** â€” record manual corrections against the current signal (lux / sun elevation / time) and fold them into the curve so it self-tunes (Lunar). Feasibility: **Likely.** Win/mac/Linux.
- **Adaptive contrast** â€” a second interpolation curve on 0x12 in lockstep with the backlight curve so low-light stays legible and daytime stays punchy. Feasibility: **Proven.** Win/mac/Linux. *Uses the contrast code already in the enum.*
- **Per-input-source brightness memory** â€” remember distinct settings per input so switching HDMI<->DP (work laptop vs personal PC) restores the right level. Mechanism: key the profile on the 0x60 value. Feasibility: **Likely.** Natural extension of Duja's input-source support. Win/mac/Linux.
- **Conditional / trigger-based profile switching** â€” a rules engine keyed on power source, SSID, docked/undocked monitor set (by EDID), time, or app (Monitorian's Conditional/Time Commands). Feasibility: **Proven.** Win/mac/Linux.
- **Color-temperature scheduling** â€” warm the white point at night on a sunrise/sunset or clock schedule, independent of backlight (f.lux 1200-6500K, Gammy 2000-6500K). Feasibility: **Proven.** Win/mac/Linux.
- **Named eye-strain / special modes** â€” Movie (preserve shadow/sky detail), Darkroom (red, inverted), Grayscale (kill color distraction). Mechanism: preset gamma/LUT + inversion via the OS color pipeline. Feasibility: **Proven.** Win/mac/Linux.
- **Health-break / 20-20-20 timer** â€” recurring look-away/stand-up reminders, optionally a gentle full-screen break overlay (Duja already owns an overlay layer). Feasibility: **Proven.** Win/mac/Linux.
- **FaceLight / video-call key-light** â€” a hotkey pushes brightness/contrast to max and lays a warm-white overlay to light your face in a dark room (Lunar Ctrl+Cmd+5). Feasibility: **Proven.** Win/mac/Linux.
- **BlackOut / soft-disable a display** â€” black out one monitor (mirror or backlight-off) while keeping its USB hub and charging alive. Mechanism: 0xD6 standby, or backlight 0 + black overlay when the panel can't sleep without dropping USB. Feasibility: **Likely.** Win/mac.
- **Wake/sleep chronotype schedule** â€” anchor the warm ramp to the user's actual wake and bedtime, not just the sun (f.lux). Feasibility: **Proven.** Win/mac/Linux.
- **OLED burn-in care** â€” track runtime and static-content exposure, nudge users to run pixel refresh, and apply app-side mitigations (pixel-shift, taskbar-region dimming, DPMS-off on idle). Mechanism: vendors auto-run pixel refresh with no documented DDC trigger, so the realistic play is reminder + mitigation, probing 0xE0-0xFF in case a model exposes a trigger. Feasibility: **Hard.** Win/mac/Linux.

---

## 5. Integrations & extensibility

Turning Duja from an app into a control surface other things can drive, plus calibration.

- **CLI / scripting** â€” full command-line control of brightness and every VCP, with display selectors by name/model/serial/number, for scripts, Task Scheduler, AutoHotkey, macros. Mechanism: a companion CLI or subcommand (Lunar, Twinkle Tray, ddcutil are the models). Feasibility: **Proven.** Win/mac/Linux. *Direct wrap of the existing control surface, a strong early integration win.*
- **Local API server (UDP/REST) + remote control** â€” a localhost endpoint so other apps and even other machines can drive DDC (Twinkle Tray's localhost UDP, Lunar `--remote`). Feasibility: **Likely.** Win/mac/Linux.
- **Home Assistant / MQTT bridge** â€” expose each monitor as an MQTT light with HA auto-discovery for brightness/power/input in smart-home automations. Mechanism: publish `ddc-monitor/{i}/brightness/set|get` topics. Feasibility: **Proven** (ddc-mqtt, HASS.Agent). Win/Linux/mac.
- **Elgato Stream Deck plugin** â€” buttons/dials for brightness, contrast, input switching. Mechanism: a plugin calling Duja's CLI/API. Feasibility: **Proven.** Win/mac.
- **OS automation / voice hooks** â€” macOS Shortcuts/Siri actions, Windows via CLI from Task Scheduler/AutoHotkey/PowerToys. Feasibility: **Likely.** Win/mac.
- **MCP server for AI/agent control** â€” let an LLM/agent read and set monitor state through Model Context Protocol. Mechanism: wrap the DDC surface in an MCP server (ddc-ci-control-bridge already ships one). Feasibility: **Likely.** Win/mac/Linux.
- **Bias-lighting / smart-light sync** â€” drive Philips Hue / Nanoleaf / WLED behind the monitor as static bias light or screen-color match. Mechanism: sample screen edges, push to each vendor API. Feasibility: **Likely.** Win/mac/Linux.
- **Colorimeter-driven closed-loop calibration** â€” integrate an i1Display/Calibrite/Spyder so Duja measures the screen and drives 0x10 + 0x16/0x18/0x1A to hit a target white point/gamma before writing an ICC/LUT (DisplayCAL from the tray). Mechanism: shell out to ArgyllCMS for instrument access. Feasibility: **Likely.** Win/mac/Linux.
- **Nits-based calibrated targeting** â€” set and sync brightness in real nits rather than 0-100 so mismatched panels match perceptually. Mechanism: a per-model 0-100 -> measured-nits mapping (manual entry or EDID luminance metadata). Feasibility: **Hard.** Win/mac.
- **Monitor-as-USB-hub awareness** â€” correlate a monitor (EDID serial) with its USB hub via the OS device tree so "devices follow the active input" logic knows which peripherals hang off which display. Mechanism: SetupAPI/WMI `Win32_USBHub` containerId. Feasibility: **Likely.** Win/mac/Linux.
- **Secondary sensor-panel / desk-accessory display driver** â€” push Duja's own telemetry (brightness, input, uptime) to a cheap USB mini-LCD (AIDA64-style), or accept one as a physical brightness knob. Feasibility: **Likely.** Win/Linux.
- **Per-model capability database (community feature packs)** â€” the crowdsourced EDID-keyed override map from Section 1f, framed as an integration: users upload anonymized capability dumps, the community labels overdrive/KVM/PIP/RGB/telemetry codes. Feasibility: **Likely.** Win/mac/Linux.

Sources: [Lunar CLI](https://lunar.fyi/), [ddc-ci-control-bridge](https://github.com/Defozo/ddc-ci-control-bridge), [DisplayCAL/ArgyllCMS](https://displaycal.net/), [OpenRGB](https://openrgb.org/).

---

## 6. Speculative / research-grade ideas

Clearly flagged as experiments. These are worth writing down but carry the weakest feasibility, the thinnest hardware support, or the biggest platform risk.

- **Auto-KVM (follow-the-mouse / follow-the-USB)** â€” automatically flip input + USB to whichever machine the user is touching, turning a plain dual-input monitor into an automatic KVM. Mechanism: detect a trigger (USB arrival via `WM_DEVICECHANGE`, signal loss, hotkey, or a cross-PC heartbeat) then issue 0x60 + vendor USB-upstream writes. Feasibility: **Likely** as an idea, **Speculative** to make robust across the vendor-code mess. Win/mac/Linux.
- **Panel-side ambient light sensor as the auto-brightness source (0x66 / vendor)** â€” clean hardware loop where present, but almost no consumer panel surfaces it. Feasibility: **Speculative.**
- **Manufacturer 0xE0-0xFF "raw VCP" advanced panel** â€” let power users bind arbitrary vendor codes with a crowd-sourced map. Feasibility: **Speculative.** Caveat: bad writes can wedge a monitor's OSD until power-cycle; must be strongly gated.
- **Gaming-overlay features via sub-VCP discovery (crosshair, black equalizer, aim stabilizer)** â€” high novelty, no lightweight tool does it, but entirely dependent on reverse-engineering per model over USB-upstream. Feasibility: **Hard/Speculative.**
- **USB-C PD wattage negotiation control** â€” not just reading PD but influencing it. Feasibility: **Speculative** and essentially not exposed; diagnostics-only is the honest ceiling.
- **XDR / EDR over-drive brightness unlock** â€” push past the OS nit cap. Feasibility: **Speculative,** macOS-only, panel-specific.
- **CRT / TV nostalgia suite (degauss 0x01, convergence, TV tuner 0x8B)** â€” full legacy coverage for retro/AV enthusiasts. Feasibility: **Speculative** (correct but a vanishing hardware base). Gate on 0xB6/technology type.
- **Anti-theft PIN security auditor (0xFA-0xFD)** â€” surface insecure PIN exposure as a security feature. Feasibility: **Speculative,** niche.
- **Webcam / IR human-presence and BLE-phone-RSSI presence** â€” richer away-detection than session-lock. Feasibility: **Hard/Speculative,** privacy-sensitive.
- **Indirect/virtual display drivers as a shipped feature** â€” powerful (headless/streaming targets) but shipping a UMDF2/IddCx driver, an EVDI module, or private CGVirtualDisplay usage is a large surface-area and stability bet that cuts against Duja's "ultra-lightweight" identity. Feasibility: **Likely** technically, **Speculative** as a fit.
- **DisplayPort AUX / DPCD / MST deep access** â€” unlocks data plain DDC can't reach, but needs GPU-vendor driver paths and is not in the standard API. Feasibility: **Hard/Speculative.**
- **Monitor firmware flashing (not just awareness)** â€” re-implementing a vendor flash is risky, unsupported, and can brick panels. Feasibility: **Speculative,** recommend awareness-only.

---

## Suggested triage buckets

A starting point for prioritization, not a commitment. Grouped by effort-to-value against where Duja's code is today.

**Quick wins (adjacent to today's code, mostly wiring + UI)**
- Contrast (0x12) UI â€” already in the `Feature` enum.
- Input source (0x60) UI with a per-model value-override table â€” already in the enum.
- Capabilities-string parsing to build the control list dynamically (the structural unlock for everything else).
- Power mode / DPMS (0xD6) with an OS-broadcast wake fallback.
- Color presets (0x14) and RGB gain (0x16/0x18/0x1A) via the NC/continuous paths.
- Factory reset (0x04) and OSD language (0xCC) via the NC path.
- "About this monitor" read-only info panel (0xB6/0xC0/0xC9/0xDF/0xAC/0xAE + EDID).
- Presets/scenes, sync groups, per-monitor offsets, global hotkeys, gradual transitions, idle dimming, time-of-day and sunrise/sunset schedules, auto-learning sub-zero (all pure logic over the existing brightness path).
- A CLI wrapping the existing control surface.
- Dead-pixel/burn-in test patterns and FaceLight (both ride the overlay Duja already has).

**High-value mid-term**
- Speaker volume/mute (0x62/0x8D) with media-key passthrough.
- Save/load full monitor config profiles and per-display DDC calibration (min/max, invert, remap, skew).
- Software gamma color-temperature shift and content-aware (Gammy-style) dimming for DDC-less/internal panels.
- App/window-aware and fullscreen-aware profiles; conditional/trigger rules engine (dock/undock, AC/battery, SSID).
- Hardware ALS adaptive brightness (USB lux sensor) and sync-internal-to-external.
- Per-input-source brightness memory; adaptive contrast.
- Local API server + Home Assistant/MQTT + Stream Deck + MCP integrations.
- Per-monitor HDR toggle and HDR-aware SDR brightness slider (Windows); ICC profile switching.
- BlackOut (keep USB/charging alive).
- Per-model community capability database.

**Ambitious / differentiators**
- Vendor sub-VCP discovery engine (hook the OEM app, learn 0xE0-0xFF), unlocking PIP/PBP, hardware KVM/USB-upstream, and gaming-overlay features.
- Auto-KVM (follow-the-USB) built natively and cross-machine.
- Colorimeter closed-loop calibration and nits-based targeting.
- Cross-vendor RGB/bias lighting via OpenRGB.
- OLED burn-in care suite.
- Multi-display arrangement/topology profiles with dock-aware auto-apply.
- Cross-platform parity for the gamma/overlay/night-shift layers (XRandR/Wayland on Linux, CoreBrightness/CoreDisplay on macOS), with an honest reliability note about macOS Tahoe/M5 silently ignoring gamma calls.

**Speculative / park**
- Manufacturer raw-VCP advanced panel (gated writes) and gaming-overlay toggles.
- Panel-side ALS (0x66) as a first-class source.
- USB-C PD as anything beyond read-only diagnostics.
- XDR/EDR over-drive unlock (macOS).
- CRT/TV nostalgia controls and anti-theft PIN auditing.
- Webcam/IR/BLE presence detection.
- Virtual/indirect display drivers (weigh hard against the lightweight identity).
- DisplayPort AUX/DPCD/MST deep access and firmware flashing (awareness-only, not flashing).

A closing thought on identity: Duja's edge is being small, fast, and Rust-clean. Several ideas here (IDD/virtual displays, driver-level RGB, firmware flashing) are genuinely useful yet pull hard against that. The capabilities-parsing foundation plus the automation layer is where a light tool can out-punch heavier competitors without gaining weight, so those buckets probably deserve the first look.
# Linux packaging

Duja ships one Linux artifact today: a **portable tarball**,
`duja-<version>-linux-x64.tar.gz`, staged by
[`xtask`](../../xtask/src/dist.rs).

```sh
cargo build --release -p duja-app -p dujactl
cargo run --release -p xtask -- dist --version 0.3.0
```

That produces, under `target/dist/`:

- `duja-<version>-linux-x64/`: the staging tree
- `duja-<version>-linux-x64.tar.gz`: the tarball, with the tree at its root

The tree holds `duja`, `dujactl`, both licences, `README.md`, this directory's
`duja.desktop`, and `duja.png` (the brand mark, taken from `docs/images/` so
there is one file rather than two that can drift).

## Why a tarball and not an AppImage or a `.deb`

Because neither can be built responsibly yet, and the reason is the same for
both: **nobody has run Duja on a Linux desktop.** No CI runner has a
`StatusNotifierWatcher`, an X server or a compositor, and the Windows
development box cannot build `duja-app` for Linux at all
(`yeslogic-fontconfig-sys` wants a pkg-config sysroot).

That matters more for a package than for an archive, because a package makes a
**claim** an archive does not:

- a `.deb` or `.rpm` declares its runtime dependencies, and a wrong `Depends:`
  is an install that succeeds and an application that will not start;
- an AppImage bundles its libraries, and one missing from the `AppDir` fails the
  same way, later and with more machinery in between.

Both would be a guess presented as a supported package. A tarball claims only
that these are the bytes, which is true and checkable. It is also what the phase
gate needs in order to test anything at all; the AppImage question is answerable
*after* someone has run the binary, not before.

Tracked in [`docs/debt.md`](../../docs/debt.md).

## What the binary needs at runtime

Dynamically linked, so the host supplies these. Named here because the tarball
has no dependency metadata to carry them, and a missing one is a linker error at
launch rather than anything Duja can report:

| need | Debian/Ubuntu | Fedora |
|---|---|---|
| font configuration | `libfontconfig1` | `fontconfig` |
| keyboard handling | `libxkbcommon0` | `libxkbcommon` |
| X11 session | `libx11-6`, `libxcb1`, `libxrandr2` | `libX11`, `libxcb`, `libXrandr` |
| Wayland session | `libwayland-client0` | `wayland-libs-client` |
| tray | a `StatusNotifierItem` host (see below) | same |

The X11 and Wayland rows are alternatives, not both: Duja picks the transport
from `WAYLAND_DISPLAY` and `DISPLAY` at runtime
([ADR-0011](../../docs/adr/0011-linux-software-dimming.md)). `dujactl doctor`
reports what the session actually offers.

**The tray needs a host**, and that is not a shared library.
[ADR-0010](../../docs/adr/0010-linux-tray-ksni.md) chose the freedesktop
`StatusNotifierItem` protocol, which KDE Plasma implements natively; GNOME needs
the AppIndicator extension, and most wlroots panels (Waybar, `ags`) support it
directly. Without a host the tray icon simply never appears: the process runs,
`dujactl` works, and there is nothing to click.

## What the session has to offer, and what it cannot

`dujactl doctor` answers all of this for the machine in front of you, which is
why the README sends a first-time Linux user there before anything else. The
reference version:

**External monitors need `i2c-dev`.** Linux DDC/CI talks over `/dev/i2c-*`, so
the kernel module has to be loaded and you need read/write access to the nodes.
The module ships no udev rule, so on a stock system those nodes are root-only
and no `i2c` group exists at all: installing `i2c-tools` (or `ddcutil`) adds the
rule and the group together, and you then join it. Both steps are required.
Without them external monitors do not appear; the built-in panel is unaffected.
`doctor` reports which of the two is missing and prints the remedy.

**Software dimming is probed per session, not per desktop.** On Wayland the
overlay needs `wlr-layer-shell`, and the sub-floor gamma channel needs
`wlr-gamma-control`. Duja asks the session which it offers, and for gamma
whether it can actually take it: a compositor can advertise the protocol and
still refuse, for instance when `wlsunset` already holds it. The answer is per
session rather than by desktop name ([ADR-0011](../../docs/adr/0011-linux-software-dimming.md)),
so this is not a list of supported compositors. GNOME's Mutter is widely
reported to offer neither, and where that holds Duja reports software dimming
unavailable; hardware control works either way.

On **X11** the overlay additionally needs a compositing manager to be running,
because X does not blend a window's alpha channel itself: it draws the colours
at full strength and lets the compositor do the blending. On a bare window
manager with no `picom` or equivalent, Duja reports overlay dimming as
unavailable, and hardware control is unaffected. The gamma channel there comes
from XRandR's per-CRTC transfer table, which every modern X server offers.

The two gamma transports are never crossed over. An XRandR ramp on a Wayland
session would land on Xwayland's virtual CRTCs, which are not on the path to any
monitor: it would be accepted and change nothing on screen, so Duja refuses it
there rather than reporting a dim that did not happen. `duja --restore` resets
the gamma on every CRTC of an X screen and will clear a ramp any program left
behind. It has nothing to do on Wayland, and that is not a gap: a
`wlr-gamma-control` ramp lives only as long as the program that set it, so the
compositor restores the output by itself the moment that program exits, even
after a kill.

**Global hotkeys do not exist on Linux.** The `global-hotkey` backend is
X11-only, and Duja ships Wayland dimming, so a registrar that worked on one
transport and silently did nothing on the other would be worse than none. Duja
registers nothing and says so: the three hotkey settings parse and validate, the
rows grey out with the reason, and no combination is claimed. The route to a
real implementation is the XDG desktop portal's `GlobalShortcuts` interface.

## Installing from the tarball

There is no installer. The two binaries go anywhere on `PATH`; the desktop entry
and icon are what make Duja appear in an application menu:

```sh
tar xzf duja-<version>-linux-x64.tar.gz
cd duja-<version>-linux-x64
install -Dm755 duja dujactl -t ~/.local/bin/
install -Dm644 duja.desktop -t ~/.local/share/applications/
install -Dm644 duja.png ~/.local/share/icons/hicolor/512x512/apps/duja.png
```

`duja.desktop` says `Exec=duja` and `Icon=duja`, a command and an icon *name*,
resolved from `PATH` and the icon theme respectively, which is what makes the
entry work from any prefix. Launch-at-login is a separate file that Duja writes
itself when the setting is enabled (`~/.config/autostart/`), so it is not shipped
here.

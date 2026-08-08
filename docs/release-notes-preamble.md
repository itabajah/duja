> ### Windows is the released platform. macOS and Linux ship as previews.
>
> **Windows**: `duja-setup-<version>.exe` and `duja-<version>-windows-x64.zip`
> are the released build. Developed and QA'd on real hardware.
>
> **macOS** (`.dmg`) and **Linux** (`.tar.gz`) are **unverified previews**. They
> are built, staged, checksummed, minisigned and provenance-attested by the same
> pipeline as the Windows artifacts, and **no one has ever run either one on the
> hardware it targets**. Every backend on those two platforms is verified by
> types, pure tests, CI and cross-referenced primary sources only. Expect
> defects that no amount of that catches.
>
> **On Linux, run `dujactl doctor` first.** It prints the display server it
> found, whether overlay dimming and gamma dimming are available this session,
> and the displays it can see, so a missing `i2c-dev` module or an unusable
> compositor says so instead of looking like a hang. Two things it does *not*
> tell you: whether your desktop has a `StatusNotifierItem` host (without one
> the tray icon simply never appears, and nothing reports that), and anything
> about macOS, where the report is display information only.
>
> **If a screen is left dim or discoloured**, how you recover depends on the
> transport, and it is worth reading before you need it:
>
> - **X11**: `duja --restore` resets the gamma on every CRTC of your X screen.
> - **Wayland**: `duja --restore` has nothing to do and says so. It is not
>   broken - a `wlr-gamma-control` ramp lives only as long as the process that
>   set it, so **killing Duja is the recovery**, and the compositor puts the
>   output back by itself even after a hard kill.
> - **macOS**: `duja --restore` is the only route, and there is no automatic
>   crash recovery behind it - the crash marker the other platforms write is
>   not written there. Both binaries live inside
>   `Duja.app/Contents/MacOS/`, so neither is on your `PATH` without a symlink.
>
> Reports are the point of shipping these. A
> [quirk report](https://github.com/itabajah/duja/issues/new?template=monitor-quirk-report.yml)
> from a real Mac or a real Linux desktop is what moves those platforms off
> preview. See [SECURITY.md](https://github.com/itabajah/duja/blob/main/SECURITY.md)
> for how to verify a download first.

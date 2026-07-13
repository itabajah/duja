# Duja

**The lightweight monitor brightness controller.** Hardware-first physical dimming
(DDC/CI + native panel APIs), a seamless software floor down to true black, native
tray UI on Windows, macOS, and Linux — and no Electron anywhere.

> Status: **Windows feature-complete; `v0.1.0-alpha` pending visual QA.** Hardware
> control, software dimming, tray + flyout, settings, global hotkeys, input
> switching, and the `dujactl` CLI all work on Windows (phases P0–P5); the macOS
> port has landed its backend crates (P6 wave 1). Linux and packaging are next.
> See [docs/STATUS.md](docs/STATUS.md) for the live picture.

## Why Duja

- **Ultra-lightweight.** Rust + [Slint](https://slint.dev), no webview, no
  runtime. Budgets: ≤ 35 MB idle RSS, zero idle CPU wakeups, ≤ 16 MB binary
  (aspiration 12; [docs/perf-budgets.md](docs/perf-budgets.md)).
- **Hardware control first.** External monitors via DDC/CI (brightness,
  contrast, input source); laptop panels via each OS's native backlight API.
- **Seamless software floor.** Displays without hardware control (TVs, docks,
  virtual screens) — and the range *below* hardware 0% — are dimmed by a
  click-through overlay, one continuous slider from 100% to black.
- **Multi-monitor native.** Sync groups, per-monitor settings keyed to stable
  display identity, hot-plug that never loses your levels.

## Planned support matrix

| Capability | Windows | macOS | Linux |
|---|---|---|---|
| External DDC/CI | P3 | P6 (experimental¹) | P7 (X11/Wayland²) |
| Internal panel | P3 | P6 | P7 |
| Overlay dimming | P4 | P6 | P7 (not GNOME Wayland³) |
| Tray + flyout | P4 | P6 | P7 |
| Hotkeys, input switch, `dujactl` | P5 | P6 | P7 |

¹ Apple Silicon DDC uses private APIs (same approach as MonitorControl/Lunar).
² Requires the `i2c-dev` module and an udev rule (documented; `dujactl doctor` checks).
³ GNOME Wayland exposes no third-party overlay/gamma path; hardware control still works.

## Building

```sh
cargo build --workspace          # toolchain pinned in rust-toolchain.toml
cargo test --workspace
```

Hardware-touching tests are double-gated and never run in CI:
`DUJA_HW_TESTS=1 cargo test -p duja-ddc -- --ignored` (restores your
brightness afterwards).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Monitor misbehaving? File a
[quirk report](https://github.com/itabajah/duja/issues/new?template=monitor-quirk-report.yml)
— reports seed the shared quirks database that makes Duja work on imperfect
hardware.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. UI built with Slint under its Royalty-Free license.

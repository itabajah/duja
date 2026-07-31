# macOS packaging

There is no checked-in `Info.plist` or bundle template here on purpose. Unlike
the Windows installer, which is a declarative Inno Setup script
([`packaging/windows/duja.iss`](../windows/duja.iss)), the macOS artifact is
**generated**:

```sh
cargo build --release --target aarch64-apple-darwin -p duja-app -p dujactl
cargo build --release --target x86_64-apple-darwin  -p duja-app -p dujactl
cargo run --release -p xtask -- dist --version 0.2.0
```

That produces, under `target/dist/`:

- `duja-<ver>-macos-universal/Duja.app` — the bundle, with universal `duja` and
  `dujactl` binaries, the licences and README in `Contents/Resources`, and an
  ad-hoc code signature.
- `duja-<ver>-macos-universal.dmg` — a compressed read-only disk image holding
  that bundle beside an `/Applications` symlink, so mounting it gives the usual
  drag-to-install window.

The `Info.plist` is composed in [`xtask/src/bundle.rs`](../../xtask/src/bundle.rs)
rather than stored as a file so its contract is **unit-tested**: that
`LSUIElement` is set (a menu-bar agent, no Dock tile), that
`CFBundleIdentifier` is byte-identical to the `launchd` job label
`duja-platform` registers for launch-at-login, that `LSMinimumSystemVersion`
matches the deployment target the release workflow compiles both slices against,
and that `CFBundleExecutable` names a file the assembly actually writes. Those
tests run on every CI lane, Windows and Linux included.

## Signing

The default is an ad-hoc signature (`codesign -s -`): enough for macOS to
execute the binary — Apple Silicon refuses an unsigned one outright — but not
enough for Gatekeeper to open a downloaded copy without the right-click-Open
detour. Duja has no Apple Developer account, exactly as it has no Windows
Authenticode certificate.

`cargo xtask dist --sign "<identity>"` signs with a real Developer ID instead;
[`release.yml`](../../.github/workflows/release.yml) passes it automatically once
the `MACOS_SIGN` repo variable is set, and notarizes and staples the image in the
same run. That block is inert today and documents its own one-time setup.

## Not shipped yet

No `CFBundleIconFile`, so Finder and the Login Items list show the generic
application icon. Duja's icon art is drawn in code (`duja-ui`'s `icon.rs`) and
no raster asset exists in the tree. See `docs/debt.md`.

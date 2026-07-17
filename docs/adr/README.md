# Architecture Decision Records

MADR-lite format. Accepted ADRs are changed by superseding, not editing.

| # | Title | Status |
|---|---|---|
| [0001](0001-ui-toolkit-slint.md) | UI toolkit: Slint + tray-icon (no webview) | accepted, spike-verified |
| [0002](0002-own-windows-ddc-backend.md) | DDC backend: own dxva2 implementation on Windows | accepted |
| [0003](0003-overlay-first-dimming.md) | Software dimming: overlay primary, gamma opt-in | accepted, spike-verified |
| [0004](0004-stable-edid-identity.md) | Display identity: stable EDID-derived IDs | accepted |
| [0005](0005-threads-not-tokio.md) | Concurrency: std threads + channels, no async runtime | accepted |
| [0006](0006-ipc-transport-and-protocol.md) | IPC transport & protocol | accepted |
| [0007](0007-config-schema-and-migrations.md) | Config schema, migrations, and persistence | accepted |
| [0008](0008-licensing.md) | Licensing (MIT OR Apache-2.0; Slint royalty-free) | accepted |
| [0009](0009-software-renderer-default.md) | Slint renderer: software renderer default | accepted |
| 0010 | Linux tray: tray-icon vs ksni | pending (P7) |
| 0011 | GNOME Wayland dimming strategy | pending (P7 spike) |
| [0012](0012-binary-size-budget-variance.md) | Binary-size budget raised 12 → 16 MB | accepted |
| [0013](0013-macos-ddc-wrap-vs-vendor.md) | macOS DDC/CI: own thin backend (don't wrap ddc-macos) | accepted |
| [0014](0014-perceptual-continuum-v2.md) | Perceptual brightness continuum (v2) | accepted |
| [0015](0015-update-check-on-by-default.md) | Update check on by default (smart-notify) | accepted |
| [0016](0016-windows-distribution-and-signing.md) | Windows distribution & signing (Inno + minisign + provenance) | accepted |
| [0017](0017-engine-shutdown-lifecycle-contract.md) | Engine shutdown & worker-lifecycle contract (generation + retired + bounded shutdown) | accepted |
| [0018](0018-app-owns-continuum-engine-owns-pacing.md) | App owns the continuum; the engine owns pacing (single write authority) | accepted |
| [0019](0019-version-ladder-and-release-trains.md) | Version ladder & release trains (v0.1.x Windows, v0.2 macOS, v0.3 Linux, v1.0) | accepted |
| [0020](0020-release-integrity-and-signing-readiness.md) | Release integrity & signing readiness (no injection, gated publish, hermetic, Azure-ready) | accepted |

Spike evidence lives on branches `spike/eventloop`, `spike/ddc`,
`spike/overlay` (code is not merged; findings are).

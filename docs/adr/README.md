# Architecture Decision Records

MADR-lite format. Accepted ADRs are changed by superseding, not editing.

| # | Title | Status |
|---|---|---|
| [0001](0001-ui-toolkit-slint.md) | UI toolkit: Slint + tray-icon (no webview) | accepted, spike-verified |
| [0002](0002-own-windows-ddc-backend.md) | DDC backend: own dxva2 implementation on Windows | accepted |
| [0003](0003-overlay-first-dimming.md) | Software dimming: overlay primary, gamma opt-in | accepted, spike-verified |
| [0004](0004-stable-edid-identity.md) | Display identity: stable EDID-derived IDs | accepted |
| [0005](0005-threads-not-tokio.md) | Concurrency: std threads + channels, no async runtime | accepted |
| 0006 | IPC design | pending (P5) |
| [0007](0007-config-schema-and-migrations.md) | Config schema, migrations, and persistence | accepted |
| 0008 | Licensing (MIT OR Apache-2.0; Slint royalty-free) | pending (P4, when Slint lands) |
| [0009](0009-software-renderer-default.md) | Slint renderer: software renderer default | accepted |
| 0010 | Linux tray: tray-icon vs ksni | pending (P7) |
| 0011 | GNOME Wayland dimming strategy | pending (P7 spike) |

Spike evidence lives on branches `spike/eventloop`, `spike/ddc`,
`spike/overlay` (code is not merged; findings are).

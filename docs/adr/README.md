# Architecture Decision Records

MADR-lite format. Accepted ADRs are changed by superseding, not editing.

| # | Title | Status |
|---|---|---|
| [0001](0001-ui-toolkit-slint.md) | UI toolkit: Slint + tray-icon (no webview) | accepted |
| 0002 | DDC crate: use / fork / wrap `ddc-hi` | pending (P1 spike) |
| [0003](0003-overlay-first-dimming.md) | Software dimming: overlay primary, gamma opt-in | accepted |
| [0004](0004-stable-edid-identity.md) | Display identity: stable EDID-derived IDs | accepted |
| [0005](0005-threads-not-tokio.md) | Concurrency: std threads + channels, no async runtime | accepted |
| 0006 | IPC design | pending (P5) |
| 0007 | Config schema & migrations | pending (P2) |
| 0008 | Licensing (MIT OR Apache-2.0; Slint royalty-free) | pending (P4, when Slint lands) |
| 0009 | Slint renderer: FemtoVG vs Skia | pending (P1 spike) |
| 0010 | Linux tray: tray-icon vs ksni | pending (P7) |
| 0011 | GNOME Wayland dimming strategy | pending (P7 spike) |

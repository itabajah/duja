# 0004 — Display identity: stable EDID-derived IDs

- Status: accepted
- Date: 2026-07-07

## Context

OS display handles rot: Windows documents that on `WM_DISPLAYCHANGE` any
`HMONITOR` may become invalid; indices reshuffle on hot-plug; macOS display
IDs change across reconnects. Duja must restore per-monitor state (levels,
names, sync groups, quirks) across replug, sleep, docking, and reboots.

## Decision

All state is keyed on a `StableDisplayId` derived from EDID: PNP manufacturer
ID + product code + serial string (e.g. `GSM-5B09-312NTAB1C234`); a content
hash fallback when the serial is absent; a connector-slot suffix disambiguates
identical twins. OS handles are ephemeral lookup values, never keys.

## Consequences

- Hot-plug pipeline: debounce (~750 ms) → re-enumerate → **diff by stable ID**
  → drop/reacquire handles → restore last levels.
- EDID parsing is a fuzz target (P2); fixtures collected from real hardware.
- Identical monitors without serials get slot-suffixed IDs — settings follow
  the port, not the panel; documented behavior.

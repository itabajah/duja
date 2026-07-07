# Refactor debt

Running list, drained by a dedicated `refactor:` PR at each phase checkpoint
(time-boxed to ~15% of the phase). Add entries during feature work instead of
detouring; delete entries when drained.

| Added | Where | What | Why deferred |
|---|---|---|---|
| P0 | `.github/workflows/ci.yml` | Add ubuntu system-deps step (fontconfig, xkbcommon…) when Slint lands | No GUI deps exist yet |
| P1 | quirks/quirks.toml | Encode MSI MP273QP quirk rows (pacing, flaky caps, bogus 0x60 max, write-verify) once the quirks schema/parser exists | Parser lands in P2 wave 2; raw data preserved in ADR-0002 + spike/ddc |
| P1 | P4 QA | Manually verify tray menu-click delivery (not scriptable; identical dispatch path as verified hotkey) | Needs human hand on shell tray |

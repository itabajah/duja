# Refactor debt

Running list, drained by a dedicated `refactor:` PR at each phase checkpoint
(time-boxed to ~15% of the phase). Add entries during feature work instead of
detouring; delete entries when drained.

| Added | Where | What | Why deferred |
|---|---|---|---|
| P0 | `.github/workflows/ci.yml` | Add ubuntu system-deps step (fontconfig, xkbcommon…) when Slint lands | No GUI deps exist yet |
| P0 | workspace | Install cargo-nextest/cargo-deny/git-cliff locally once Smart App Control decision is made | SAC blocks locally-built tools |

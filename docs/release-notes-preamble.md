<!--
  Prepended verbatim to every published release's notes by the "Generate release
  notes" step in .github/workflows/release.yml, ahead of the git-cliff changelog.

  ADR-0024 requires it: a release that carries an artifact for a platform nobody
  has confirmed on real hardware must say so in its own notes, and that claim
  lives in a reviewed file rather than in whoever is cutting the release. It
  cannot live in cliff.toml, because the release step runs `git cliff --strip
  all`, which drops the configured header and footer.

  EDIT THIS IN THE SAME PR THAT BUMPS THE VERSION. When a platform leaves
  preview - macOS at ADR-0013's three independent community confirmations per
  architecture, Linux at a human running the tray on a real session - move it out
  of the preview list here and out of the README's support-matrix note in the
  same change. A stale preamble understates or overstates what a download is,
  and both directions are the false-assurance shape docs/plan.md forbids.
-->

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
> Two things make a bad first run recoverable, and they are worth knowing before
> you start. Run **`dujactl doctor`** first: it reports what your session can
> actually do (transport, overlay, gamma, and the displays it found), so a
> missing kernel module or tray host says so instead of looking like a hang. And
> if a screen is left dim or discoloured, **`duja --restore`** puts the gamma
> back, as does simply killing the process on Wayland.
>
> Reports are the point of shipping these. A
> [quirk report](https://github.com/itabajah/duja/issues/new?template=monitor-quirk-report.yml)
> from a real Mac or a real Linux desktop is what moves those platforms off
> preview. See [SECURITY.md](https://github.com/itabajah/duja/blob/main/SECURITY.md)
> for how to verify a download first.

# 0016 — Windows distribution & signing (Inno Setup + minisign + provenance)

- Status: accepted
- Date: 2026-07-16 (v0.1.0)

## Context

v0.1.0 is the first artifact users install. The repo had **no** release
automation (only `ci.yml`), and `SECURITY.md` had promised, for the first tag,
"SHA256SUMS with a build-provenance attestation and a minisign signature." Two
questions had to be settled: **how users install**, and **how they trust** an
unsigned binary from an individual maintainer.

## Decision

**Packaging.** Ship two Windows x64 artifacts from a tag-triggered
`.github/workflows/release.yml`:

- an **Inno Setup** installer (`packaging/windows/duja.iss`) — per-user
  (`PrivilegesRequired=lowest`, no UAC), Start-Menu shortcut carrying the toast
  AUMID, an optional "launch at login" task that writes the **same** HKCU `Run`
  value (`Duja` = quoted exe path) as Duja's in-app autostart, and an uninstaller;
- a **portable zip** staged by the dependency-free `xtask dist` (std +
  PowerShell `Compress-Archive` only — no archiving crate, no cargo-deny surface).

Inno Setup (chosen over WiX/MSI and cargo-wix) gives a per-user, no-admin install
with the least ceremony; it is installed on the runner via `choco install
innosetup`. The installer and zip consume the same `target/release` binaries, so
the build happens once.

**Trust, without a code-signing certificate.** No Authenticode cert is bought for
v0.1.0 (cost/logistics for a solo maintainer; revisit in `docs/debt.md`). Instead
every artifact carries:

- a **`SHA256SUMS`** file,
- a **minisign** signature (`.minisig`) from a passwordless key generated offline
  (`minisign -G -W`); the secret is a GitHub Actions secret, the public key is
  committed and published in `SECURITY.md`/README, and
- a GitHub **build-provenance attestation** (`actions/attest-build-provenance`)
  binding each artifact to the workflow run.

A `workflow_dispatch` runs the whole pipeline as an **artifacts-only dry run**
(no publish), and a **tag == workspace-version guard** fails the run rather than
shipping a mislabeled installer. Release notes are rendered by git-cliff. All
third-party actions are SHA-pinned per the repo's supply-chain policy; shell tools
installed via choco are out of that policy, exactly like `apt-get`/`choco` in
`ci.yml`.

## Consequences

- Users get a one-click installer *or* a portable zip, both verifiable offline.
- SmartScreen will warn on first run (unsigned); documented in README/SECURITY
  with the verification path as the mitigation. Buying a certificate (or Azure
  Trusted Signing) is a tracked follow-up — it removes the first-run friction that
  costs installs.
- The minisign private key is the sole confidentiality boundary for signatures;
  it is kept offline and rotated (new `.pub` published) if leaked.
- macOS/Linux packaging (DMG/notarization; AppImage/deb) are **not** covered here
  — they land with P6 wave 2 / P7.

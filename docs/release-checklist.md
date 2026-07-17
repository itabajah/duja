# Release checklist

The runbook for cutting a Duja release. The pipeline lives in
[`.github/workflows/release.yml`](../.github/workflows/release.yml); the packaging
and trust rationale is
[ADR-0016](adr/0016-windows-distribution-and-signing.md).

A tag push (`v*`) builds, gates, signs, and publishes. `workflow_dispatch` runs
the identical build as an **artifacts-only dry run** (no publish). Every run first
re-validates the tagged commit (cargo-deny + clippy + tests), so a red or
advisory-drifted commit fails before anything is built.

## Before tagging

- [ ] **Cut from a green `main` only.** The commit you tag must be the exact
      commit that passed CI on `main`. The release gate re-runs
      cargo-deny/clippy/tests, but the full 3-OS matrix only runs on the PR —
      never tag a branch tip or an un-merged commit.
- [ ] **Bump the version and changelog.** Update the workspace `version` in
      `Cargo.toml` (refresh `Cargo.lock`), and move the `CHANGELOG.md` unreleased
      entries under a new `vX.Y.Z` heading. Merge that through CI first.
- [ ] **Dry run.** Trigger the `release` workflow via **Run workflow**
      (`workflow_dispatch`) on the merged commit. Download the
      `duja-<ver>-windows-x64` artifact and confirm the installer, portable zip,
      and `SHA256SUMS` are present and sane. This publishes nothing.

## Tagging

The tag **must** equal the `Cargo.toml` version with a `v` prefix — the pipeline's
guard fails the run when `vX.Y.Z` does not match the workspace version, so a
mislabeled installer never ships.

```sh
git tag v0.1.0            # == the version in Cargo.toml
git push origin v0.1.0
```

The tag push runs the gate, builds the binaries, Authenticode-signs the installer
(only if enabled — see below), computes `SHA256SUMS`, minisigns every asset,
attests the two binaries, renders release notes with git-cliff, and creates the
GitHub Release.

## After publish — verify every asset

Download all assets from the release into one directory, then:

```sh
# 1. Checksums. SHA256SUMS is written LF-only with no BOM, so -c passes on Linux
#    and macOS (a CRLF file would fail with a trailing \r on each filename).
sha256sum -c SHA256SUMS

# 2. minisign. The checksums file is the root of trust; verifying it chains to the
#    binaries via their hashes. The public key is published in SECURITY.md.
minisign -Vm SHA256SUMS -P RWSeL0en/zyHopbYOTmC4nwO4pLW0WN6awWsuhwoUZnSM+D0zukOl0UK

# 3. Build-provenance attestation on each binary (installer + portable zip).
gh attestation verify duja-setup-0.1.0.exe        --repo itabajah/duja
gh attestation verify duja-0.1.0-windows-x64.zip  --repo itabajah/duja
```

All three must pass. Provenance covers the two binaries only; `SHA256SUMS` and the
`.minisig` files are covered by minisign, not attestation.

## Enabling Authenticode (Azure Trusted Signing) later

Duja ships unsigned today, so Windows SmartScreen warns on first run. The pipeline
already contains an **inert, secret-gated** Azure Trusted Signing step (search for
`azure/trusted-signing-action` in `release.yml`). Turning it on needs **no edit**
to the workflow:

1. Create an Azure Trusted Signing account and certificate profile.
2. Add repo **variables**: `AZURE_SIGN=true`, `AZURE_SIGN_ENDPOINT`,
   `AZURE_SIGN_ACCOUNT`, `AZURE_SIGN_CERT_PROFILE`.
3. Add repo **secrets**: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`,
   `AZURE_CLIENT_SECRET` — or wire OIDC via the workflow's existing
   `id-token: write` permission and drop the client secret (preferred).

The step runs **before** `SHA256SUMS` is computed, so the checksums and the
provenance attestation automatically cover the signed installer — no reordering
needed. Only the installer `.exe` is Authenticode-signable; the portable `.zip`
and `SHA256SUMS` stay covered by minisign + provenance. Once signing is confirmed
on a real release, drop the SmartScreen note from `SECURITY.md` / README.

> **Note.** The Azure step is also `PUBLISH`-gated, so a `workflow_dispatch` dry
> run never exercises it — the signing path first runs on a real `v*` tag. When
> you enable it, verify the first tagged release's installer is Authenticode-signed
> (right-click → Properties → Digital Signatures), since the dry run cannot.

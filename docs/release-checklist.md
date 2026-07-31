# Release checklist

The runbook for cutting a Duja release. The pipeline lives in
[`.github/workflows/release.yml`](../.github/workflows/release.yml); the packaging
and trust rationale is
[ADR-0016](adr/0016-windows-distribution-and-signing.md).

A tag push (`v*`) builds, gates, signs, and publishes. `workflow_dispatch` runs
the identical build as an **artifacts-only dry run** (no publish). Every run first
re-validates the tagged commit (cargo-deny + clippy + tests), so a red or
advisory-drifted commit fails before anything is built.

Two jobs: `macos` builds both Apple slices, fuses them, and produces the signed
`Duja.app` inside a disk image; `release` then does everything else on Windows
and folds that image into the one `SHA256SUMS`, the one minisign pass, the one
provenance attestation, and the one Release. The macOS job runs first and the
Windows one `needs:` it, so **a macOS packaging failure blocks the whole
release** rather than quietly publishing a Windows-only one.

## Before tagging

- [ ] **Cut from a green `main` only.** The commit you tag must be the exact
      commit that passed CI on `main`. The release gate re-runs
      cargo-deny/clippy/tests, but the full 3-OS matrix only runs on the PR —
      never tag a branch tip or an un-merged commit.
- [ ] **Bump the version and changelog.** Update the workspace `version` in
      `Cargo.toml` (refresh `Cargo.lock`), and move the `CHANGELOG.md` unreleased
      entries under a new `vX.Y.Z` heading. Merge that through CI first.
- [ ] **Sync `docs/STATUS.md`.** Refresh the "last updated" stamp, flip the
      previous release's At-a-glance row from "shipping" to "shipped", add a row
      and a written section for the new one, and update the test count. This
      drifted three releases behind once (caught at v0.1.5); it is cheap to do
      here and invisible until someone reads a stale claim.
- [ ] **Refresh the platform-facing docs when a platform is new.** The README's
      Install section, the support matrix, and `SECURITY.md` describe what a user
      can actually download. Do not add a platform's instructions before the tag
      that first publishes its artifact — until then they point at a file that is
      not on the Releases page. (Outstanding: the macOS `.dmg` install steps, due
      with `v0.2.0`.)
- [ ] **Dry run.** Trigger the `release` workflow via **Run workflow**
      (`workflow_dispatch`) on the merged commit. Download the
      `duja-<ver>-release` artifact and confirm the installer, portable zip,
      disk image, and `SHA256SUMS` are present and sane. This publishes nothing.
      The dry run is also the **only** automated exercise of the macOS packaging
      path — `lipo`, `codesign` and `hdiutil` cannot run on the other lanes — so
      check the `macos` job's *Verify the bundle* step went green: it lints the
      `Info.plist`, asserts `LSUIElement`, and proves both binaries carry an
      arm64 **and** an x86_64 slice built against the advertised floor.

## Tagging

The tag **must** equal the `Cargo.toml` version with a `v` prefix — the pipeline's
guard fails the run when `vX.Y.Z` does not match the workspace version, so a
mislabeled installer never ships.

```sh
git tag v0.1.0            # == the version in Cargo.toml
git push origin v0.1.0
```

The tag push packages macOS, runs the gate, builds the Windows binaries,
Authenticode-signs the installer (only if enabled — see below), computes
`SHA256SUMS`, minisigns every asset, attests the three binaries, renders release
notes with git-cliff, and creates the GitHub Release.

## After publish — verify every asset

Download all assets from the release into one directory, then:

```sh
# 1. Checksums. SHA256SUMS is written LF-only with no BOM, so -c passes on Linux
#    and macOS (a CRLF file would fail with a trailing \r on each filename).
sha256sum -c SHA256SUMS

# 2. minisign. The checksums file is the root of trust; verifying it chains to the
#    binaries via their hashes. The public key is published in SECURITY.md.
minisign -Vm SHA256SUMS -P RWSeL0en/zyHopbYOTmC4nwO4pLW0WN6awWsuhwoUZnSM+D0zukOl0UK

# 3. Build-provenance attestation on each binary (installer, portable zip, image).
gh attestation verify duja-setup-0.1.0.exe            --repo itabajah/duja
gh attestation verify duja-0.1.0-windows-x64.zip      --repo itabajah/duja
gh attestation verify duja-0.1.0-macos-universal.dmg  --repo itabajah/duja
```

All three must pass. Provenance covers the three binaries only; `SHA256SUMS` and
the `.minisig` files are covered by minisign, not attestation.

On a Mac, also confirm the image mounts and the bundle is intact — this is the
part CI checked on a virtualized runner and a user checks on real hardware:

```sh
hdiutil attach duja-0.1.0-macos-universal.dmg
codesign --verify --strict --verbose=2 /Volumes/Duja\ 0.1.0/Duja.app
lipo -archs /Volumes/Duja\ 0.1.0/Duja.app/Contents/MacOS/duja   # x86_64 arm64
hdiutil detach /Volumes/Duja\ 0.1.0
```

Until a Developer ID signature is enabled (below), the bundle carries an **ad-hoc**
signature: `codesign --verify` passes, `spctl --assess` does not, and Gatekeeper
blocks the first open of a downloaded copy. The user allows it in **System
Settings → Privacy & Security → Open Anyway** — macOS 15 Sequoia removed the
Control-click → Open shortcut, so any instruction that still says "right-click →
Open" is wrong on every release Duja targets. That is the macOS twin of the
SmartScreen prompt on the unsigned Windows installer, and both are called out in
`SECURITY.md`.

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

## Enabling Developer ID signing + notarization later

The bundle ships ad-hoc-signed today, so Gatekeeper blocks a downloaded copy on
first open. `release.yml` already contains the **inert, variable-gated** steps
(search for `MACOS_SIGN`). Turning them on needs **no edit** to the workflow:

1. Join the Apple Developer Program; create a *Developer ID Application*
   certificate and export it as a `.p12`.
2. Create an App Store Connect API key with the Developer ID role.
3. Add repo **variables**: `MACOS_SIGN=true`, `MACOS_SIGN_IDENTITY` (the
   certificate's full common name).
4. Add repo **secrets**: `MACOS_CERTIFICATE` (base64 of the `.p12`),
   `MACOS_CERTIFICATE_PASSWORD`, `MACOS_NOTARY_KEY` (base64 of the `.p8`),
   `MACOS_NOTARY_KEY_ID`, `MACOS_NOTARY_ISSUER`.

`xtask dist` already takes the identity as an argument and always applies the
hardened runtime (`--options runtime`), which notarization requires, so enabling
this changes no step ordering and no packaging code.

> **Note.** Unlike the Azure step, these are *not* `PUBLISH`-gated — a
> `workflow_dispatch` dry run signs and notarizes too. That is deliberate:
> notarization is a network round-trip to Apple that can fail for reasons a build
> cannot predict, and finding that out on a dry run is the point.
>
> The same unpredictability applies on a tag push, where `needs: macos` means a
> notary outage fails the *whole* release. **That is recoverable and does not
> burn the tag**: re-run the workflow from the Actions tab on the same tag. The
> gate, the builds and the packaging all repeat, and `softprops/action-gh-release`
> updates the existing Release rather than creating a second one. Do not delete
> and re-push the tag.

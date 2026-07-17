# 0020 — Release integrity and signing readiness

- Status: accepted
- Date: 2026-07-17

## Context

[ADR-0016](0016-windows-distribution-and-signing.md) established the distribution
mechanism (Inno Setup installer + portable zip, `SHA256SUMS`, minisign, and a
build-provenance attestation). The post-v0.1.0 deep review then audited the
release *pipeline itself* as a trust boundary and found gaps: the tag name flowed
untrusted into privileged `run:` blocks, the tagged commit was published without
re-running the quality gate, the checksums file was unusable on Unix, the signing
key could survive a failed run, and the build was non-hermetic. These are
supply-chain risks independent of the code being released. This ADR records the
integrity properties the pipeline must hold, and the shape that keeps a future
code-signing certificate a drop-in change.

## Decision

The tag-triggered release workflow
([`.github/workflows/release.yml`](../../.github/workflows/release.yml)) must
uphold:

1. **No untrusted context in `run:` bodies.** `github.*` values (tag name, event,
   ref) are passed via a step `env:` map and read as `$env:VAR`, never
   string-interpolated into a shell body — a crafted tag cannot inject code into
   the job that holds `contents`/`id-token`/`attestations:write` and the minisign
   secret. This is audited as part of every release.yml change.

2. **Publish only a gated, green commit.** The release job re-runs the full gate
   (`cargo deny check` + clippy + tests + doctests) on the tagged commit before
   building or publishing, catching advisory/yank drift since the merge and a tag
   placed on a never-CI'd commit. The tag must equal the `Cargo.toml` version
   (guard) or the run fails fast.

3. **Hermetic, verifiable artifacts.** The signed build restores no mutable cache;
   third-party tools are version-pinned with a required checksum (Inno Setup via
   choco `--require-checksums`; minisign downloaded and hash-checked). `SHA256SUMS`
   is written **LF-only, no BOM**, so `sha256sum -c` passes on Linux and macOS.
   The signing key is written to an absolute temp path and wiped in a `finally`
   regardless of the working directory or a mid-signing failure.

4. **Honest, scoped attestation.** The build-provenance attestation covers the two
   binaries (installer + portable zip); `SHA256SUMS` and the `.minisig` files are
   covered by minisign (the checksums file is itself minisigned as the root of
   trust). `SECURITY.md` states this scope exactly, without overstatement.

5. **Signing readiness.** An Authenticode step (Azure Trusted Signing) is staged
   in the pipeline **inert** — SHA-pinned, secret/variable-gated, and placed
   before checksum/attestation so that enabling it (setting a few repo
   variables/secrets, no workflow edit) automatically covers the signed bytes.
   Until then, authenticity is via checksums + minisign + provenance, and
   SmartScreen friction is documented.

Every real tag is validated by a `workflow_dispatch` **dry run** first (the
[release checklist](../release-checklist.md)); publish-only steps
(minisign/attest/gh-release) and the choco pin are cleared there before tagging.

## Consequences

- The release pipeline is treated as production code with a real threat model, not
  a convenience script; changes to it get the same review scrutiny as the app.
- Enabling code signing later is a configuration change, not a re-architecture.
- The dry-run-before-tag discipline means the first *real* run of the publish-only
  path still carries some risk (it cannot be exercised without publishing); the
  checklist calls this out so the maintainer verifies the first signed release's
  assets by hand.

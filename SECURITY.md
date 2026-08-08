# Security Policy

## Reporting a vulnerability

Please report vulnerabilities **privately** via
[GitHub Security Advisories](https://github.com/itabajah/duja/security/advisories/new).
Do not open public issues for security problems. You'll get an acknowledgement
within 7 days.

## Threat model (summary)

Duja runs unprivileged and ships no telemetry. The **only** network code is the
update check: while enabled (on by default; opt-out with
`general.update_check = false`) it makes one HTTPS GET to the GitHub releases API
**at most once a day**, piggybacked on a real user interaction so an idle machine
never wakes for it. On a newer release it surfaces a tray item and a toast whose
click opens the releases page; it **never downloads, installs, or executes
anything**. The response body is read-capped at 64 KiB before buffering, over
rustls with a 5-second timeout.

Local attack surface and mitigations:

- **IPC endpoint** (`dujactl` ↔ app): user-only ACLs (named-pipe DACL /
  0600 unix socket in a 0700 dir whose ownership is verified rather than
  assumed), peer-identity verification, anti-squatting flags, length-prefixed
  frames with a 64 KiB cap enforced before allocation, strict parameter
  validation, connection and read-timeout limits.
- **Config files**: `config.toml` and `state.toml` are typed-parsed only, and
  capped at 1 MiB each, enforced by a bounded read rather than after the file is
  in memory. No user-supplied regex anywhere in the process. Parse failures fall
  back to embedded defaults: never abort, never execute content.
- **The quirk database is compiled in**, not read: `include_str!` at build time,
  and every runtime call site uses the embedded copy. Its 1 MiB cap therefore
  guards a parser rather than a file, and there is no user-supplied quirk file
  for an attacker to reach. (The plan has long named a user override file; it
  does not exist, and `docs/debt.md` carries that as D-012.)
- **Screen-state restitution**: gamma/overlay state is guarded so a crash
  cannot leave the screen unusable (`duja --restore`, crash-marker recovery).

## Supply chain

Pinned lockfile; `cargo-deny` (advisories + license allowlist) on every PR **and
again on the tagged commit at release time**; GitHub Actions pinned by commit SHA.
Each tagged release
([`.github/workflows/release.yml`](.github/workflows/release.yml)) ships the
Windows installer `.exe`, a portable `.zip`, a macOS universal `.dmg` and a Linux
`.tar.gz`, each carrying a GitHub **build-provenance attestation**.
Alongside them a **SHA256SUMS** file lists their hashes, and a **minisign**
signature (`.minisig`) covers each binary *and* `SHA256SUMS` itself. The
minisigned `SHA256SUMS` is the root of trust: verify it, then its hashes chain to
the binaries. The provenance attestation covers **the binaries only**;
`SHA256SUMS` and the `.minisig` files are verified through minisign, not
attestation.

The step-by-step release procedure (dry run, per-asset verification, and how to
turn on Authenticode / Azure Trusted Signing later) is in
[`docs/release-checklist.md`](docs/release-checklist.md).

> **Note on code signing.** Release binaries carry no OS-recognised publisher
> identity yet, on either platform. On Windows there is no Authenticode
> certificate, so SmartScreen may warn on first run. On macOS the `.app` inside
> the disk image is signed **ad-hoc** (`codesign -s -`) rather than with a
> Developer ID: enough for macOS to execute it (Apple Silicon refuses an
> unsigned binary outright), but not notarized, so Gatekeeper blocks the first
> open. Allow it in **System Settings → Privacy & Security → Open Anyway**;
> macOS 15 Sequoia removed the older Control-click → Open shortcut, so the
> instruction you will find in most guides no longer works. Verify
> authenticity with the checksums and minisign signature below instead; both
> gaps are one paid developer account away and the pipeline already has the
> inert steps wired (see the release checklist).

### Verifying a release

```sh
sha256sum -c SHA256SUMS          # Linux
shasum -a 256 -c SHA256SUMS      # macOS (sha256sum is GNU coreutils, not preinstalled)
minisign -Vm SHA256SUMS -P <DUJA_MINISIGN_PUBLIC_KEY>
```

Duja's minisign public key (published here; the private key is kept offline):

```
untrusted comment: minisign public key A2873CFFA7472F9E
RWSeL0en/zyHopbYOTmC4nwO4pLW0WN6awWsuhwoUZnSM+D0zukOl0UK
```

So the verify command is:

```sh
minisign -Vm SHA256SUMS -P RWSeL0en/zyHopbYOTmC4nwO4pLW0WN6awWsuhwoUZnSM+D0zukOl0UK
```

You can also verify the build-provenance attestation on any of the four
artifacts (the installer `.exe`, the portable `.zip`, the macOS `.dmg`, or the
Linux `.tar.gz`) with `gh attestation verify <file> --repo itabajah/duja`.
(`SHA256SUMS` and the `.minisig` files are not attested; they are covered by
minisign above.)

All four are published as of `v0.1.6`, and **two of them are previews**. The
macOS `.dmg` and the Linux `.tar.gz` carry the same checksums, minisign
signatures and provenance attestation as the Windows artifacts, and **nobody has
run either one on the hardware it targets**. They were held until `v0.1.6` for
exactly that reason; [ADR-0024](docs/adr/0024-preview-artifacts-on-the-patch-train.md)
records why holding them was self-defeating and what shipping them costs. Every
release that carries an unconfirmed platform says so in its own notes, from a
committed file rather than from whoever cut it.

What that means for this page is narrow and worth stating: **the integrity story
is identical across all four**, because it is a property of the pipeline rather
than of the code inside. Verifying a `.dmg` proves it is the artifact this
repository built at that tag. It does not prove the program in it behaves.

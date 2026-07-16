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
click opens the releases page — it **never downloads, installs, or executes
anything**. The response body is read-capped at 64 KiB before buffering, over
rustls with a 5-second timeout.

Local attack surface and mitigations:

- **IPC endpoint** (`dujactl` ↔ app): user-only ACLs (named-pipe DACL /
  0600 unix socket in a 0700 dir), peer-identity verification, anti-squatting
  flags, length-prefixed frames with a 64 KiB cap enforced before allocation,
  strict parameter validation, connection and read-timeout limits.
- **Config & quirks files**: typed parsing only, size caps, no user-supplied
  regex, parse failures fall back to embedded defaults — never abort, never
  execute content.
- **Screen-state restitution**: gamma/overlay state is guarded so a crash
  cannot leave the screen unusable (`duja --restore`, crash-marker recovery).

## Supply chain

Pinned lockfile; `cargo-deny` (advisories + license allowlist) on every PR;
GitHub Actions pinned by commit SHA. Each tagged release
([`.github/workflows/release.yml`](.github/workflows/release.yml)) publishes, for
every artifact, a **SHA256SUMS** file, a **minisign** signature (`.minisig`), and
a GitHub **build-provenance attestation**.

> **Note — code signing.** Release binaries are **not** yet signed with an
> Authenticode certificate, so Windows SmartScreen may warn on first run. Verify
> authenticity with the checksums and minisign signature below instead.

### Verifying a release

```sh
sha256sum -c SHA256SUMS
minisign -Vm SHA256SUMS -P <DUJA_MINISIGN_PUBLIC_KEY>
```

Duja's minisign public key (published here; the private key is kept offline):

```
# TODO(release): paste the line from `minisign.pub` here before tagging v0.1.0.
# Generate once with:  minisign -G -W -p minisign.pub -s minisign.key
# then add the secret MINISIGN_SECRET_KEY (= minisign.key contents) to the repo.
RWQ_REPLACE_WITH_REAL_PUBLIC_KEY
```

You can also inspect the build-provenance attestation on any release asset with
`gh attestation verify <file> --repo itabajah/duja`.

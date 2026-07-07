# Security Policy

## Reporting a vulnerability

Please report vulnerabilities **privately** via
[GitHub Security Advisories](https://github.com/itabajah/duja/security/advisories/new).
Do not open public issues for security problems. You'll get an acknowledgement
within 7 days.

## Threat model (summary)

Duja runs unprivileged, ships no telemetry, and makes **no network requests by
default** (the only network code is an opt-in, off-by-default version check
that opens the releases page — it never downloads or executes anything).

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
GitHub Actions pinned by commit SHA; releases ship SHA256SUMS with build
provenance attestation and a minisign signature.

# 0008 — Licensing: MIT OR Apache-2.0, with Slint under its Royalty-Free license

- Status: accepted
- Date: 2026-07-10 (recorded retroactively 2026-07-13; decided when Slint landed in P4, PR #13)
- Evidence: `LICENSE-MIT`, `LICENSE-APACHE`, the `license = "MIT OR Apache-2.0"`
  workspace field, the `deny.toml` license allowlist + Slint exception, and the
  README license note. Plan §8.

## Context

Duja is an open-source project that wants the permissive, ecosystem-standard
licensing Rust crates expect, while depending on **Slint**, which is tri-licensed
(`GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR
LicenseRef-Slint-Software-3.0`). Taking Slint's GPL option would force the whole
application to GPL, which is incompatible with the permissive intent. This ADR
settles the repo's own license, how Slint is consumed, and how the boundary is
enforced so a GPL (or any un-allowlisted) dependency cannot slip in unnoticed.

## Decision

- **The repo is dual-licensed `MIT OR Apache-2.0`** (the Rust-ecosystem norm;
  `LICENSE-MIT` + `LICENSE-APACHE`, `license.workspace = true` on every crate).
- **Slint is consumed under `LicenseRef-Slint-Royalty-free-2.0`**, *never* the GPL
  option. Attribution ("Made with Slint") is shown in the About surface as the
  royalty-free terms require.
- **`cargo-deny` enforces the boundary on every PR**: an allowlist of permissive
  licenses (MIT, Apache-2.0 [+ LLVM exception], BSD-2/3-Clause, BSL-1.0, ISC,
  Zlib, MPL-2.0, Unicode-3.0), with the Slint royalty-free license granted **only**
  to the Slint family of crates *by name* — so a GPL, or any un-allowlisted
  license, appearing anywhere else still fails the gate. Two narrowly-scoped data
  exceptions cover `webpki-roots` (`CDLA-Permissive-2.0`, the Mozilla CA bundle
  pulled by the opt-in update check) and the fuzz-only `libfuzzer-sys` (`NCSA`).
- **Third-party attribution** is bundled via `cargo-about` into a
  `THIRD-PARTY-LICENSES` document for installers (packaging phase).

## Consequences

- Downstream users may take Duja under either MIT or Apache-2.0, at their option.
- Adding a dependency whose license is outside the allowlist fails CI until it is
  removed or (rarely, with justification) added as a scoped exception — the
  supply-chain posture recorded in `SECURITY.md`.
- The Slint exception is deliberately **per-crate**, not a blanket
  `LicenseRef-Slint-*` allow, so the royalty-free grant cannot leak to an unrelated
  crate that happens to carry a Slint-style license reference.
- If Slint's licensing ever changes, this ADR is superseded rather than edited (the
  ADR ground rule).

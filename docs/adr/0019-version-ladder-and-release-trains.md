# 0019 — Version ladder and release trains

- Status: accepted
- Date: 2026-07-17

## Context

The original phase roadmap mapped milestones to versions before the first
release existed: it expected `m5` (Windows feature-complete) to ship as
`v0.2.0-beta` and `m6` (macOS) as `v0.3.0-beta`. Reality diverged: the
Windows-complete build shipped as a **stable `v0.1.0`** (2026-07-16), because the
built-in update checker only prompts on a newer *stable* release via GitHub's
`/releases/latest`, so a `-beta`/`-alpha` line would never notify users. That
left the milestone→version mapping in the roadmap stale and ambiguous, and the
post-v0.1.0 fix wave needed a defined home (a patch train).

## Decision

Adopt a SemVer ladder keyed to platform completeness, with patch trains between
milestones:

| Version line | Meaning |
|---|---|
| `v0.1.x` | **Windows stable train.** `v0.1.0` = Windows feature-complete; `v0.1.1+` = correctness/safety/supply-chain patches. |
| `v0.2.0` | **macOS beta** (`m6-macos`): the macOS app assembly ships. Hardware-blind → community-verified. |
| `v0.3.0` | **Linux beta** (`m7-linux`): the Linux port ships. |
| `v1.0.0` | **Hardening milestone** (`m8`): fuzz burn-in, soak, size/perf budgets met, packaging, cross-platform hardware sign-off. |

Rules:

- **Patch releases (`v0.1.x`)** carry fixes and low-risk improvements only — no
  new platform, no schema break. They are cut from a green `main` following
  [docs/release-checklist.md](../release-checklist.md).
- **Minor releases (`v0.2.0`, `v0.3.0`)** add a platform and may be labelled beta
  in their notes, but ship as **stable SemVer** (no `-beta` pre-release suffix)
  so the update checker prompts existing users. "Beta" is communicated in the
  release notes and README, not the version string.
- **Pre-release suffixes** (`-rc.N`) are reserved for genuine release-candidate
  testing where prompting stable users is *not* wanted; the SemVer precedence in
  the update checker already orders them below their stable release.
- The workspace `version` in `Cargo.toml` is the single source of truth; the
  release pipeline's tag guard fails a tag that does not match it.

This supersedes the `m5→v0.2.0-beta` / `m6→v0.3.0-beta` mapping in the original
roadmap.

## Consequences

- The post-v0.1.0 deep-review fixes ship as **`v0.1.1`** (this train), not folded
  into a future minor.
- macOS lands as `v0.2.0`, Linux as `v0.3.0`, and 1.0 is gated on hardening — a
  clear, user-legible progression where each minor is "a new platform is
  supported".
- Because betas ship as stable SemVer, the update checker keeps working across
  the whole ladder without a channel mechanism; a real beta channel (the
  `/releases` list endpoint + a channel setting) remains future work if a
  pre-release line is ever wanted.

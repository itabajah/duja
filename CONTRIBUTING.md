# Contributing to Duja

## Ground rules

- **Trunk-based.** `main` is protected; work on short-lived branches
  (`feat/…`, `fix/…`, `refactor/…`, `chore/…`, `docs/…`) and squash-merge via PR.
  PR titles must be [conventional commits](https://www.conventionalcommits.org)
  since they become the commit on `main`. Scopes are crate names:
  `feat(core): continuum mapper`.
- **TDD.** `duja-core` is written test-first, no exceptions. Backends must pass
  the shared `BrightnessController` contract suite
  (`duja-core/src/testing/contract.rs`) against fakes; hardware variants are
  `#[ignore]`d and double-gated behind `DUJA_HW_TESTS=1`.
- **Lint wall.** `cargo clippy --workspace --all-targets -- -D warnings` must be
  clean. No `unwrap`/`expect`/`panic!` in production code (denied at the
  workspace level; tests are exempt). Every `#[allow]` needs a `// RATIONALE:`
  comment.
- **Unsafe policy.** `unsafe` only in `duja-ddc` / `duja-panel` / `duja-dimmer`
  / `duja-platform`, confined to `ffi`/`sys` modules, every block documented
  with `// SAFETY:`. Core crates `#![forbid(unsafe_code)]`.
- **No new dependencies casually.** Additions go through `deny.toml`
  (license allowlist) and get a sentence of justification in the PR.

## Local workflow

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # or: cargo nextest run --workspace
# Match CI exactly. --document-private-items is not optional: without it
# rustdoc strips private items before resolving intra-doc links, so private
# modules compile but go unchecked.
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
```

## Architecture

Decisions are recorded as ADRs in [docs/adr/](docs/adr/). Read 0001 to 0005
before proposing structural changes. To change what an accepted ADR **decided**,
write a new ADR that supersedes it; never rewrite the old decision. Correcting a
false claim, or adding a pointer to the ADR that later settled something, is an
edit in place, and [docs/adr/README.md](docs/adr/README.md) sets out the
difference. Refactor debt goes to [docs/debt.md](docs/debt.md) and is drained at
each phase checkpoint; a drained row moves to
[docs/debt-archive.md](docs/debt-archive.md) rather than being deleted, because
how a row drained is usually worth more than the row was. Cite a row by its id
(`D-017`) rather than by the file alone.

## Reporting monitors

The most valuable non-code contribution: run `dujactl doctor --report` and file
a [monitor quirk report](https://github.com/itabajah/duja/issues/new?template=monitor-quirk-report.yml)
for any display that misbehaves.

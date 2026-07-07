# Contributing to Duja

## Ground rules

- **Trunk-based.** `main` is protected; work on short-lived branches
  (`feat/…`, `fix/…`, `refactor/…`, `chore/…`, `docs/…`) and squash-merge via PR.
  PR titles must be [conventional commits](https://www.conventionalcommits.org)
  — they become the commit on `main`. Scopes are crate names:
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
cargo doc --workspace --no-deps   # RUSTDOCFLAGS="-D warnings" in CI
```

## Architecture

Decisions are recorded as ADRs in [docs/adr/](docs/adr/). Read 0001–0005 before
proposing structural changes; propose changes as a new ADR, not an edit to an
accepted one. Refactor debt goes to [docs/debt.md](docs/debt.md) and is drained
at each phase checkpoint.

## Reporting monitors

The most valuable non-code contribution: run `dujactl doctor --report` and file
a [monitor quirk report](https://github.com/itabajah/duja/issues/new?template=monitor-quirk-report.yml)
for any display that misbehaves.

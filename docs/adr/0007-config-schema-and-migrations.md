# 0007 — Config schema, migrations, and persistence

- Status: accepted
- Date: 2026-07-07
- Evidence: `crates/duja-core/src/config/` (43 tests incl. crash simulation,
  round-trip property tests, migration snapshots) and
  `crates/duja-core/src/quirks.rs`.

## Context

Duja persists user settings (per-monitor, keyed by stable identity), volatile
state (last levels), and consumes a quirks database. Requirements: forward
compatibility (a downgrade must not destroy a newer config), crash safety
(power loss mid-write must never corrupt), and user-editability (hand edits
and comments survive).

## Decision

- **Single TOML crate: `toml_edit`** (with `serde` feature) — its document
  model round-trips unknown keys and comments; typed access goes through serde
  schema structs; edits write back only touched keys.
- **Schema v1** with `schema_version` gating and a chained migration framework
  (`migrate(doc, from) → doc`, one step per version). Unknown *fields* are
  tolerated (forward compat); unknown *versions* are a typed error — never
  silently overwritten.
- **Atomic persistence**: same-directory tempfile → rename, fsync file (and
  parent dir on Unix). Missing file → defaults; corrupt file → typed error,
  caller decides. This is the only filesystem I/O in `duja-core`
  (`config::persist`); path discovery stays in the app layer.
- **State separation**: volatile last-levels live in `state.toml` with a ≥2 s
  trailing write-debounce helper, so slider drags never churn `config.toml`.
- **`DimMode` serde mirror** in the schema (exhaustive `From` impls to the
  frozen `model::DimMode`) rather than serde-gating the core model — drift is
  caught at compile time and the model stays dependency-free.
- **Notable defaults**: `update_check = false` (network is opt-in, see
  SECURITY.md), `hw_floor_pct = 0` (no artificial floor until the user sets
  one), `dim_mode = "overlay"` (ADR-0003).
- **Quirks database** (`quirks.rs`): same TOML stack, strict
  `schema_version == 1` gate, 1 MiB cap, prefix/trailing-`*` matching only
  (no regex), exact-beats-glob with longest-prefix ranking, per-field merge
  where specific entries override broad ones. Embedded DB ships in the binary
  (`include_str!`), user overrides load from the config dir, parse failures
  fall back to the embedded copy.

## Consequences

- Migrations are append-only; the v0→v1 snapshot test is the template.
- Config carries no secrets; ids are display identities only.
- The `dujactl doctor` report can cite accumulated quirk `notes` to explain
  why a monitor is being driven conservatively.

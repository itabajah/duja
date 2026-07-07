# Phase-gate review rubric

Run on the phase's cumulative diff before the milestone tag. Every "no" gets
an issue or a docs/debt.md entry before the gate closes.

## Correctness
- [ ] Every fallible path returns a typed error; no `unwrap`/`expect`/`panic!`
      outside tests (lint wall enforces; check for `#[allow]` without `// RATIONALE:`).
- [ ] Error paths are *tested*, including failure injection via fakes
      (latency, `Disconnected`, `Unsupported`, mid-operation removal).
- [ ] Hot-plug/sleep/session transitions considered for any new state.

## Safety
- [ ] All new `unsafe` confined to `ffi`/`sys` modules with `// SAFETY:`
      invariants that match the platform docs.
- [ ] No new unsafe in forbid-crates (core, ui, ipc, dujactl).

## Tests
- [ ] New logic arrived test-first (check commit order in the PR).
- [ ] Coverage thresholds hold (core ≥ 90%, ipc/view-models ≥ 85%).
- [ ] Contract suite green vs fakes; hardware suite run manually if backend code changed.

## Performance
- [ ] No new polling loops, timers, or busy-waits; idle-wakeup audit clean.
- [ ] No allocation in per-frame or per-event hot paths without justification.
- [ ] Budgets re-measured if the change plausibly affects them.

## API & docs
- [ ] Public items documented; rustdoc builds with `-D warnings`.
- [ ] ADR written/updated for any structural decision.
- [ ] CHANGELOG regenerated; debt.md drained by a `refactor:` PR (~15% of phase time-box).

## Security (P5 and P8: full SECURITY.md checklist item-by-item)
- [ ] New IPC/CLI inputs validated (range, charset, size caps before allocation).
- [ ] No new network calls; no telemetry; deny/audit clean.

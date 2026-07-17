# 0017 — Engine shutdown & worker-lifecycle contract

- Status: accepted
- Date: 2026-07-17

## Context

The engine actor owns one std thread per monitor, each exclusively holding a
[`BrightnessController`](../../crates/duja-core/src/controller.rs). Every control
op is blocking FFI that can stall for seconds or wedge forever (ADR-0005). The
watchdog already detaches (never joins) a worker whose write goes unacked past
`watchdog_timeout`. A deep review of the lifecycle around that detach found four
gaps that all stem from one missing invariant — *worker identity* — plus an
unbounded shutdown:

- **Unbounded shutdown (H3).** `shutdown_workers` sent `Shutdown` then `join()`ed
  **every** worker still in the map. A worker blocked in `controller.set()` but
  younger than the watchdog is still mapped, so `join()` blocked app exit until
  the driver call returned — or forever. This propagated through `Engine::stop`/
  `Drop` to the UI thread's quit path.
- **Detached zombie becomes a double-writer (E-A).** A detached worker was
  dropped without joining, assuming a stuck call stays stuck. If the call merely
  blocked *longer* than the watchdog then returned, the zombie parked on `recv`,
  drained the buffered backlog crossbeam still delivers, coalesced it, and issued
  another `set()` to the same panel — concurrently with its freshly-spawned
  replacement. Two DDC writers on one monitor.
- **OpenFailed lost control for the session (E-C).** A genuine `OpenFailed`
  retired the handle only when `join.is_finished()` — a race-prone thread-exit
  proxy — and never marked the display unresponsive. The manager kept
  `responsive = true`, the UI never greyed it, and a later plain enumeration
  emitted no event, so no worker was ever respawned: the slider moved while the
  hardware never changed.
- **Abandoned display falsely un-greyed (E-D).** After the max stuck cycles a
  display is abandoned (no respawn), yet the `Responsive` arm still notified
  `DisplayResponsive`, un-greying a dead display with no worker.
- **Recovery relearned instead of restoring (E-E).** The pure watchdog-recovery
  path issued an initial `Get`, whose ack overwrote the retained user level with
  whatever the hardware read — discarding the user's intent if the wedged write
  never applied, unlike the `Reattached` arm which restores it.

## Decision

Introduce a **worker-identity backbone** and a **bounded shutdown**, and make
*worker presence* — not `manager.responsive` alone — the source of truth.

1. **Generation stamp.** The engine assigns each spawned worker a monotonic
   `generation` (global, on `WorkerHandle`). This is WORKER identity and is kept
   distinct from `seq` (OP identity, unchanged). A worker's `OpenFailed` ack
   carries its generation; the engine acts on it **only** when it matches the
   currently-registered worker's generation, so a stale failure from an
   already-replaced worker is ignored and a legitimate one is never missed to
   thread-exit timing. This replaces the `join.is_finished()` proxy.

2. **`retired` flag.** Each worker holds an `Arc<AtomicBool>` the engine flips
   (Release) on every detach (`retire_worker`, and on shutdown); the worker loads
   it (Acquire) after every controller call and before performing any buffered
   write, and exits immediately when set. **A detached worker never performs
   another hardware write** — so it can never race its replacement as a second
   writer. The Release/Acquire pair publishes the replacement's existence to the
   zombie's exit check.

3. **Bounded shutdown.** Shutdown flags every worker `retired`, sends `Shutdown`,
   then waits for exits against a **single shared deadline** (`SHUTDOWN_JOIN_BUDGET`,
   2 s) observed via a per-worker `done` channel that disconnects when the thread
   exits. A worker that exits is reaped; one still wedged when the budget is spent
   is **detached** (its thread leaks and dies when its call returns and it sees
   `retired`). App exit never blocks unboundedly on a driver call.

4. **OpenFailed marks unresponsive.** A generation-matched `OpenFailed` marks the
   display unresponsive (greying it and arming the next sighting's `Responsive`
   respawn). A *prompt* open-failure — the opener returns `None` quickly — does
   **not** count toward the stuck/abandon budget: unlike a hung driver it leaks no
   thread and is cheap to retry. An opener that itself *blocks* past
   `watchdog_timeout` before failing is instead caught by the watchdog on the
   initial-`Get` armed after spawn, and does count as a stuck cycle — which is
   correct, since an open that stalls for seconds is indistinguishable from a hung
   driver. Decoupling the watchdog from a not-yet-opened worker (so even a
   slow-to-fail open stays off the abandon budget) is a possible future
   refinement; the current behaviour is safe and bounded.

5. **A live worker is what the UI greys on.** The `Responsive` arm emits
   `DisplayResponsive` **only** when the display actually has a live worker —
   either a sighting event in the same pass already (re)spawned one, or it
   respawns one here. An abandoned display gets no worker and stays greyed. On
   the respawn it **restores** the recorded user level (`dispatch_set`, mirroring
   `Reattached`), falling back to an initial `Get` only when no level was ever
   recorded.

## Consequences

- Shutdown is bounded to `SHUTDOWN_JOIN_BUDGET`; a wedged worker leaks one thread
  (already the watchdog's bound: 1/monitor/session) instead of hanging exit.
- The generation gate and `retired` flag are the single mechanism behind E-A and
  E-C; `seq`-gating of Set/Get acks, latest-wins coalescing, the zero-idle-wakeup
  select loop, and the `Reattached`+`Responsive` same-pass dedupe are all
  preserved unchanged.
- `manager.responsive` may briefly read `true` for an abandoned display (a
  sighting flips it in `duja-core`, which this layer does not touch); this has no
  external effect because dispatch and polling are both keyed on worker presence,
  and the UI greys on the notifications, which now track a live worker.
- Watchdog recovery preserves the user's level at the manager layer, matching the
  `Reattached` path; the app layer already masked the user-visible symptom, so
  this is a correctness/consistency fix.

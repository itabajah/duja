# 0006 — IPC transport & protocol (local control channel for dujactl ↔ app)

- Status: accepted
- Date: 2026-07-10 (recorded retroactively 2026-07-13; decided and shipped in P5, PRs #18 and #22)
- Evidence: the shipped transport in `crates/duja-platform/src/ipc/` (Windows
  named pipe, unix socket), the protocol types in `crates/duja-ipc/`, the P5
  security-checklist pass and adversarial-review findings (`docs/STATUS.md` →
  "P5 gate results"), and `SECURITY.md`. Plan §6.

## Context

`dujactl` and a second `duja.exe` launch need a local channel to the running tray
app: forward `ShowFlyout`, drive brightness/input, report status. The plan (§3,
§6) constrained it up front — a local-only surface that must be unprivileged,
spoof-resistant, and allocation-bounded, with no network and no telemetry. The
open questions this ADR settles: framing, the per-OS transport, identity and
authorization, and where the code lives across crates.

## Decision

- **Protocol** — length-prefixed JSON with a versioned envelope, in the pure,
  `#![forbid(unsafe_code)]` `duja-ipc` crate (protocol + framing types only, no
  transport). A 64 KiB max frame is enforced **before** allocation; every field is
  strictly validated (percent 0–100; display-id charset `[A-Za-z0-9#\-]{1,64}`; a
  VCP `0x60` input value only if it is on that display's *probed* allow-list).
  JSON (not a bespoke binary) keeps the wire debuggable and `dujactl` scriptable;
  the size cap and strict types keep it safe.
- **Transport, per OS**, in `duja-platform` (the only crate with the OS FFI),
  mutually cfg-gated (`win_pipe` / `unix_socket` / `noop`):
  - **Windows** — a per-user named pipe `\\.\pipe\duja-<user-SID>` with an
    explicit user-only DACL *and* an explicit owner (`O:<sid>` — an elevated
    process's default object owner is the Administrators group, which would fail
    the owner assertion), `FILE_FLAG_FIRST_PIPE_INSTANCE` (anti-squat),
    `PIPE_REJECT_REMOTE_CLIENTS`, and a client PID + session check. Built on
    **overlapped I/O** with `CancelIoEx` cancellation.
  - **macOS/Linux** — a unix-domain socket at `0600` in a `0700` dir, peer euid
    verified via `getpeereid` / `SO_PEERCRED`, stale-socket takeover, reads timed
    with `poll(2)` (macOS rejects `SO_RCVTIMEO` on `AF_UNIX` with `EINVAL`).
- **Limits** — ≤ 4 concurrent connections, ≤ 2 handler threads, and a single
  **exchange-wide** 5 s read deadline (armed once per exchange, *not* per `read()`
  syscall).
- **Client behaviour** — `dujactl` speaks IPC-first and silently falls back to the
  direct hardware backend when no app is running; a second `duja.exe` forwards
  `ShowFlyout` and exits 0 (single-instance).

## Consequences

- **Two adversarial-review findings, both fixed test-first** (P5 gate): the read
  deadline was minted per-`read()`, so a same-user peer dribbling one byte every
  few seconds renewed it forever and pinned a handler thread (two such clients
  denied the whole server) — now armed once per exchange; and the first pipe
  instance leaked on a thread-spawn failure. A pre-gate integration run also caught
  a **deadlock**: the transport timed reads with `PeekNamedPipe`, which is not
  guaranteed non-blocking, so a silent read-only client froze a handler inside the
  syscall — this triggered the overlapped-I/O rewrite, which in turn surfaced a
  stop-path bug (returning `ErrorKind::Interrupted`, which `read_exact` silently
  retries into an infinite spin — now `ConnectionAborted`).
- **Local-only threat surface**, documented in `SECURITY.md`: the socket/pipe plus
  the config and quirks files. No network path exists by default.
- The `unix_socket` transport serves both macOS (P6) and Linux (P7) unchanged.

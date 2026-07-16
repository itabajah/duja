# 0015 — Update check on by default (smart-notify)

- Status: accepted
- Date: 2026-07-16 (v0.1.0)
- Supersedes the P5 posture recorded in ADR-0008 / STATUS ("opt-in, off by default").

## Context

P5 shipped the update check as **opt-in, off by default, manual only**: it ran
only when the user enabled `general.update_check` *and* clicked "Check now" (or
ran `duja --check-updates`), it lived only in the settings window, and it did one
HTTPS GET that opens the releases page — never downloading. That posture was
chosen to keep "no network by default" literally true.

For an early product shipping its **first public release**, the update loop is the
primary way users stay current and hear about fixes. A checker buried in settings
and off by default reaches almost no one. The requirement for v0.1.0 was an update
system that "works perfectly" as a retention mechanism — without breaking the two
load-bearing guarantees: **zero idle CPU wakeups** and **never download/execute**.

## Decision

Promote the checker to **smart-notify**, on by default:

1. **Default on**, opt-out via `general.update_check = false`. This is the one
   deliberate change to the "no network by default" stance; it is documented in
   `SECURITY.md`.
2. **Once-a-day background check, no timer.** The check piggybacks on events the
   process already handles — tray/menu/hotkey interactions and startup — gated by
   a pure `due_for_check(now, last, 24h)` using the already-persisted
   `last_update_check_unix`. There is **no** background thread waking on a timer,
   so the zero-idle-wakeup guarantee is preserved by construction. An in-flight
   guard and the daily gate prevent hammering the API or spamming toasts.
3. **Surfaced, not buried.** A newer release prepends an **"Update available"**
   item to the tray menu, sets a tray tooltip, and raises a **WinRT toast** (built
   on the `windows` crate already present — no new dependency; the process and the
   installer's Start-Menu shortcut share the AUMID `io.github.itabajah.duja` so
   the toast resolves an identity). All best-effort: a toast failure is logged and
   ignored.
4. **Still notify-only.** Clicking any surface opens the releases page. Duja does
   **not** download, verify, or install binaries. Full self-update was rejected
   for v0.1.0: the binaries are unsigned, and auto-replacing a running unsigned
   process is both risky and low-trust (see ADR-0016).
5. **SemVer-correct comparison.** The version compare now implements SemVer §11
   precedence (pre-release ordering, build metadata ignored) so an alpha/beta line
   orders correctly in future. GitHub's `/releases/latest` endpoint excludes
   pre-releases, so stable users are never prompted onto a beta today.

## Consequences

- First-run users get update notifications with no configuration.
- The zero-idle-wakeup and no-download guarantees are intact; `SECURITY.md`'s
  threat model is updated to describe the once-a-day, interaction-driven check.
- `duja --check-updates` still forces a check regardless of the toggle (running it
  is itself the request).
- A true **beta channel** (the `/releases` list endpoint + a channel setting) and
  **auto-download self-update** (which needs signing, see ADR-0016) are explicit
  follow-ups tracked in `docs/debt.md`, not part of v0.1.0.

# 0022 — Linux D-Bus: zbus, and the async executor that is already in the build

- Status: accepted
- Date: 2026-08-01

## Context

Three separate P7 features want D-Bus, and the roadmap treated them as three
decisions when they are one:

- **Internal panel brightness** — logind's
  `org.freedesktop.login1.Session.SetBrightness`, which works unprivileged for the
  active session where a raw `/sys/class/backlight` write needs a udev rule or
  root.
- **Suspend/resume events** — logind's `PrepareForSleep` signal, which the
  platform event pump needs for `Suspending`/`Resumed` (`duja-platform`'s module
  docs already promise these on Linux).
- **The tray** — StatusNotifierItem is a D-Bus protocol (ADR-0010).

The objection to D-Bus has always been dependency weight, and specifically that
`zbus` implies an async runtime, which ADR-0005 turned down. **That objection is
already obsolete, and not by anything Duja chose.**

Verified with `cargo tree --target x86_64-unknown-linux-gnu -i zbus -e normal`:

```
zbus v5.17.0
└── i-slint-backend-winit v1.17.1
    ├── duja-ui  (direct dependency)
    └── i-slint-backend-selector → slint
```

and `-i async-io -e normal`:

```
async-io v2.6.0
├── async-process v2.5.0 → zbus v5.17.0 → i-slint-backend-winit …
└── zbus v5.17.0 (*)
```

So on Linux, **`zbus` 5.17 and the `async-io` executor are already normal
(non-dev) dependencies of the shipping binary**, pulled by the Slint winit backend
that ADR-0001 and ADR-0009 selected. `async-executor`, `async-lock` and
`futures-lite` are in `Cargo.lock` too. `tokio` is not (`grep -c` → 0).

## Decision

**Use `zbus` for all three, with `default-features = false` and its `async-io`
executor**, matching what Slint already selected so the graph gains no second
runtime. Duja's own code stays synchronous and thread-based per ADR-0005, calling
through `zbus`'s blocking API.

**Both logind users degrade rather than fail.** D-Bus absence is a normal
condition, not an error: containers, minimal window managers, `ssh` sessions, and
systems that are not running systemd at all.

- Panel brightness falls back to writing `/sys/class/backlight/<dev>/brightness`,
  and if that is not writable the panel is reported read-only rather than absent —
  the same "graceful absence" rule `duja-panel`'s docs already state for a desktop
  with no panel.
- Sleep events simply do not arrive. The pump keeps working for display hot-plug,
  which comes from the kernel uevent netlink socket and needs no D-Bus at all.

**Do not** add a D-Bus dependency for display hot-plug. `NETLINK_KOBJECT_UEVENT`
is readable with the `libc` already in `duja-platform`, needs no libudev and no
session bus, and works in every one of the degraded environments above.

## Consequences

- **The marginal cost of D-Bus in P7 is `ksni` and `task-local`.** Everything else
  is already compiled into the Linux build. That is a much weaker reason to avoid
  it than the roadmap assumed, and it is why this ADR exists as its own decision
  rather than as three arguments repeated.
- **ADR-0005's title is imprecise on Linux and should not be read as falsified.**
  Its *decision* is about Duja's concurrency model — `std::thread` +
  `crossbeam-channel`, no `spawn_blocking` wrapper around blocking FFI — and that
  is unchanged and unchallenged. What is no longer true on Linux is the ambient
  claim that no async executor exists in the process: one does, inside Slint's
  D-Bus client, and it did before P7 started. Recorded here rather than by editing
  an accepted ADR, per the ADR README's supersede-don't-edit rule.
- **A new failure mode to keep out of the log: a missing session bus is normal.**
  Every one of these call sites must treat `DBUS_SESSION_BUS_ADDRESS` being unset,
  or a connect failing, as a `debug!` and a capability report — never a `warn!` on
  every start.
- **The version is pinned by Slint, not by Duja.** Duja must declare the same
  `zbus` major it resolves today, and a Slint upgrade that moves `zbus` becomes a
  two-crate change. Worth a line in the release checklist rather than a surprise
  during a bump.
- **Unverified on hardware.** logind's `SetBrightness` has never been called by
  this project, and a GitHub runner has no session bus and no backlight device.
  CI can test the fallback selection and the pure error mapping; the D-Bus calls
  themselves ship 🧪.

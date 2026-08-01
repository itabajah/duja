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
    ├── duja-ui v0.1.5 (crates/duja-ui)
    │   └── duja-app v0.1.5 (crates/duja-app)
    └── i-slint-backend-selector v1.17.1
        └── slint v1.17.1
            ├── duja-app v0.1.5 (crates/duja-app)
            └── duja-ui v0.1.5 (crates/duja-ui) (*)
```

and `-i async-io -e normal`:

```
async-io v2.6.0
├── async-process v2.5.0
│   └── zbus v5.17.0
│       └── i-slint-backend-winit v1.17.1
│           ├── duja-ui v0.1.5 (crates/duja-ui)
│           │   └── duja-app v0.1.5 (crates/duja-app)
│           └── i-slint-backend-selector v1.17.1
│               └── slint v1.17.1
│                   ├── duja-app v0.1.5 (crates/duja-app)
│                   └── duja-ui v0.1.5 (crates/duja-ui) (*)
├── async-signal v0.2.14
│   └── async-process v2.5.0 (*)
└── zbus v5.17.0 (*)
```

(Absolute paths shortened to repo-relative; otherwise verbatim.)

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
is readable with the `libc` already declared under `duja-platform`'s
`[target.'cfg(unix)'.dependencies]`, needs no libudev and no session bus, and
works in every one of the degraded environments above. The kernel registers that
multicast group `NL_CFG_F_NONROOT_RECV`, so an unprivileged process may listen.

This **reverses a plan of record** rather than merely choosing between options,
and it is called out here because the old plan is still written down:
`duja-platform`'s crate docs say the Linux event source is a *"udev `drm`
monitor"*. That would mean libudev, a C library and a system dependency, to
receive the same netlink messages `libc` can read directly. The module doc is
updated in the wave that implements the pump; until then the two disagree and
this ADR is the newer decision.

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
- **Unverified on hardware, including two claims this ADR rests on.** logind's
  `SetBrightness` has never been called by this project, so *"works unprivileged
  for the active session"* is taken from its documented contract and not from
  observation; likewise that the `drm` uevents carry what hot-plug detection needs.
  Both are third-party claims of the same class ADR-0011 declines to encode, and
  both are cheap to check on the VM/WSL environment before the code that depends
  on them ships. A GitHub runner has no session bus and no backlight device, so CI
  can test the fallback selection and the pure error mapping only; the D-Bus calls
  themselves ship 🧪.

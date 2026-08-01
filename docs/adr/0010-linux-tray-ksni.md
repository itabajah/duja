# 0010 — Linux tray: ksni, not tray-icon

- Status: accepted
- Date: 2026-08-01

## Context

`tray-icon` 0.24 already carries the Windows and macOS tray (ADR-0001), so the
cheap answer is to use its Linux backend too and keep one code path. The reserved
question from the original plan was whether that works. It does not, and the
reasons are checkable rather than aesthetic. Everything below was read out of the
crate sources in the local registry, `Cargo.lock`, and `deny.toml` — not inferred
from reputation.

**`tray-icon`'s Linux backend needs a GTK event loop.** Its own crate docs
(`lib.rs`) say: *"On Windows and Linux, an event loop must be running on the
thread … on Linux, a gtk event loop. It doesn't need to be the main thread but you
have to create the tray icon on the same thread as the event loop."* Duja's main
thread runs Slint's winit loop, which is not GTK. A GTK loop is therefore an
additional dedicated thread whose callbacks then have to be marshalled back — not
impossible, but it is a second event loop in a process that has a stated
single-owner threading model (ADR-0005).

The rest of that backend's Linux behaviour is a poor fit independently:

- `platform_impl/gtk/mod.rs` drives `libappindicator::AppIndicator` and sets the
  icon by **writing a temp PNG to disk** and pointing the indicator at it, with a
  counter and an unlink per change.
- Runtime system libraries the user must install: `gtk3`, `libxdo`, and
  `libappindicator3` or `libayatana-appindicator3`.
- Documented Linux limits: `tooltip` is *"Unsupported"*, and *"once a menu is set,
  it cannot be removed"*.
### The supply-chain argument for ksni does not exist, and the first draft of this ADR got it backwards

The obvious argument is that `deny.toml`'s **8 RUSTSEC advisory ignores**
(`RUSTSEC-2024-0370/0412/0413/0415/0416/0418/0419/0420`, the unmaintained gtk-rs
GTK3 family plus `proc-macro-error`) go away by dropping GTK. That is false, and
it is worth writing down because the mechanism is not obvious.

**Duja does not depend on `tray-icon` on Linux at all.** `duja-app/Cargo.toml`
declares it under `cfg(windows)` and `cfg(target_os = "macos")` only, and
`cargo tree --target x86_64-unknown-linux-gnu -i gtk -e normal` prints *"nothing
to print"*. There is no Linux tray today; P7 **adds** one. Choosing ksni therefore
removes nothing.

The GTK family is in the *cargo-deny* graph for an unrelated reason:
`deny.toml`'s `[graph]` sets `all-features = true` with no `targets` key, so
cargo-deny evaluates every target, and `tray-icon`'s **default** features
(`default = ["libxdo", "gtk"]`, where `gtk = ["muda/gtk", "dep:libappindicator"]`)
declare `libappindicator` for Linux-family targets. `tray-icon` stays for Windows
and macOS after this decision, so those edges — and the 8 ignores — would survive
the swap untouched.

The lever that does work is `default-features = false`, and it is available now,
independent of this ADR. Verified by experiment on this branch and reverted: the
whole GTK family (`gtk`, `gtk-sys`, `gtk3-macros`, `libappindicator`,
`libappindicator-sys`, `proc-macro-error`) **leaves `Cargo.lock` entirely**, all
8 ignores can be deleted with `cargo deny check advisories` still reporting
`advisories ok`, and the Windows build is unaffected (`clippy --workspace
--all-targets --all-features -D warnings` clean, 1049/1049 nextest). It lands as
its own change rather than inside this decision, because it is not one.

`ksni` 0.3.6 implements the freedesktop **StatusNotifierItem** spec directly over
D-Bus, with no GTK and no C library. Its cost in this workspace is unusually low:
its four non-optional dependencies are `zbus`, `futures-util`, `serde` and
`pastey`, and **all four are already in `Cargo.lock`**, because
`i-slint-backend-winit` pulls `zbus` 5.17 (see ADR-0022).

Two facts about `ksni` that a summary would get wrong, so they are stated here:

1. Its `blocking` feature is **not** a standalone mode. `src/compat.rs` opens with
   `compile_error!(r#"Either "tokio" (default) or "async-io" must be enabled."#)`.
   `blocking` adds no dependencies of its own; it adds a `block_on` over whichever
   executor is selected. So an executor must be chosen.
2. Its licence is **`Unlicense`**, which is not in `deny.toml`'s `allow` list.

## Decision

**Use `ksni` for the Linux tray**, with `default-features = false` and features
`["async-io", "blocking"]`.

`async-io` rather than `tokio` because `tokio` is **not** in the graph at all
(`grep -c '^name = "tokio"$' Cargo.lock` → 0) while every crate ksni's `async-io`
feature names — `async-io`, `async-lock`, `async-executor`, `futures-lite`,
`futures-channel` — **is** already there, pulled by `zbus` for Slint. The only
addition is `task-local`, so the Linux graph grows by `ksni` and `task-local`.
Selecting `tokio` would add a whole runtime instead. (`task-local` 0.1 is not in
the local registry cache, so its own transitive deps are unverified; "two crates"
assumes it is a leaf.)

`blocking` so Duja's own code stays synchronous, in keeping with ADR-0005.

Add `"Unlicense"` to `deny.toml`'s `allow` list rather than a scoped exception:
it is a public-domain dedication with no obligations, and scoping it to one crate
would imply a judgement about the crate rather than about the licence.

Introduce a small tray seam in `duja-app` so `AppState` stops naming a concrete
backend. It currently holds `tray_icon::TrayIcon`, `tray_icon::menu::Menu` and
`tray_icon::menu::MenuItem` directly (`bin_support/tray/state.rs`), which is
already recorded in `docs/debt.md` as a structural constraint — there the stated
consequence is that `AppState` cannot be *constructed in a test*, because of the
Win32 tray constructor and two live Slint shells. P7 is when it acquires a second
implementation and therefore has to earn the seam.

## Consequences

- **No supply-chain change, in either direction.** The 8 advisory ignores are
  neither created nor removed by this decision (see above); they are a `deny.toml`
  configuration artefact of `tray-icon`'s *default features* on a
  Windows/macOS-only dependency. What this decision does buy is that Duja never
  *adds* GTK to the Linux graph, which the alternative would have done.
- **No new system-library requirements on Linux.** Nothing to `apt install`,
  which matters for a tarball distribution (wave 6).
- **Two tray implementations to keep behaviourally aligned**, with only one of
  them testable on the maintainer's hardware. The seam is what limits the damage:
  the menu model, the state machine and every action stay shared, and only the
  backend differs. Divergence is the standing risk and it is why the seam is part
  of this decision rather than a later tidy-up.
- **`ksni` is a smaller, less-exercised crate than `tray-icon`** and the ecosystem
  is one maintainer. Accepted knowingly: the alternative's Linux backend is a
  wrapper over an unmaintained binding family, which is not obviously safer.
- **Neither choice fixes GNOME.** GNOME Shell ships no StatusNotifierItem host, so
  a tray icon needs the AppIndicator extension there. That is equally true of
  `libappindicator`, which speaks SNI on modern desktops as well, so it does not
  discriminate between the options — but it must be disclosed in the README rather
  than discovered by users. Recorded again in ADR-0011, which faces the same
  GNOME-shaped hole for dimming.
- **Untested on hardware at the time of writing.** The maintainer's hardware is
  Windows, and a GitHub runner has no StatusNotifierItem host, so CI can compile
  this and exercise the pure parts but cannot show a tray icon appearing. This is
  narrower than "no Linux is available": `docs/STATUS.md` §3 plans P7 as
  *VM-assisted*, and a WSL distribution is being set up, so a Linux **runtime**
  will exist — what will still be missing is a desktop session with an SNI host,
  which is what a tray needs. The Linux tray ships as 🧪 in the support matrix,
  exactly as macOS did, until community confirmation on a real desktop.

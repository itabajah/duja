//! Support modules for the `duja` binary.
//!
//! `main.rs` stays a thin dispatcher; every piece of logic that is worth
//! testing lives here as a small, focused module:
//!
//! - [`cli`] — hand-rolled argument parsing into a [`cli::Command`] (no `clap`).
//! - [`backend`] — real hardware enumeration (`duja-ddc` + `duja-panel`) mapped
//!   to [`duja_core::manager::DiscoveredDisplay`], plus a re-enumerate-and-open
//!   controller factory and per-display bounds discovery.
//! - [`bounds`] — the app-side resolved-id → display-geometry map (twin-slot
//!   aware): bounds plus the two platform tokens. Every value is
//!   platform-specific — bounds are physical pixels on Windows and points on
//!   macOS, and the tokens are one GDI device name repeated on Windows versus two
//!   distinct `CGDirectDisplayID`s on macOS, one addressing the display and one
//!   naming its framebuffer (see [`backend::DisplayGeom`] for the contract).
//! - [`clone_group`] — groups mirrored (Duplicate-mode) panels sharing one GDI
//!   surface into a single control (one merged row, one overlay per surface).
//! - [`counting`] — a [`counting::CountingController`] decorator that tallies
//!   hardware set/get/error calls for the stress harness.
//! - [`dimming`] — the pure continuum → dimmer planner (overlay/gamma + hardware).
//! - [`ipc`] — the binary's IPC wiring: starting the OS transport, the
//!   second-instance handshake, and the tray bridge that routes `set` /
//!   `show-flyout` onto the main thread. The transport-agnostic half (the
//!   [`ipc::IpcBridge`] trait, [`ipc::handle_request`], and the headless bridge)
//!   lives in the library so an integration test can drive the whole seam.
//! - [`gamma`] — wires the opt-in gamma sub-floor channel: a pure engage/restore
//!   coordinator (unit-tested with a fake sink) plus a per-platform sink that
//!   drives the GPU ramp — Windows' guard-backed one, which owns the
//!   persistent-ramp crash marker, and macOS' `Core Graphics` one, which needs
//!   no marker because a macOS ramp is not believed to outlive the process.
//! - [`hotkey`] — pure accelerator-string parsing + conflict detection for the
//!   global-hotkey table (the tray converts + registers the result).
//! - [`level_forward`] — the slider → engine forwarding seam behind a
//!   [`level_forward::LevelSink`], where the final-value-of-a-drag contract
//!   (never throttle on the UI side) is pinned.
//! - [`logging`] — `tracing` setup with a size-rotated file log.
//! - [`num`] — pure percent ↔ raw brightness scaling.
//! - [`paths`] — resolved config/state/marker/log locations (`ProjectDirs`).
//! - [`positioning`] — pure flyout-anchor geometry.
//! - [`rng`] — a dependency-free xorshift PRNG for the stress flood.
//! - [`run`] — the `--once` / `--headless` assembly.
//! - [`settings`] — config → [`ContinuumConfig`](duja_core::continuum::ContinuumConfig)
//!   mapping, the HDR gamma guard, and theme resolution.
//! - [`settings_apply`] — applying a settings command to the config document
//!   (format-preserving) plus the UI ↔ config theme/dim-mode mappings.
//! - [`startup`] — crash-marker recovery on launch.
//! - [`state_store`] — the user-level book with debounced persistence.
//! - [`stress`] — the `--stress` exit-criteria harness and its report.
//! - [`updates`] — the opt-in update check: a pure decision function over an
//!   injected transport, plus the rustls-backed HTTPS transport.
//! - `toast` — a best-effort desktop notification for a newly-available update.
//!   Compiled where the tray is (its only caller), and Windows-only in substance:
//!   a `WinRT` toast there, a documented no-op on macOS, because the tray menu
//!   item is the guaranteed surface.
//! - `tray` — the real tray + flyout assembly on the Slint main thread, on
//!   Windows and macOS. Not intra-doc-linked here: it is still cfg-gated, so a
//!   link would break the cross-platform (Linux) rustdoc build.

pub(crate) mod backend;
pub(crate) mod bounds;
pub(crate) mod cli;
pub(crate) mod clone_group;
pub(crate) mod counting;
pub(crate) mod dimming;
pub(crate) mod fmt;
pub(crate) mod gamma;
pub(crate) mod hotkey;
pub(crate) mod ipc;
pub(crate) mod level_forward;
pub(crate) mod logging;
pub(crate) mod motion;
pub(crate) mod num;
pub(crate) mod paths;
pub(crate) mod positioning;
pub(crate) mod rng;
pub(crate) mod run;
pub(crate) mod settings;
pub(crate) mod settings_apply;
pub(crate) mod startup;
pub(crate) mod state_store;
pub(crate) mod stress;
pub(crate) mod updates;

// `toast` has exactly one caller — `tray::update_flow` — so it is gated to
// wherever the tray is. Declaring it unconditionally makes it dead code on Linux,
// which the ubuntu clippy lane rejects with `-D warnings`; the module being
// *internally* cross-platform (a WinRT toast on Windows, a documented no-op on
// macOS) is a separate axis from where it is compiled at all.
// Both un-gated in P7 wave 5: the tray now has a third backend (`ksni`,
// ADR-0010) and runs on every platform Duja targets.
//
// `toast` follows it rather than keeping a narrower gate of its own, because its
// gate was only ever "wherever the tray is" — its one caller is
// `tray::update_flow` — and the file has had a `cfg(not(windows))` no-op arm
// since macOS. Linux takes that arm. A real `org.freedesktop.Notifications` call
// is a feature with a design (zbus is already in the graph, so it is reachable),
// not plumbing this wave owes; `docs/debt.md` carries it. The update still
// surfaces through the tray menu item and tooltip, which is the guaranteed path
// on all three platforms and the only one on two of them.
pub(crate) mod toast;
pub(crate) mod tray;

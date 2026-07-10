//! Support modules for the `duja` binary.
//!
//! `main.rs` stays a thin dispatcher; every piece of logic that is worth
//! testing lives here as a small, focused module:
//!
//! - [`cli`] — hand-rolled argument parsing into a [`cli::Command`] (no `clap`).
//! - [`backend`] — real hardware enumeration (`duja-ddc` + `duja-panel`) mapped
//!   to [`duja_core::manager::DiscoveredDisplay`], plus a re-enumerate-and-open
//!   controller factory and per-display bounds discovery.
//! - [`bounds`] — the app-side resolved-id → pixel-bounds map (twin-slot aware).
//! - [`counting`] — a [`counting::CountingController`] decorator that tallies
//!   hardware set/get/error calls for the stress harness.
//! - [`dimming`] — the pure continuum → dimmer planner (overlay/gamma + hardware).
//! - [`ipc`] — the app side of the local IPC server: the [`ipc::IpcBridge`]
//!   trait, the [`ipc::handle_request`] request→response mapping, and the
//!   tray/headless bridges that route `set` and `show-flyout` to the right path.
//! - [`gamma`] — wires the opt-in gamma sub-floor channel: a pure engage/restore
//!   coordinator (unit-tested with a fake sink) plus the Windows guard-backed
//!   sink that drives the GPU ramp and owns the persistent-ramp crash marker.
//! - [`hotkey`] — pure accelerator-string parsing + conflict detection for the
//!   global-hotkey table (the Windows tray converts + registers the result).
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
//! - `tray` — (Windows only) the real tray + flyout assembly on the Slint main
//!   thread. Not intra-doc-linked here: it is `cfg(windows)`, so a link would
//!   break the cross-platform (Linux) rustdoc build.

pub(crate) mod backend;
pub(crate) mod bounds;
pub(crate) mod cli;
pub(crate) mod counting;
pub(crate) mod dimming;
pub(crate) mod fmt;
pub(crate) mod gamma;
pub(crate) mod hotkey;
pub(crate) mod ipc;
pub(crate) mod logging;
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

#[cfg(windows)]
pub(crate) mod tray;

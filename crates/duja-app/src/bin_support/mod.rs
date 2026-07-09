//! Support modules for the `duja` binary.
//!
//! `main.rs` stays a thin dispatcher; every piece of logic that is worth
//! testing lives here as a small, focused module:
//!
//! - [`cli`] — hand-rolled argument parsing into a [`cli::Command`] (no `clap`).
//! - [`backend`] — real hardware enumeration (`duja-ddc` + `duja-panel`) mapped
//!   to [`duja_core::manager::DiscoveredDisplay`], plus a re-enumerate-and-open
//!   controller factory.
//! - [`counting`] — a [`counting::CountingController`] decorator that tallies
//!   hardware set/get/error calls for the stress harness.
//! - [`num`] — pure percent ↔ raw brightness scaling.
//! - [`rng`] — a dependency-free xorshift PRNG for the stress flood.
//! - [`run`] — the `--once` / `--headless` assembly.
//! - [`stress`] — the `--stress` exit-criteria harness and its report.

pub(crate) mod backend;
pub(crate) mod cli;
pub(crate) mod counting;
pub(crate) mod fmt;
pub(crate) mod num;
pub(crate) mod rng;
pub(crate) mod run;
pub(crate) mod stress;

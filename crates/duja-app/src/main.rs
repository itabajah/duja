//! Duja headless application entry point.
//!
//! A thin dispatcher over [`bin_support`]: parse the mode, run it, translate
//! the outcome into an exit code. Every mode degrades cleanly to
//! `"no displays"` + exit 0 when no monitors are visible (e.g. a disconnected
//! session), so it is safe to run anywhere.
//!
//! Modes (see `--help`): `--headless`, `--once`, `--stress <secs> [--hz <n>]`,
//! `--restore`. The tray UI, single-instance guard and config wiring arrive in
//! P4; this binary stays a console harness until then.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::process::ExitCode;

mod bin_support;

use bin_support::cli::{self, Command};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("duja: {err:#}");
            ExitCode::from(1)
        }
    }
}

/// Parse and dispatch one invocation.
///
/// # Errors
/// Returns any error bubbled up from assembling or running the chosen mode
/// (e.g. the platform event pump failing to start).
fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    match cli::parse(args) {
        Ok(Command::Help) => {
            println!("{}", cli::USAGE);
            Ok(ExitCode::SUCCESS)
        }
        Ok(Command::Once) => Ok(bin_support::run::once()),
        Ok(Command::Headless) => bin_support::run::headless(),
        Ok(Command::Restore) => Ok(bin_support::run::restore()),
        Ok(Command::Stress { secs, hz }) => bin_support::stress::run(secs, hz),
        Err(err) => {
            eprintln!("{err}");
            Ok(ExitCode::from(2))
        }
    }
}

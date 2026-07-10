//! Duja application entry point.
//!
//! A thin dispatcher over [`bin_support`]: parse the mode, initialise logging,
//! run it, translate the outcome into an exit code.
//!
//! The default (no args) is the **tray application** (tray icon + flyout, engine,
//! dimmer, config/state — see [`bin_support::tray`]). The console modes
//! (`--headless`, `--once`, `--stress`, `--restore`) remain for development and
//! degrade cleanly when no monitors are visible.
//!
//! # Windows subsystem
//!
//! Release builds use the `windows` subsystem so double-clicking the tray app
//! opens no console. A consequence: under a `windows_subsystem = "windows"`
//! release binary the CLI subcommands cannot write to a console (there is none)
//! — acceptable for P4; the dedicated CLI story is `dujactl`. Dev/debug builds
//! keep the console subsystem so every mode prints normally.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
// RATIONALE: unlike the pure `duja-app` library (which keeps `#![forbid(unsafe_code)]`),
// the *binary* wires the Windows tray, which needs a little documented FFI in
// `bin_support::tray` (cursor/work-area geometry). Every `unsafe` block there
// carries a `// SAFETY:` note and the workspace `unsafe_op_in_unsafe_fn` /
// `undocumented_unsafe_blocks` denials still apply.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::process::ExitCode;

mod bin_support;

use bin_support::cli::{self, Command};
use bin_support::logging;
use bin_support::paths::DujaPaths;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!(error = %format!("{err:#}"), "duja exited with an error");
            eprintln!("duja: {err:#}");
            ExitCode::from(1)
        }
    }
}

/// Parse and dispatch one invocation.
///
/// # Errors
/// Returns any error bubbled up from assembling or running the chosen mode
/// (e.g. the platform event pump or the flyout window failing to start).
fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    let command = match cli::parse(args) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("{err}");
            return Ok(ExitCode::from(2));
        }
    };

    init_logging(command);

    match command {
        Command::Help => {
            println!("{}", cli::USAGE);
            Ok(ExitCode::SUCCESS)
        }
        Command::Tray { verbose } => run_tray(verbose),
        Command::Once => Ok(bin_support::run::once()),
        Command::Headless => bin_support::run::headless(),
        Command::Restore => Ok(bin_support::run::restore()),
        Command::Stress { secs, hz } => bin_support::stress::run(secs, hz),
    }
}

/// Install the tracing subscriber for this run.
///
/// The tray app logs WARN to a rotating file (or DEBUG to stderr under
/// `--verbose`); console modes log WARN to stderr so their diagnostics surface
/// alongside their normal stdout output.
fn init_logging(command: Command) {
    match command {
        Command::Tray { verbose: true } => logging::init(None, true),
        Command::Tray { verbose: false } => {
            let log_dir = DujaPaths::resolve().map(|p| p.log_dir);
            logging::init(log_dir.as_deref(), false);
        }
        _ => logging::init(None, false),
    }
}

/// Run the tray application (Windows only).
#[cfg(windows)]
fn run_tray(verbose: bool) -> anyhow::Result<ExitCode> {
    bin_support::tray::run(verbose)
}

/// The tray app is Windows-only in this build; other targets report and exit
/// non-zero rather than silently doing nothing.
#[cfg(not(windows))]
fn run_tray(_verbose: bool) -> anyhow::Result<ExitCode> {
    eprintln!("duja: the tray application is only available on Windows in this build");
    Ok(ExitCode::from(1))
}

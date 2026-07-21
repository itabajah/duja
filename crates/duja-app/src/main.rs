//! Duja application entry point.
//!
//! A thin dispatcher over [`bin_support`]: parse the mode, initialise logging,
//! run it, translate the outcome into an exit code.
//!
//! The default (no args) is the **tray application** (tray icon + flyout, engine,
//! dimmer, config/state — see the Windows-only `bin_support::tray`). The console modes
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
        Command::Tray { verbose } => run_tray(verbose, false),
        Command::Relaunch => run_tray(false, true),
        Command::Once => Ok(bin_support::run::once()),
        Command::Headless => bin_support::run::headless(),
        Command::Restore => Ok(bin_support::run::restore()),
        Command::CheckUpdates => Ok(check_updates()),
        Command::Stress { secs, hz } => bin_support::stress::run(secs, hz),
    }
}

/// Run the update check once and print the outcome (the `--check-updates` mode).
/// Always makes the network request when invoked explicitly, regardless of the
/// `general.update_check` config toggle (which is on by default and gates only
/// the app's automatic background check) — running this subcommand is itself the
/// request.
fn check_updates() -> ExitCode {
    use bin_support::updates::{self, HttpsTransport, UpdateOutcome};

    match updates::check_for_update(&HttpsTransport, env!("CARGO_PKG_VERSION")) {
        UpdateOutcome::UpToDate => {
            println!("Duja {} is up to date.", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        UpdateOutcome::UpdateAvailable { version } => {
            println!("A newer release is available: {version}");
            println!("Releases: {}", updates::RELEASES_PAGE_URL);
            ExitCode::SUCCESS
        }
        UpdateOutcome::Failed(reason) => {
            eprintln!("Update check failed: {reason}");
            ExitCode::from(1)
        }
    }
}

/// Install the tracing subscriber for this run.
///
/// The tray app logs WARN to a rotating file (or DEBUG to stderr under
/// `--verbose`); console modes log WARN to stderr so their diagnostics surface
/// alongside their normal stdout output.
fn init_logging(command: Command) {
    match command {
        Command::Tray { verbose: true } => {
            logging::init(None, true);
            // Console/verbose mode: stderr is live, so no crash file is needed.
            logging::install_panic_hook(None);
        }
        Command::Tray { verbose: false } | Command::Relaunch => {
            // Derive the log paths through the SAME resolve-or-fallback the tray
            // runs from, so a host with no home dir still logs to the temp root
            // (previously logging was silently disabled while the tray itself ran
            // on temp paths). Unchanged when a home dir resolves. A relaunch logs
            // exactly like the plain tray it becomes.
            let paths = DujaPaths::resolve_or_fallback();
            logging::init(Some(&paths.log_dir), false);
            // The tray release build has no console; a panic (e.g. inside a Slint
            // callback, which then aborts) would otherwise vanish. Persist it.
            logging::install_panic_hook(Some(paths.log_dir.join(logging::CRASH_FILE)));
        }
        _ => logging::init(None, false),
    }
}

/// Run the tray application (Windows only). `relaunch` is set when this process
/// was spawned by the tray "Restart" item, so startup waits for the outgoing
/// instance to release the single-instance lock before taking over.
#[cfg(windows)]
fn run_tray(verbose: bool, relaunch: bool) -> anyhow::Result<ExitCode> {
    bin_support::tray::run(verbose, relaunch)
}

/// The tray app is Windows-only in this build; other targets report and exit
/// non-zero rather than silently doing nothing.
// RATIONALE: the Result wrapper mirrors the Windows signature so the caller is
// cfg-free; this stub itself can never fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(not(windows))]
fn run_tray(_verbose: bool, _relaunch: bool) -> anyhow::Result<ExitCode> {
    eprintln!("duja: the tray application is only available on Windows in this build");
    Ok(ExitCode::from(1))
}

//! Hand-rolled argument parsing for the `duja` binary (no `clap`).

use std::fmt;

/// The default flood rate for `--stress`, in ticks per second per display.
pub(crate) const DEFAULT_STRESS_HZ: u32 = 20;

/// A parsed `duja` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    /// The default (no args): run the tray application. `verbose` routes DEBUG
    /// logs to stderr instead of the WARN rotating file log.
    Tray {
        /// Whether `--verbose` was passed.
        verbose: bool,
    },
    /// Run the tray application as a **relaunch** of a quitting instance (the
    /// tray "Restart" item spawns `duja --relaunch`). Identical to [`Command::Tray`] except
    /// startup first waits briefly for the outgoing instance to release the
    /// single-instance lock, so the two do not collide. Internal — not advertised
    /// in `--help`.
    Relaunch,
    /// Assemble the real pipeline and run until the user quits (`q<Enter>`).
    Headless,
    /// Enumerate once, print a table, exit.
    Once,
    /// Run the stress exit-criteria harness for `secs` at `hz` ticks/sec.
    Stress {
        /// Flood duration in seconds.
        secs: u64,
        /// Flood rate in ticks per second per display.
        hz: u32,
    },
    /// Restore the screen: clear overlays + identity gamma, then report.
    Restore,
    /// Run the update check once, print the outcome, exit (headless).
    CheckUpdates,
    /// Print usage.
    Help,
}

/// A usage error from [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliError(pub(crate) String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CliError {}

/// The usage text printed for `--help` and on a usage error.
pub(crate) const USAGE: &str = "\
duja — monitor brightness controller (dev harness)

USAGE:
    duja [MODE]

With no MODE, duja runs as the tray application (tray icon + flyout).

MODES:
    (default)             run the tray application
    --verbose             run the tray app with DEBUG logging to stderr
    --headless            assemble the real pipeline; run until `q<Enter>`
    --once                enumerate once, print a display table, exit
    --stress <secs>       flood SetUserLevel for <secs> seconds, print a report
        [--hz <n>]        flood rate per display (default 20)
    --restore             clear overlays + reset identity gamma, then report
    --check-updates       check GitHub for a newer release, print the result
    --help                print this help

With no monitors visible (e.g. a disconnected session) the console modes
degrade cleanly: they print \"no displays\" and exit 0.";

/// Parse the argument list (excluding `argv[0]`) into a [`Command`].
///
/// # Errors
/// Returns [`CliError`] on an unknown mode, a missing/invalid `<secs>` or
/// `--hz` value, or conflicting modes.
pub(crate) fn parse(args: &[String]) -> Result<Command, CliError> {
    let mut iter = args.iter();
    let Some(mode) = iter.next() else {
        return Ok(Command::Tray { verbose: false });
    };

    match mode.as_str() {
        "--verbose" => expect_end(iter, Command::Tray { verbose: true }),
        "--relaunch" => expect_end(iter, Command::Relaunch),
        "--headless" => expect_end(iter, Command::Headless),
        "--once" => expect_end(iter, Command::Once),
        "--restore" => expect_end(iter, Command::Restore),
        "--check-updates" => expect_end(iter, Command::CheckUpdates),
        "--help" | "-h" => Ok(Command::Help),
        "--stress" => parse_stress(iter),
        other => Err(CliError(format!("unknown mode `{other}`\n\n{USAGE}"))),
    }
}

/// Ensure no trailing arguments follow a mode that takes none.
fn expect_end<'a>(
    mut iter: impl Iterator<Item = &'a String>,
    cmd: Command,
) -> Result<Command, CliError> {
    match iter.next() {
        None => Ok(cmd),
        Some(extra) => Err(CliError(format!(
            "unexpected argument `{extra}`\n\n{USAGE}"
        ))),
    }
}

/// Parse `<secs> [--hz <n>]` after `--stress`.
fn parse_stress<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<Command, CliError> {
    let secs_raw = iter
        .next()
        .ok_or_else(|| CliError(format!("--stress needs <secs>\n\n{USAGE}")))?;
    let secs = secs_raw.parse::<u64>().map_err(|_| {
        CliError(format!(
            "invalid <secs> `{secs_raw}` (want a non-negative integer)"
        ))
    })?;

    let mut hz = DEFAULT_STRESS_HZ;
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--hz" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| CliError("--hz needs <n>".to_owned()))?;
                hz = raw.parse::<u32>().ok().filter(|n| *n >= 1).ok_or_else(|| {
                    CliError(format!("invalid --hz `{raw}` (want an integer >= 1)"))
                })?;
            }
            other => {
                return Err(CliError(format!(
                    "unexpected argument `{other}`\n\n{USAGE}"
                )));
            }
        }
    }
    Ok(Command::Stress { secs, hz })
}

#[cfg(test)]
mod tests {
    use super::{Command, DEFAULT_STRESS_HZ, parse};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_args_is_the_tray_app() {
        assert_eq!(parse(&[]), Ok(Command::Tray { verbose: false }));
    }

    #[test]
    fn verbose_flag_selects_the_tray_app() {
        assert_eq!(
            parse(&args(&["--verbose"])),
            Ok(Command::Tray { verbose: true })
        );
        // `--verbose` takes no trailing argument.
        assert!(parse(&args(&["--verbose", "extra"])).is_err());
    }

    #[test]
    fn relaunch_flag_selects_the_relaunch_tray() {
        assert_eq!(parse(&args(&["--relaunch"])), Ok(Command::Relaunch));
        // `--relaunch` takes no trailing argument.
        assert!(parse(&args(&["--relaunch", "extra"])).is_err());
    }

    #[test]
    fn simple_modes_parse() {
        assert_eq!(parse(&args(&["--headless"])), Ok(Command::Headless));
        assert_eq!(parse(&args(&["--once"])), Ok(Command::Once));
        assert_eq!(parse(&args(&["--restore"])), Ok(Command::Restore));
        assert_eq!(
            parse(&args(&["--check-updates"])),
            Ok(Command::CheckUpdates)
        );
        assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
    }

    #[test]
    fn stress_uses_default_hz() {
        assert_eq!(
            parse(&args(&["--stress", "5"])),
            Ok(Command::Stress {
                secs: 5,
                hz: DEFAULT_STRESS_HZ
            })
        );
    }

    #[test]
    fn stress_reads_explicit_hz() {
        assert_eq!(
            parse(&args(&["--stress", "3", "--hz", "50"])),
            Ok(Command::Stress { secs: 3, hz: 50 })
        );
    }

    #[test]
    fn stress_rejects_zero_hz_and_bad_secs() {
        assert!(parse(&args(&["--stress", "3", "--hz", "0"])).is_err());
        assert!(parse(&args(&["--stress", "abc"])).is_err());
        assert!(parse(&args(&["--stress"])).is_err());
    }

    #[test]
    fn unknown_mode_and_trailing_args_error() {
        assert!(parse(&args(&["--frobnicate"])).is_err());
        assert!(parse(&args(&["--once", "extra"])).is_err());
    }
}

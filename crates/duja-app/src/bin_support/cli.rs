//! Hand-rolled argument parsing for the `duja` binary (no `clap`).

use std::fmt;

/// The default sampling interval for `--soak`, in seconds.
///
/// A minute. Fine enough that an hour's run has sixty points to see a slope in,
/// coarse enough that a 24-hour run prints 1,440 lines rather than 86,400.
pub(crate) const DEFAULT_SOAK_INTERVAL_SECS: u64 = 60;

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
    /// Run the idle soak harness for `secs`, sampling every `interval_secs`.
    Soak {
        /// How long to hold the pipeline idle, in seconds.
        secs: u64,
        /// Seconds between samples.
        interval_secs: u64,
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
    --soak <secs>         hold the pipeline idle for <secs>, sampling RSS and
                          GDI/USER handle counts, then print a growth report
        [--every <n>]     seconds between samples (default 60)
    --restore             clear overlays + reset identity gamma, then report
    --check-updates       check GitHub for a newer release, print the result
    --help                print this help

With no monitors visible (e.g. a disconnected session) the console modes
degrade cleanly: they print \"no displays\" and exit 0. `--soak` is the one
exception, and deliberately: it exits non-zero when a budget is missed or when
it could not measure at all.";

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
        "--soak" => parse_soak(iter),
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

/// Parse `<secs> [--every <n>]` after `--soak`.
fn parse_soak<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<Command, CliError> {
    let secs_raw = iter.next().ok_or_else(|| {
        CliError(format!(
            "--soak needs <secs>

{USAGE}"
        ))
    })?;
    // `>= 1`, unlike `--stress`: a zero-second soak takes exactly one sample,
    // and a run that reports a verdict from one sample has measured nothing.
    // `--every 0` is refused for a related reason two blocks down.
    let secs = secs_raw
        .parse::<u64>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| {
            CliError(format!(
                "invalid <secs> `{secs_raw}` (want an integer >= 1)"
            ))
        })?;

    let mut interval_secs = DEFAULT_SOAK_INTERVAL_SECS;
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--every" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| CliError("--every needs <n>".to_owned()))?;
                interval_secs = raw.parse::<u64>().ok().filter(|n| *n >= 1).ok_or_else(|| {
                    CliError(format!("invalid --every `{raw}` (want an integer >= 1)"))
                })?;
            }
            other => {
                return Err(CliError(format!(
                    "unexpected argument `{other}`

{USAGE}"
                )));
            }
        }
    }
    Ok(Command::Soak {
        secs,
        interval_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::{Command, DEFAULT_SOAK_INTERVAL_SECS, DEFAULT_STRESS_HZ, parse};

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
    fn soak_defaults_its_sampling_interval() {
        assert_eq!(
            parse(&args(&["--soak", "3600"])),
            Ok(Command::Soak {
                secs: 3600,
                interval_secs: DEFAULT_SOAK_INTERVAL_SECS
            })
        );
    }

    #[test]
    fn soak_reads_an_explicit_interval() {
        assert_eq!(
            parse(&args(&["--soak", "60", "--every", "5"])),
            Ok(Command::Soak {
                secs: 60,
                interval_secs: 5
            })
        );
    }

    /// A zero interval would spin the sampling loop as fast as the OS will
    /// answer, which measures the harness rather than Duja - and would fill a
    /// 24-hour log with millions of lines.
    #[test]
    fn soak_refuses_a_zero_interval() {
        assert!(parse(&args(&["--soak", "60", "--every", "0"])).is_err());
    }

    #[test]
    fn soak_needs_a_duration() {
        assert!(parse(&args(&["--soak"])).is_err());
        assert!(parse(&args(&["--soak", "forever"])).is_err());
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

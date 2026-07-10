//! Hand-rolled argument parsing for `dujactl` (no `clap`).
//!
//! Parsing is a pure function over the argument list so it is exhaustively unit
//! testable; `main` only maps the resulting [`Command`] to an action and the
//! error to an exit code.

use std::fmt;

/// Exit code: success.
pub const EXIT_OK: u8 = 0;
/// Exit code: usage or validation error.
pub const EXIT_USAGE: u8 = 2;
/// Exit code: the requested display id is unknown.
pub const EXIT_UNKNOWN_DISPLAY: u8 = 3;
/// Exit code: a backend (hardware/OS) operation failed.
pub const EXIT_BACKEND: u8 = 4;
/// Exit code: reached the running app over IPC, but the exchange itself failed
/// (a transport fault after connecting — distinct from a clean fall-back to the
/// direct backend, which happens silently when no server is up).
pub const EXIT_SERVER: u8 = 5;

/// The usage text for `--help` and usage errors.
pub const USAGE: &str = "\
dujactl — scriptable control of Duja's displays

Talks to the running Duja app over local IPC when it is up, and falls back to
the direct in-process backend (DDC + panel) when it is not.

USAGE:
    dujactl [-v|--verbose] <COMMAND>

COMMANDS:
    list                              list displays: id, kind, name, brightness, features
    get <id>                          print one display's brightness percent
    set <id|all> brightness <0-100>   set brightness percent (mapped onto the probed range)
    input <id>                        list a display's allowed input sources and the current one
    input <id> <name|code>            switch the display's active input (e.g. hdmi1, dp1, 0x11)
    doctor                            environment / backend / quirk diagnostics + server reachability
    version                           print the workspace version
    --help                            print this help

OPTIONS:
    -v, --verbose   report which path (ipc / direct) served each request

EXIT CODES:
    0  ok
    2  usage / validation error
    3  unknown display id
    4  backend (hardware/OS) error
    5  IPC server error (reached the app, but the exchange failed)";

/// A parsed `dujactl` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// List every controllable display.
    List,
    /// Print one display's brightness.
    Get {
        /// The target display's stable id.
        id: String,
    },
    /// Set brightness on one display or all displays.
    Set {
        /// The target selector.
        target: SetTarget,
        /// Validated percent in `0..=100`.
        percent: u8,
    },
    /// List or switch a display's DDC input source (VCP `0x60`).
    Input {
        /// The target display's stable id.
        id: String,
        /// The requested input (name or code); `None` lists the allowed set.
        value: Option<String>,
    },
    /// Print environment / backend / quirk diagnostics.
    Doctor,
    /// Print the workspace version.
    Version,
    /// Print usage.
    Help,
}

/// The target of a `set` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetTarget {
    /// Every display.
    All,
    /// A single display by id.
    One(String),
}

/// A usage/validation error (maps to [`EXIT_USAGE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// Parse the argument list (excluding `argv[0]`) into a [`Command`].
///
/// # Errors
/// Returns [`UsageError`] on an unknown command, missing operands, or an
/// out-of-range / non-numeric brightness value.
pub fn parse(args: &[String]) -> Result<Command, UsageError> {
    let mut iter = args.iter();
    let Some(cmd) = iter.next() else {
        return Ok(Command::Help);
    };

    match cmd.as_str() {
        "list" => end(iter, Command::List),
        "doctor" => end(iter, Command::Doctor),
        "version" => end(iter, Command::Version),
        "--help" | "-h" | "help" => Ok(Command::Help),
        "get" => parse_get(iter),
        "set" => parse_set(iter),
        "input" => parse_input(iter),
        other => Err(usage(&format!("unknown command `{other}`"))),
    }
}

/// Ensure no trailing arguments follow a nullary command.
fn end<'a>(
    mut iter: impl Iterator<Item = &'a String>,
    cmd: Command,
) -> Result<Command, UsageError> {
    match iter.next() {
        None => Ok(cmd),
        Some(extra) => Err(usage(&format!("unexpected argument `{extra}`"))),
    }
}

/// Parse `get <id>`.
fn parse_get<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<Command, UsageError> {
    let id = iter.next().ok_or_else(|| usage("get needs <id>"))?;
    if let Some(extra) = iter.next() {
        return Err(usage(&format!("unexpected argument `{extra}`")));
    }
    Ok(Command::Get { id: id.clone() })
}

/// Parse `input <id> [<name|code>]`.
fn parse_input<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<Command, UsageError> {
    let id = iter
        .next()
        .ok_or_else(|| usage("input needs <id> [<name|code>]"))?;
    let value = iter.next().cloned();
    if let Some(extra) = iter.next() {
        return Err(usage(&format!("unexpected argument `{extra}`")));
    }
    Ok(Command::Input {
        id: id.clone(),
        value,
    })
}

/// Parse `set <id|all> brightness <0-100>`.
fn parse_set<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<Command, UsageError> {
    let target_raw = iter
        .next()
        .ok_or_else(|| usage("set needs <id|all> brightness <0-100>"))?;
    let feature = iter
        .next()
        .ok_or_else(|| usage("set needs a feature (only `brightness` is supported)"))?;
    if feature != "brightness" {
        return Err(usage(&format!(
            "unsupported feature `{feature}` (only `brightness` is supported)"
        )));
    }
    let value_raw = iter
        .next()
        .ok_or_else(|| usage("set needs a <0-100> brightness value"))?;
    let percent = parse_percent(value_raw)?;
    if let Some(extra) = iter.next() {
        return Err(usage(&format!("unexpected argument `{extra}`")));
    }

    let target = if target_raw == "all" {
        SetTarget::All
    } else {
        SetTarget::One(target_raw.clone())
    };
    Ok(Command::Set { target, percent })
}

/// Parse and range-check a brightness percent.
fn parse_percent(raw: &str) -> Result<u8, UsageError> {
    let value: u32 = raw.parse().map_err(|_| {
        usage(&format!(
            "invalid brightness `{raw}` (want an integer 0-100)"
        ))
    })?;
    if value > 100 {
        return Err(usage(&format!("brightness {value} out of range (0-100)")));
    }
    u8::try_from(value).map_err(|_| usage("brightness out of range (0-100)"))
}

/// Build a [`UsageError`] with the shared usage text appended.
fn usage(message: &str) -> UsageError {
    UsageError(format!("{message}\n\n{USAGE}"))
}

#[cfg(test)]
mod tests {
    use super::{Command, SetTarget, parse};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_args_is_help() {
        assert_eq!(parse(&[]), Ok(Command::Help));
    }

    #[test]
    fn nullary_commands_parse() {
        assert_eq!(parse(&args(&["list"])), Ok(Command::List));
        assert_eq!(parse(&args(&["doctor"])), Ok(Command::Doctor));
        assert_eq!(parse(&args(&["version"])), Ok(Command::Version));
        assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
    }

    #[test]
    fn get_needs_exactly_one_id() {
        assert_eq!(
            parse(&args(&["get", "GSM-1"])),
            Ok(Command::Get {
                id: "GSM-1".to_owned()
            })
        );
        assert!(parse(&args(&["get"])).is_err());
        assert!(parse(&args(&["get", "a", "b"])).is_err());
    }

    #[test]
    fn set_parses_target_and_percent() {
        assert_eq!(
            parse(&args(&["set", "all", "brightness", "40"])),
            Ok(Command::Set {
                target: SetTarget::All,
                percent: 40
            })
        );
        assert_eq!(
            parse(&args(&["set", "GSM-1", "brightness", "0"])),
            Ok(Command::Set {
                target: SetTarget::One("GSM-1".to_owned()),
                percent: 0
            })
        );
    }

    #[test]
    fn set_validates_feature_and_range() {
        assert!(parse(&args(&["set", "all", "contrast", "40"])).is_err());
        assert!(parse(&args(&["set", "all", "brightness", "101"])).is_err());
        assert!(parse(&args(&["set", "all", "brightness", "-1"])).is_err());
        assert!(parse(&args(&["set", "all", "brightness", "abc"])).is_err());
        assert!(parse(&args(&["set", "all", "brightness"])).is_err());
    }

    #[test]
    fn input_parses_list_and_switch_forms() {
        assert_eq!(
            parse(&args(&["input", "GSM-1"])),
            Ok(Command::Input {
                id: "GSM-1".to_owned(),
                value: None
            })
        );
        assert_eq!(
            parse(&args(&["input", "GSM-1", "hdmi1"])),
            Ok(Command::Input {
                id: "GSM-1".to_owned(),
                value: Some("hdmi1".to_owned())
            })
        );
        // A numeric/hex code is carried through verbatim (validated later).
        assert_eq!(
            parse(&args(&["input", "GSM-1", "0x11"])),
            Ok(Command::Input {
                id: "GSM-1".to_owned(),
                value: Some("0x11".to_owned())
            })
        );
    }

    #[test]
    fn input_requires_an_id_and_rejects_extra_args() {
        assert!(parse(&args(&["input"])).is_err());
        assert!(parse(&args(&["input", "GSM-1", "hdmi1", "extra"])).is_err());
    }

    #[test]
    fn unknown_command_errors() {
        assert!(parse(&args(&["frobnicate"])).is_err());
    }
}
